//! Composition root: the only crate that builds a concrete `SystemService`,
//! `PlatformService` and - where the host has one - `DeclarativeService`,
//! wires them into `App`, and runs the terminal event loop. Nothing above
//! this line knows which implementations it is talking to, and nothing
//! above this line names `ratatui` or `crossterm`.

mod platform;

use std::io::Write;
use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{self, Event, KeyCode as TermCode, KeyEvent, KeyEventKind, KeyModifiers};
use masys_app::buffer::{BufferGates, Registry};
use masys_app::keymap::Keymap;
use masys_app::{Aftermath, App, Flow, Key, KeyCode};
use masys_domain::finding::Thresholds;
use masys_domain::service::SystemService;
use masys_render::theme::Theme;
use masys_systemd::SystemdService;
use platform::{NoScanner, select_declarative, select_packages, select_platform};
use ratatui::style::Color;
use ratatui::{DefaultTerminal, Frame};

/// How far back flapping is looked for. Matches
/// `masys_domain::finding::Thresholds`' own default window - the adapter
/// prunes restart history to this, and pruning tighter than the triage
/// window would hide real flapping.
const FLAPPING_WINDOW_MS: u64 = 3_600_000;

/// How often the machine is re-sampled.
///
/// One interval for everything, not the design's per-source cadence table
/// (procs 2s, units 5s, disk 30s, journal 5s). `App::tick` currently does
/// one sample plus one unit poll as a unit, so there is nothing here to
/// stagger yet; splitting the cadences is app-layer work that arrives with
/// the buffers that need it. Two seconds is the fastest of those cadences,
/// so this is the conservative choice: nothing refreshes slower than the
/// design intends, some things faster.
const TICK: Duration = Duration::from_secs(2);

/// How often the loop wakes while a log is open.
///
/// Four times a second: fast enough that a line appears as it is written,
/// and cheap enough to be free when nothing has - the check behind it is
/// a zero-timeout `sd_journal_wait`, not a query.
const FOLLOW_POLL: Duration = Duration::from_millis(250);

/// Startup failures surface as a plain message rather than a `MasysError`:
/// everything here runs before `ratatui::init`, so there is still a real
/// terminal to print to.
pub fn run() -> Result<(), String> {
    // The design's error table: "not a systemd host - refuse to start with
    // one clear line". Connecting to the bus is what actually establishes
    // that, so the check and the connection are the same step.
    let system = SystemdService::connect(FLAPPING_WINDOW_MS).map_err(|e| {
        format!("cannot reach systemd on the system bus - is this a systemd host? ({e})")
    })?;

    // One read, because three decisions turn on the one fact: which
    // platform adapter, whether there is a declarative service, and
    // which buffers the keymap has. Reading the file once per question
    // would be three chances for one host to answer the same question
    // three ways.
    //
    // `None` where it could not be read, kept apart from `Some("")`:
    // collapsing the two let an unreadable file reach `FallbackPlatform`
    // as an unrecognised distro, which answers `Ownership::Imperative`
    // and so promises a `systemctl enable` will stick on a host that may
    // be NixOS.
    let os_release = std::fs::read_to_string("/etc/os-release").ok();

    // Distro detection, at last with something to choose between. The
    // guard this feeds is not cosmetic: on NixOS a unit's definition
    // lives in the read-only store, so enabling one either fails or is
    // undone by the next rebuild, and the fallback adapter would have
    // reported it as a change that sticks.
    // The other two already fail safe on an unreadable file - no
    // declarative service and no package adapter is the same answer they
    // give an unrecognised distro, and both absences are honest. Only
    // `select_platform` had an answer that changed.
    let platform = select_platform(os_release.as_deref());
    let declarative = select_declarative(os_release.as_deref().unwrap_or_default());
    let packages = select_packages(os_release.as_deref().unwrap_or_default());

    // The registry follows the service, not the distro name: it is the
    // presence of an adapter that decides whether `5` exists, and
    // `App::with_declarative` asserts the two agree.
    let Config {
        keymap,
        thresholds,
        theme,
        problems,
    } = load_config(Registry::new(BufferGates {
        declarative: declarative.is_some(),
        packages: packages.is_some(),
    }));
    // The retention floor is scaled to the filesystem being scanned, and
    // the largest mounted one is the honest yardstick for a scanner that
    // will be pointed at any of them.
    let scanner = masys_scan::RayonScanner::new(largest_filesystem_bytes(&system)).ok();
    let scanner: Box<dyn masys_domain::scan::DirScanner> = match scanner {
        Some(scanner) => Box::new(scanner),
        // A pool that will not build is not a reason to refuse to start:
        // every other buffer still works, and this one simply finds
        // nothing.
        None => Box::new(NoScanner),
    };
    let mut app = App::with_declarative(
        Box::new(system) as Box<dyn SystemService>,
        platform,
        scanner,
        hostname(),
        keymap,
        declarative,
    );
    app.set_package_service(packages);
    app.set_thresholds(thresholds);
    // Reported before the terminal is taken over, because a config
    // problem the operator cannot see is a config problem they will not
    // fix - and this is the last moment there is a real terminal to
    // print to.
    for problem in &problems {
        eprintln!("masys: config: {problem}");
    }

    // One tick before the first draw, so the opening frame shows the
    // machine rather than an empty buffer that fills in two seconds later.
    tick(&mut app);

    // `ratatui::init` panics when there is no terminal - piping masys
    // into `less` printed a backtrace from inside ratatui rather than
    // saying what was wrong. This is the last point where a plain message
    // still reaches the user.
    let mut terminal = ratatui::try_init().map_err(|e| format!("masys needs a terminal ({e})"))?;
    let result = event_loop(&mut terminal, &mut app, &theme);
    // Restore before propagating: a raw-mode terminal would otherwise
    // swallow the error message about to be printed.
    ratatui::restore();
    result.map_err(|e| e.to_string())
}

