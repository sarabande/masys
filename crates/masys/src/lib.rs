//! Composition root: the only crate that builds a concrete `SystemService`
//! and `PlatformService`, wires them into `App`, and runs the terminal
//! event loop. Nothing above this line knows which implementations it is
//! talking to, and nothing above this line names `ratatui` or `crossterm`.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{self, Event, KeyCode as TermCode, KeyEvent, KeyEventKind, KeyModifiers};
use masys_app::keymap::Keymap;
use masys_app::{App, Flow, Key, KeyCode};
use masys_domain::service::{PlatformService, SystemService};
use masys_platform_fallback::FallbackPlatform;
use masys_platform_nixos::{NixosPlatform, is_nixos};
use masys_render::theme::Theme;
use masys_systemd::SystemdService;
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
    let system = SystemdService::connect(FLAPPING_WINDOW_MS)
        .map_err(|e| format!("cannot reach systemd on the system bus - is this a systemd host? ({e})"))?;

    // Distro detection, at last with something to choose between. The
    // guard this feeds is not cosmetic: on NixOS a unit's definition
    // lives in the read-only store, so enabling one either fails or is
    // undone by the next rebuild, and the fallback adapter would have
    // reported it as a change that sticks.
    let platform = select_platform();

    let (keymap, problems) = load_keymap();
    // The retention floor is scaled to the filesystem being scanned, and
    // the largest mounted one is the honest yardstick for a scanner that
    // will be pointed at any of them.
    let scanner = masys_scan::RayonScanner::new(largest_filesystem_bytes(&system)).ok();
    let scanner: Box<dyn masys_domain::scan::DirScanner> = match scanner {
        Some(scanner) => Box::new(scanner),
        // A pool that will not build is not a reason to refuse to start:
        // every other view still works, and this one simply finds
        // nothing.
        None => Box::new(NoScanner),
    };
    let mut app = App::with_keymap(Box::new(system) as Box<dyn SystemService>, platform, scanner, hostname(), keymap);
    // Reported before the terminal is taken over, because a config
    // problem the operator cannot see is a config problem they will not
    // fix - and this is the last moment there is a real terminal to
    // print to.
    for problem in &problems {
        eprintln!("masys: config: {problem}");
    }

    // One tick before the first draw, so the opening frame shows the
    // machine rather than an empty buffer that fills in two seconds later.
    let _ = tick(&mut app);

    // `ratatui::init` panics when there is no terminal - piping masys
    // into `less` printed a backtrace from inside ratatui rather than
    // saying what was wrong. This is the last point where a plain message
    // still reaches the user.
    let mut terminal = ratatui::try_init().map_err(|e| format!("masys needs a terminal ({e})"))?;
    let result = event_loop(&mut terminal, &mut app);
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
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The status header's clock, in the host's own timezone.
///
/// Delegated to masys-render rather than duplicated: the journal buffer
/// formats timestamps the same way, and two `localtime_r` call sites
/// would eventually disagree about a leap second or a zone change.
fn timestamp(now_ms: u64) -> String {
    masys_render::view::local_datetime(now_ms)
}

/// One sample-and-triage pass. Errors are returned rather than swallowed;
/// the caller decides whether one failed tick should end the session.
fn tick(app: &mut App) -> Result<(), masys_domain::MasysError> {
    let now = now_unix_ms();
    app.tick(now, timestamp(now))
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    let theme = Theme::default();
    let mut next_tick = Instant::now() + TICK;

    loop {
        // The port in both directions: the session produces a `View`, the
        // renderer consumes it. The renderer never holds an `&mut App`, so
        // drawing cannot mutate session state behind our back.
        terminal.draw(|frame: &mut Frame| {
            masys_render::render(frame, &app.view(), &theme);
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
        let wait = if app.following() { until_tick.min(FOLLOW_POLL) } else { until_tick };
        if event::poll(wait)?
            && let Event::Key(event) = event::read()?
            && event.kind == KeyEventKind::Press
        {
            // Ctrl-C is the terminal's own convention rather than a
            // binding: it is not in the keymap because a user must
            // not be able to rebind their way out of being able to
            // quit.
            if event.code == TermCode::Char('c') && event.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(());
            }
            let key = convert(event);
            // Refresh is the one action the session cannot perform for
            // itself: it needs a clock, and `App::tick` deliberately
            // takes the time rather than reading one.
            if app.wants_refresh(key) {
                let _ = tick(app);
                next_tick = Instant::now() + TICK;
            } else {
                match app.handle_key(key) {
                    Flow::Quit => return Ok(()),
                    // An action needs the screen: `systemctl edit` spawns
                    // `$EDITOR`, which expects to own the terminal. Only
                    // this crate can give it up, which is why the session
                    // asks rather than acts.
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
                let _ = tick(app);
            }
            next_tick = Instant::now() + TICK;
        }
    }
}

/// Hands the terminal to whatever the session asked to run, then takes it
/// back.
///
/// `ratatui::restore` leaves raw mode and the alternate screen, so the
/// editor gets a normal terminal and its own scrollback. Re-initialising
/// afterwards returns a cleared alternate screen, which is why the frame
/// after this looks untouched however much the editor drew.
///
/// The order matters and only runs one way: release, run, restore. If the
/// restore fails there is no terminal to report into, so the error goes up
/// and the loop ends - the alternative is drawing frames nobody can see.
fn suspended(app: &mut App) -> std::io::Result<DefaultTerminal> {
    ratatui::restore();
    app.run_suspended();
    let terminal = ratatui::try_init()?;
    // The unit may have changed under the editor, and the frame drawn next
    // is the first thing the operator sees on coming back.
    let _ = tick(app);
    Ok(terminal)
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

/// The platform adapter for this host.
///
/// One `/etc/os-release` read. An unreadable or unrecognised file gets
/// the fallback, which answers every distro-specific question with "not
/// supported" - the design's point being that an unknown distro is a
/// normal working state rather than an error.
fn select_platform() -> Box<dyn PlatformService> {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    if is_nixos(&os_release) {
        return Box::new(NixosPlatform);
    }
    Box::new(FallbackPlatform)
}

/// The keymap, with `~/.config/masys/config.toml`'s `[keys]` applied.
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
fn load_keymap() -> (Keymap, Vec<String>) {
    let Some(path) = config_path() else {
        return (Keymap::default(), Vec::new());
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (Keymap::default(), Vec::new());
    };
    let table: toml::Table = match text.parse() {
        Ok(table) => table,
        Err(e) => return (Keymap::default(), vec![format!("{}: {e}", path.display())]),
    };
    let Some(keys) = table.get("keys").and_then(|keys| keys.as_table()) else {
        return (Keymap::default(), Vec::new());
    };

    let mut overrides = Vec::new();
    let mut problems = Vec::new();
    for (action, value) in keys {
        match value.as_str() {
            Some(spelling) => overrides.push((action.clone(), spelling.to_string())),
            None => problems.push(format!("`keys.{action}` must be a string")),
        }
    }
    let (keymap, mut rejected) = Keymap::with_overrides(&overrides);
    problems.append(&mut rejected);
    (keymap, problems)
}

/// `$XDG_CONFIG_HOME/masys/config.toml`, falling back to `~/.config`.
fn config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config")))?;
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

/// The scanner a host gets when the thread pool will not build.
struct NoScanner;

impl masys_domain::scan::DirScanner for NoScanner {
    fn start(&self, _root: &std::path::Path) {}
    fn poll(&self) -> masys_domain::scan::ScanProgress {
        masys_domain::scan::ScanProgress::default()
    }
    fn cancel(&self) {}
}