/// The host's name, for the header. `/etc/hostname` rather than the
/// `hostname` binary or a libc call: it is one read, it is what systemd
/// itself uses as the static hostname, and being wrong here costs a header
/// label rather than correctness.
fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|name| name.trim().to_string())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The status header's clock, in the host's own timezone.
///
/// Delegated to masys-render rather than duplicated: the journal buffer
/// formats timestamps the same way, and two `localtime_r` call sites
/// would eventually disagree about a leap second or a zone change.
fn timestamp(now_ms: u64) -> String {
    masys_render::view::local_datetime(now_ms)
}

/// One sample-and-triage pass.
///
/// Nothing is returned, and that is the point: a failed sample reports
/// itself in the echo line. All four call sites here were
/// `let _ = tick(app)` while this handed back a `Result`, so the failure
/// that ends a pass was the one thing on the screen nobody could see.
fn tick(app: &mut App) {
    let now = now_unix_ms();
    app.tick(now, timestamp(now))
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App, theme: &Theme) -> std::io::Result<()> {
    let mut next_tick = Instant::now() + TICK;

    loop {
        // The port in both directions: the session produces a `View`, the
        // renderer consumes it. The renderer never holds an `&mut App`, so
        // drawing cannot mutate session state behind our back.
        terminal.draw(|frame: &mut Frame| {
            masys_render::render(frame, &app.view(), theme);
        })?;

        // Wait only as long as is left before the next sample, so a
        // keypress wakes the loop early but an idle session still ticks on
        // schedule. `saturating_duration_since` rather than subtraction
        // because a slow tick can push the deadline into the past.
        // While a log is open the loop wakes four times a second so that
        // new lines appear promptly. `sd_journal_wait(j, 0)` behind
        // `follow_log` is a non-blocking check that returns in
        // microseconds when nothing has arrived, which is what makes a
        // 250 ms cadence affordable - and on a host without libsystemd it
        // answers "no" for free and the tick does the work instead.
        let until_tick = next_tick.saturating_duration_since(Instant::now());
        let wait = if app.following() {
            until_tick.min(FOLLOW_POLL)
        } else {
            until_tick
        };
        if event::poll(wait)?
            && let Event::Key(event) = event::read()?
            && event.kind == KeyEventKind::Press
        {
            // Ctrl-C is the terminal's own convention rather than a
            // binding: it is not in the keymap because a user must
            // not be able to rebind their way out of being able to
            // quit.
            if event.code == TermCode::Char('c') && event.modifiers.contains(KeyModifiers::CONTROL)
            {
                return Ok(());
            }
            let key = convert(event);
            // Refresh is the one action the session cannot perform for
            // itself: it needs a clock, and `App::tick` deliberately
            // takes the time rather than reading one.
            if app.wants_refresh(key) {
                tick(app);
                next_tick = Instant::now() + TICK;
            } else {
                match app.handle_key(key) {
                    Flow::Quit => return Ok(()),
                    // An action needs the screen: `systemctl edit` spawns
                    // `$EDITOR`, and every Nix operation streams its own
                    // output for as long as it takes. Only this crate can
                    // give the terminal up, which is why the session asks
                    // rather than acts.
                    Flow::Suspend => {
                        *terminal = suspended(app)?;
                        next_tick = Instant::now() + TICK;
                    }
                    Flow::Continue => {}
                }
            }
        }
        // Anything else - a resize, a mouse event - redraws on the next
        // pass with no further work.

        // A wake that was not a keypress: the log may have moved.
        if app.following() {
            app.follow_log();
        }

        if Instant::now() >= next_tick {
            // An open process row freezes the timer. Its block is the one
            // part of masys that is read rather than watched - a command
            // line, a working directory, a table of descriptors - and it
            // is also the most expensive thing to re-read, so a tick
            // underneath it costs the most and helps the least. `g` still
            // refreshes on demand, because it comes through the branch
            // above rather than this one.
            //
            // The deadline moves either way, so a paused session idles at
            // the same rate rather than spinning on an expired one.
            if !app.auto_refresh_paused() {
                // A failed tick leaves the previous rows on screen rather
                // than ending the session: the machine having a bad
                // moment - a transient D-Bus hiccup, /proc racing us - is
                // exactly when an operator wants the tool to stay up. The
                // design's error table calls this "skip that tick, do not
                // block the loop".
                tick(app);
            }
            next_tick = Instant::now() + TICK;
        }
    }
}

/// Hands the terminal to whatever the session asked to run, waits if the
/// operator still has something to read there, then takes it back.
///
/// `ratatui::restore` leaves raw mode and the alternate screen, so the
/// command gets a normal terminal and its own scrollback. Re-initialising
/// afterwards returns a cleared alternate screen, which is why the frame
/// after this looks untouched however much the command drew - and why
/// output left on the normal screen is *gone* from the operator's point of
/// view the instant this returns. It is still in the scrollback; the
/// scrollback is behind the alternate screen masys is drawing on.
///
/// So who waits is split across the seam and neither half could decide it
/// alone: [`App::run_suspended`] knows what it ran, this crate knows there
/// is a screen to wait on. `Aftermath::Seen` is the editor's answer -
/// `$EDITOR` held the terminal until the operator quit it, and pausing
/// afterwards would demand a keypress to dismiss a screen they just left.
///
/// The order matters and only runs one way: release, run, read, restore.
/// If the restore fails there is no terminal to report into, so the error
/// goes up and the loop ends - the alternative is drawing frames nobody
/// can see.
fn suspended(app: &mut App) -> std::io::Result<DefaultTerminal> {
    ratatui::restore();
    if app.run_suspended() == Aftermath::Unseen {
        wait_for_key()?;
    }
    let terminal = ratatui::try_init()?;
    // The unit may have changed under the editor, and the frame drawn next
    // is the first thing the operator sees on coming back.
    tick(app);
    Ok(terminal)
}

/// What masys says on the normal screen before it waits there.
///
/// Nothing in the tree said "press a key" before this - `grep -rni press`
/// over `crates/*/src` finds only prose in comments - so this is built out
/// of the two conventions that do exist. `run` prefixes everything masys
/// says on a real terminal with `masys: `, which is what tells the
/// operator this last line is masys and not the eighty-fifth line of
/// `nix store diff-closures`; and a confirmation reads `{what}  y / n`, a
/// statement followed by the key that answers it, so this is a statement
/// followed by the key that answers it.
const RESUME_PROMPT: &str = "masys: any key returns to the buffer";

/// Holds the normal screen until the operator has read what is on it.
///
/// Raw mode is on for the read and off again immediately. `ratatui::restore`
/// has already left it, and a cooked terminal is line buffered, so without
/// this "any key" would mean "any key, then enter" - which is not what the
/// prompt promises and is a worse prompt than none. `ratatui::try_init`
/// enables it again a moment later for its own purposes; the two do not
/// overlap.
///
/// The prompt is printed before raw mode goes on, because raw mode drops
/// the carriage return from `\n` and the line would start under wherever
/// the command's output ended.
///
/// stdout rather than the `eprintln!` `run` uses for config problems:
/// those are diagnostics that should survive a redirect, this is the
/// terminal talking to the person in front of it, and stdout is the stream
/// ratatui and crossterm are driving either side of it.
fn wait_for_key() -> std::io::Result<()> {
    println!("\n{RESUME_PROMPT}");
    std::io::stdout().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let waited = wait_for_press();
    // Left again whatever the read did. An error path that returned with
    // raw mode still on would hand the shell back a terminal that does not
    // echo, and the error message about to be printed would be the last
    // legible thing on it.
    let restored = crossterm::terminal::disable_raw_mode();
    waited.and(restored)
}

/// Blocks until a key is pressed, discarding anything typed while the
/// command was still running.
///
/// The drain is not defensive tidiness. A key pressed while the child owned
/// stdin stays in the tty's input queue - the child never read it - and it
/// survives the switch into raw mode, so `event::read` would find one
/// waiting and the pause would dismiss itself before the prompt had been on
/// screen for a frame. Measured through a pty on this host, with `xyz` sent
/// 20 ms after `d` while `nix store diff-closures` still owned stdin: with
/// the drain the pause is still up five seconds later, and without it masys
/// is back in the alternate screen immediately - the `x` answered a prompt
/// that had not been printed yet. That window is a third of a second for a
/// diff and minutes for a rebuild, and the operations most worth pausing
/// after are the long ones.
///
/// `Press` only, matching the event loop: a release or a repeat is not
/// somebody answering the prompt.
///
/// ctrl-c is not special here. Raw mode delivers it as a key like any
/// other, so it dismisses the pause rather than raising SIGINT, and the
/// loop's own ctrl-c branch takes the next one. Driven through a pty: the
/// first returns to the buffer, the second quits. Two presses to leave is
/// the right cost - the alternative is a key that discards the output the
/// pause exists to show.
fn wait_for_press() -> std::io::Result<()> {
    while event::poll(Duration::ZERO)? {
        let _ = event::read()?;
    }
    loop {
        // A resize or a mouse event is not a key, and waiting on through
        // them costs nothing: the screen behind this is the command's own
        // output, which masys is not redrawing.
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            return Ok(());
        }
    }
}

/// crossterm's event, as masys's own `Key`.
///
/// This function is the whole reason no crate above the composition root
/// names a terminal library: everything above works in terms of
/// `masys_app::Key`, and swapping the backend would rewrite this and
/// nothing else.
fn convert(event: KeyEvent) -> Key {
    let code = match event.code {
        TermCode::Char(c) => KeyCode::Char(c),
        TermCode::Enter => KeyCode::Enter,
        TermCode::Esc => KeyCode::Esc,
        TermCode::Tab => KeyCode::Tab,
        TermCode::BackTab => KeyCode::BackTab,
        TermCode::Backspace => KeyCode::Backspace,
        TermCode::Up => KeyCode::Up,
        TermCode::Down => KeyCode::Down,
        TermCode::PageUp => KeyCode::PageUp,
        TermCode::PageDown => KeyCode::PageDown,
        TermCode::Home => KeyCode::Home,
        TermCode::End => KeyCode::End,
        _ => KeyCode::Other,
    };
    Key {
        code,
        alt: event.modifiers.contains(KeyModifiers::ALT),
        // Carried rather than dropped: without this, `ctrl-k` was
        // indistinguishable from `k` and opened the SIGTERM confirmation.
        ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
    }
}

/// The keymap and the triage thresholds, from `~/.config/masys/config.toml`.
///
/// Read here rather than in masys-app for the same reason the platform
/// adapter is chosen here: the composition root owns I/O and file
/// formats, and masys-app takes a `Keymap` it does not have to know the
/// provenance of. Swapping TOML for anything else would rewrite this
/// function and nothing above it.
///
/// A missing file is not a problem - it is the normal case. A malformed
/// one is reported and skipped rather than fatal: masys is a tool you
/// reach for when something is already wrong, and refusing to start over
/// a stray bracket in a keymap would be its own small betrayal.
///
/// Everything past "is there a file, and can it be read" is
/// `keymap_from`'s decision, not this function's - this is the only half
/// that touches the filesystem, and the only half `keymap_from`'s tests
/// do not have to.
fn load_config(registry: Registry) -> Config {
    let Some(path) = config_path() else {
        return config_from(registry, "", None);
    };
    let text = std::fs::read_to_string(&path).ok();
    config_from(registry, &path.to_string_lossy(), text.as_deref())
}

/// Everything one config file has to say, and everything masys would not
/// take from it.
///
/// A struct rather than a fourth element on a tuple, for the reason
/// `triage::Tick` is one: three of these are what the file said and the
/// fourth is what it got wrong, and at a call site their order was
/// carrying the whole meaning. Each field is what the host asked for on
/// its own axis - which keys, what counts as a problem, what it is drawn
/// in - so a caller that wants one of them names it.
struct Config {
    keymap: Keymap,
    thresholds: Thresholds,
    theme: Theme,
    /// Every setting masys refused, in the order the file offered them,
    /// for printing before the terminal is taken over. Empty is the
    /// normal case, including for a host with no config file at all.
    problems: Vec<String>,
}

/// Every decision `load_config` makes once the filesystem has answered
/// its one question: is there a config file, and what does it say.
///
/// One function for all three tables rather than one apiece, because the
/// file is parsed here: three loaders would parse it three times and
/// report a stray bracket three times, which reads as three problems
/// with one file.
///
/// `text` folds two of the five paths - no config file, and a file that
/// exists but could not be read - into the same `None`: both already
/// produced the same answer, so there is nothing here for them to
/// disagree about. `path` is only ever read back into the one problem
/// that needs to name a file - an unparseable `text` - so a caller with
/// no real path to offer may pass an empty one.
///
/// `registry` is threaded through every return rather than any of them
/// falling back to `Keymap::default()`. Which buffers a host has is a fact
/// about the host, not about its config file: a NixOS box with no
/// `~/.config/masys/config.toml` still has generations to show, and a
/// default keymap on that host would drop `5` while the service that
/// feeds it exists - the exact disagreement `App::with_declarative`
/// asserts against.
fn config_from(registry: Registry, path: &str, text: Option<&str>) -> Config {
    let untouched = |registry, problems| Config {
        keymap: Keymap::for_registry(registry),
        thresholds: Thresholds::default(),
        theme: Theme::default(),
        problems,
    };
    let Some(text) = text else {
        return untouched(registry, Vec::new());
    };
    let table: toml::Table = match text.parse() {
        Ok(table) => table,
        // All three fall back together. A file masys could not parse has
        // said nothing about any of them, and applying a third of
        // nothing would be worse than applying none of it.
        Err(e) => return untouched(registry, vec![format!("{path}: {e}")]),
    };
    let (keymap, mut problems) = keymap_from_table(registry, &table);
    let (thresholds, mut rejected) = thresholds_from_table(&table);
    problems.append(&mut rejected);
    let (theme, mut unknown) = theme_from_table(&table);
    problems.append(&mut unknown);
    Config {
        keymap,
        thresholds,
        theme,
        problems,
    }
}

/// The `[keys]` table, applied on top of the given registry.
fn keymap_from_table(registry: Registry, table: &toml::Table) -> (Keymap, Vec<String>) {
    let Some(keys) = table.get("keys").and_then(|keys| keys.as_table()) else {
        return (Keymap::for_registry(registry), Vec::new());
    };

    let mut overrides = Vec::new();
    let mut problems = Vec::new();
    for (action, value) in keys {
        match value.as_str() {
            Some(spelling) => overrides.push((action.clone(), spelling.to_string())),
            None => problems.push(format!("`keys.{action}` must be a string")),
        }
    }
    let (keymap, mut rejected) = Keymap::with_overrides_for(registry, &overrides);
    problems.append(&mut rejected);
    (keymap, problems)
}

/// The `[thresholds]` table, on top of the values the design names.
///
/// The design's Keymap and config section says thresholds live in config
/// rather than in code, and `masys_domain::finding::Thresholds` has taken
/// them as a parameter since it was written; this is the loader that had
/// never been built. Every rule keeps its default until a file says
/// otherwise, so a host with no config file evaluates exactly as before.
///
/// The keys are the struct's own field names, deliberately. A friendlier
/// spelling - `flapping_window` over `flapping_window_ms` - would be a
/// mapping table between what an operator writes and what triage reads,
/// and a mapping table is a thing that can drift. One name for one
/// number.
///
/// A value masys will not take leaves that one threshold at its default
/// and says so, the rule `[keys]` already follows: a typo should cost you
/// the customisation, not the check. Unknown keys are reported for the
/// same reason - a misspelled `disk_used_percentage` that silently did
/// nothing would leave an operator believing they had raised a threshold
/// they had not.
fn thresholds_from_table(table: &toml::Table) -> (Thresholds, Vec<String>) {
    let mut thresholds = Thresholds::default();
    let Some(configured) = table.get("thresholds").and_then(|table| table.as_table()) else {
        return (thresholds, Vec::new());
    };
    let mut problems = Vec::new();
    for (key, value) in configured {
        let applied =
            match key.as_str() {
                "flapping_restart_count" => at_least_one(value)
                    .map(|count| thresholds.flapping_restart_count = count as u32),
                "flapping_window_ms" => {
                    at_least_one(value).map(|window| thresholds.flapping_window_ms = window)
                }
                "psi_some_avg60_percent" => {
                    percent(value).map(|percent| thresholds.psi_some_avg60_percent = percent)
                }
                "psi_full_avg60_percent" => {
                    percent(value).map(|percent| thresholds.psi_full_avg60_percent = percent)
                }
                "disk_used_percent" => {
                    percent(value).map(|percent| thresholds.disk_used_percent = percent)
                }
                "inode_used_percent" => {
                    percent(value).map(|percent| thresholds.inode_used_percent = percent)
                }
                "journal_window_ms" => {
                    at_least_one(value).map(|window| thresholds.journal_window_ms = window)
                }
                "journal_rows_per_section" => at_least_one(value)
                    .map(|rows| thresholds.journal_rows_per_section = rows as u32),
                "thermal_throttled_percent" => {
                    percent(value).map(|percent| thresholds.thermal_throttled_percent = percent)
                }
                _ => Err("not a threshold masys has".to_string()),
            };
        if let Err(why) = applied {
            problems.push(format!("`thresholds.{key}`: {why}"));
        }
    }
    (thresholds, problems)
}

/// A count of something that must happen at least once to mean anything.
///
/// Zero is rejected rather than taken: a flapping window of no time, or a
/// restart count of none, would make every unit on the host a finding the
/// moment the file was saved - and the operator who typed it would be
/// looking at a screen of alarms wondering what broke.
fn at_least_one(value: &toml::Value) -> Result<u64, String> {
    match value.as_integer() {
        Some(number) if number >= 1 => Ok(number as u64),
        Some(_) => Err("must be at least 1".to_string()),
        None => Err("must be a whole number".to_string()),
    }
}

/// A percentage of something, which means nothing outside 0 to 100.
///
/// Refused rather than clamped. `disk_used_percent = 150` clamped to 100
/// is a check that never fires, reported as a check that was accepted;
/// refused, it is one line on startup and the default still applies.
///
/// An integer is a number here. `disk_used_percent = 90` is what somebody
/// writes, and TOML makes that an integer rather than a float - insisting
/// on `90.0` would be the config file having opinions about arithmetic.
fn percent(value: &toml::Value) -> Result<f32, String> {
    let number = value
        .as_float()
        .or_else(|| value.as_integer().map(|integer| integer as f64));
    match number {
        Some(number) if (0.0..=100.0).contains(&number) => Ok(number as f32),
        Some(_) => Err("must be between 0 and 100".to_string()),
        None => Err("must be a number".to_string()),
    }
}

/// The `[theme]` table, on top of the six colours masys draws with.
///
/// The keys are `Theme`'s own field names, for `[thresholds]`' reason:
/// one name for one colour, and no mapping table between what an operator
/// writes and what the renderer reads.
///
/// **Why the palette is not a preference.** Severity *is* colour here. A
/// finding's glyph carries the shape - `x` dead, `~`/`^` warning, `!`
/// urgent, `.` a plain fact - but the judgement is in the colour beside
/// it, and two of the defaults are `LightRed` and `DarkGray`, the two
/// most likely to be unreadable on a terminal somebody else configured.
/// On such a host the tool's main signal was lost with no recourse.
///
/// A colour masys cannot read leaves that one slot at its default and
/// says so, and an unknown key is reported rather than ignored - both
/// the rule `[keys]` and `[thresholds]` already follow. An absent table,
/// or one that names three colours, leaves the rest of the palette
/// exactly as it is: a config file is a host with something to say, not
/// a feature being switched on.
fn theme_from_table(table: &toml::Table) -> (Theme, Vec<String>) {
    let mut theme = Theme::default();
    let Some(configured) = table.get("theme").and_then(|table| table.as_table()) else {
        return (theme, Vec::new());
    };
    let mut problems = Vec::new();
    for (key, value) in configured {
        // The key first and the colour second, so a misspelled slot is
        // reported as the slot masys does not have rather than as
        // whatever its value turned out to be.
        let applied = match key.as_str() {
            "section_header" => color(value).map(|c| theme.section_header = c),
            "severity_dead" => color(value).map(|c| theme.severity_dead = c),
            "severity_warning" => color(value).map(|c| theme.severity_warning = c),
            "severity_urgent" => color(value).map(|c| theme.severity_urgent = c),
            "info" => color(value).map(|c| theme.info = c),
            "status_error" => color(value).map(|c| theme.status_error = c),
            _ => Err("not a colour masys draws with".to_string()),
        };
        if let Err(why) = applied {
            problems.push(format!("`theme.{key}`: {why}"));
        }
    }
    (theme, problems)
}

/// One colour, in any spelling the terminal library already accepts: a
/// name (`red`, `light-red`, `darkgray`), a 0-255 palette index, or
/// `#rrggbb`.
///
/// Its parser rather than a list of our own, which is not laziness about
/// the six names: a host whose problem is that `LightRed` is illegible
/// needs the spelling that names *its* palette entry, and a name-only
/// vocabulary would answer that host with sixteen colours it has already
/// redefined. The names remain what the README leads with, because they
/// are what respects a terminal's own configuration.
///
/// A colour masys cannot read is named back in the problem. `theme.info:
/// must be a colour` would leave an operator re-reading the line they
/// just typed.
fn color(value: &toml::Value) -> Result<Color, String> {
    let Some(name) = value.as_str() else {
        return Err("must be a string".to_string());
    };
    Color::from_str(name).map_err(|_| format!("`{name}` is not a colour masys can read"))
}

/// `$XDG_CONFIG_HOME/masys/config.toml`, falling back to `~/.config`.
fn config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("masys").join("config.toml"))
}

/// The used bytes of the largest mounted filesystem, for sizing the
/// scanner's retention floor.
///
/// One sample's worth of `statvfs`, taken before the terminal is entered.
/// `Filesystem` carries free bytes and a percentage rather than a used
/// total, so this reconstructs it: at `used_percent` of the whole,
/// `free_bytes` is the rest.
fn largest_filesystem_bytes(system: &SystemdService) -> u64 {
    system
        .sample()
        .map(|snapshot| {
            snapshot
                .filesystems
                .iter()
                .map(|fs| {
                    let free = fs.free_bytes as f64;
                    let used_fraction = (fs.used_percent as f64 / 100.0).clamp(0.0, 0.99);
                    (free * used_fraction / (1.0 - used_fraction)) as u64
                })
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "keymap_tests.rs"]
mod keymap_tests;
