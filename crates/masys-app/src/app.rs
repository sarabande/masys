//! The session: owns the two ports, the last sample's facts, and the
//! cursor and fold state for each buffer. Produces a `View`.
//!
//! The split between `tick` and `rebuild` is the load-bearing one here.
//! `tick` talks to the ports and stores *facts*; `rebuild` turns facts it
//! already has into the open buffer's rows. Switching buffers, moving the
//! cursor and folding a group all go through `rebuild` alone, so none of
//! them samples the machine - pressing `j` must not cost a D-Bus round
//! trip.
//!
//! Transients (the contextual action popups) are still a later plan; `?`
//! is bound and protected but opens nothing.

use std::collections::{HashMap, HashSet};

use masys_domain::error::MasysError;
use masys_domain::finding::{Finding, Thresholds};
use masys_domain::journal::{Entry, Priority};
use masys_domain::platform::Ownership;
use masys_domain::proc_detail::ProcDetail;
use masys_domain::rate::{self, DiskRate, NetRate, ProcRate};
use masys_domain::sample::{Disk, Filesystem, Interface, Proc, Snapshot};
use masys_domain::scan::{DirScanner, ScanProgress};
use masys_domain::service::{PlatformService, Signal, SystemService};
use masys_domain::triage;
use masys_domain::unit::{ActiveState, Unit};
use masys_view::{Header, KeyBinding, KeyGroup, ModalView, Node, Overview, SectionKind, StatusLine, View};

use crate::buffer::Buffer;
use crate::io_buffer::build_io_rows;
use crate::key::{Key, KeyCode};
use crate::keymap::{Action, Keymap, Sort, UnitVerb};
use crate::log_buffer::build_log_rows;
use crate::procs::build_proc_rows;
use crate::status::build_status_rows;
use crate::systemd_buffer::build_unit_rows;

/// An action waiting on confirmation.
#[derive(Debug, Clone)]
enum Pending {
    Kill { pid: u32, signal: Signal },
    Unit { unit: String, verb: UnitVerb },
}

impl crate::systemd_buffer::Expanded for HashMap<String, Option<masys_domain::unit::UnitDetail>> {
    fn is_open(&self, unit: &str) -> bool {
        self.contains_key(unit)
    }
    fn detail(&self, unit: &str) -> Option<masys_domain::unit::UnitDetail> {
        self.get(unit)?.clone()
    }
}

/// How much of a buffer is showing, for the buffer-wide cycle.
///
/// Two, because the outline has two levels of *section*: the headings and
/// the rows under them. A row's detail is not a third - it is opened one
/// row at a time with `enter`, and cycling it wholesale would open 366 of
/// them, each costing a read, to show what nobody asked to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Sections,
    Rows,
}

/// Whether the session continues after a keypress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
    /// The session needs the terminal back before it can go on.
    ///
    /// Returned when an action has to run a program that owns the screen -
    /// today only `systemctl edit`, which spawns `$EDITOR`. The caller
    /// releases the terminal, calls [`App::run_suspended`], and restores
    /// it. The session cannot do any of that itself: no layer above the
    /// composition root knows what a terminal is, which is the rule that
    /// keeps every crate here testable without one.
    Suspend,
}

/// What one tick learned, kept so any buffer can be rebuilt from it
/// without sampling again.
#[derive(Debug, Default)]
struct Facts {
    findings: Vec<Finding>,
    overview: Option<Overview>,
    procs: Vec<Proc>,
    rates: Vec<ProcRate>,
    units: Vec<Unit>,
    filesystems: Vec<Filesystem>,
    disks: Vec<Disk>,
    disk_rates: Vec<(String, DiskRate)>,
    /// Only ever non-empty while the Journal buffer is open - see
    /// `fetch_journal`.
    journal: Vec<Entry>,
    /// Carried from the snapshot so row builders can turn `cpu_ticks`
    /// into seconds without the snapshot itself.
    clock_ticks: u64,
    /// Likewise, so the log view can group entries by *local* calendar
    /// day. `masys-app` reads no environment, so the zone has to arrive
    /// with the sample.
    utc_offset_secs: i32,
    interfaces: Vec<Interface>,
    /// Per-interface throughput, derived the same way disk rates are.
    net_rates: Vec<(String, NetRate)>,
}

pub struct App {
    system: Box<dyn SystemService>,
    platform: Box<dyn PlatformService>,
    /// Summing a directory tree, behind a thread. The one source too slow
    /// for the tick's duty cycle - see `masys-scan`.
    scanner: Box<dyn DirScanner>,
    /// The filesystem whose contents are open, if any. At most one: a
    /// scan is expensive and two would compete for the same disk.
    scan_root: Option<std::path::PathBuf>,
    /// What the scan has found, refreshed every tick while it runs.
    scan: ScanProgress,
    /// Directories opened inside the scan, by path.
    open_dirs: HashSet<std::path::PathBuf>,
    thresholds: Thresholds,
    keymap: Keymap,
    hostname: String,
    timestamp: String,
    buffer: Buffer,
    rows: Vec<Node>,
    /// One cursor per buffer, so switching away and back does not move
    /// where you were.
    cursors: HashMap<&'static str, usize>,
    /// Collapsed group paths in the Procs buffer. Storing the *collapsed*
    /// set rather than the expanded one is what makes a cgroup that
    /// appears between two ticks show up expanded, which is the useful
    /// default and costs no extra bookkeeping.
    collapsed: HashSet<String>,
    /// Where the buffer-wide cycle has got to. Only `cycle_all` reads or
    /// writes it; the per-section cycle derives its position instead.
    level: Level,
    /// Slices whose subtree is showing, by name.
    ///
    /// Opt-in, like `expanded_units` and for a sharper version of the same
    /// reason: `system.slice` owns 130 services here, every one of them
    /// already listed under Services, so a slice that opened by default
    /// would double the buffer the moment it was drawn.
    open_slices: HashSet<String>,
    /// Units whose detail is open, and what was fetched for each.
    ///
    /// Opt-*in*, where `collapsed` above is opt-out. The inversion is the
    /// point: a cgroup appearing between two ticks should show expanded,
    /// while a unit's detail must stay shut until it is asked for - and
    /// asking is what pays for the reads below.
    expanded_units: HashMap<String, Option<masys_domain::unit::UnitDetail>>,
    /// The text each view is being filtered on, and whether one is being
    /// typed right now. Filtering live as you type is htop's behaviour and
    /// lnav's, and it is what makes the key worth pressing on a 300-row
    /// buffer.
    ///
    /// One filter *per view*, keyed like the cursors. A single shared
    /// string meant narrowing the systemd view to a unit and pressing `l`
    /// carried that unit's name into the log view - where it silently
    /// dropped every line systemd itself had logged about the unit, and
    /// where typing a search appended to it and matched nothing.
    filters: HashMap<&'static str, String>,
    typing_filter: bool,
    /// How many rows the open filter actually matched, or `None` when
    /// nothing is being filtered on.
    ///
    /// Held rather than recomputed in `view()`, which takes `&self` and
    /// would have to re-run the whole match to answer - and could then
    /// disagree with the rows already on screen.
    filter_matches: Option<usize>,
    /// What `filter` held when this typing session opened, so escape can
    /// put it back. Held rather than recomputed because the live filter
    /// is overwritten keystroke by keystroke and there is nowhere else
    /// the old text survives.
    filter_before: String,
    /// A destructive action waiting on a yes/no. Only signals reach here:
    /// nudging niceness is reversible and htop taught everyone it is free
    /// to try, so it acts immediately.
    pending: Option<Pending>,
    /// Built when the confirmation opens, because `ModalView::Confirm`
    /// borrows its prompt and `view` takes `&self`.
    confirm_prompt: String,
    /// The last action's failure, held until something replaces it - the
    /// design's rule that a refresh must not silently erase an error.
    error: Option<String>,
    /// Whether the `?` key help is open. A plain flag rather than a
    /// modal stack: there is exactly one popup today, and a stack with
    /// one slot is structure without a second case to justify it.
    help_open: bool,
    /// Why each failed unit failed, keyed by unit and roughly when.
    failure_reasons: HashMap<(String, u64), Option<String>>,
    /// Whose log the log view is currently showing.
    unit_log: Option<String>,
    /// Which way the log view reads. Newest first by default: `l` is
    /// pressed because something just happened, and the answer should not
    /// be two thousand lines down.
    log_newest_first: bool,
    /// The unit an approved `edit` is waiting to run against, held from
    /// the moment the confirmation is answered until the caller has given
    /// the terminal up. `None` at every other moment.
    suspended: Option<String>,
    /// Where `l` was pressed, so `esc` can go back to it.
    ///
    /// The view *and* the unit, not just the view: the systemd view
    /// remembers its own cursor already, but rows are rebuilt every tick
    /// and a unit can move between them, so back means "to that unit"
    /// rather than "to that row number".
    ///
    /// `q` cannot do this job - it quits from everywhere, which is what
    /// makes it predictable - so the drill-down needs a key of its own.
    came_from: Option<(Buffer, String)>,
    /// Procs: the open process rows, and what the detail read returned
    /// for each. `Some(None)` is "open, and the read failed" - which for
    /// an unprivileged masys looking at another user's process is the
    /// common case, and is why the value is an `Option` rather than the
    /// row simply not opening.
    ///
    /// Keyed by pid rather than by name: several processes share a name,
    /// and the one you opened is a specific one.
    expanded_procs: HashMap<u32, Option<ProcDetail>>,
    /// Procs: how groups and their members are ordered, and which way.
    sort: Sort,
    sort_descending: bool,
    /// The footer's offer for the open buffer. Held rather than built in
    /// `view()` because `View` borrows it, and rebuilt whenever the
    /// buffer changes - which is the only thing that changes it.
    hints: Vec<KeyBinding>,
    actions: Vec<KeyBinding>,
    facts: Facts,
    /// The last time `tick` was given, so a journal fetch triggered by a
    /// keypress has a lookback to use. A keypress has no clock of its
    /// own, and inventing one here is exactly what `tick` taking `now_ms`
    /// exists to avoid.
    last_now_ms: u64,
    /// The previous tick's sample, kept solely so `masys_domain::rate` has
    /// two snapshots to work with. CPU% is a derivative and does not exist
    /// before the second tick - see the design's realtime model.
    previous: Option<Snapshot>,
}

impl App {
    /// Uses the built-in default thresholds and keymap - real usage loads
    /// the user's config file instead, once masys-app owns config loading.
    pub fn new(system: Box<dyn SystemService>, platform: Box<dyn PlatformService>, scanner: Box<dyn DirScanner>, hostname: String) -> Self {
        Self::with_keymap(system, platform, scanner, hostname, Keymap::default())
    }

    pub fn with_keymap(
        system: Box<dyn SystemService>,
        platform: Box<dyn PlatformService>,
        scanner: Box<dyn DirScanner>,
        hostname: String,
        keymap: Keymap,
    ) -> Self {
        App {
            system,
            platform,
            scanner,
            scan_root: None,
            scan: ScanProgress::default(),
            open_dirs: HashSet::new(),
            thresholds: Thresholds::default(),
            keymap,
            hostname,
            timestamp: String::new(),
            buffer: Buffer::Status,
            rows: Vec::new(),
            cursors: HashMap::new(),
            // The mockup draws the kernel bucket folded, and it is by far
            // the largest group on a normal host - 145 of 353 processes here.
            collapsed: HashSet::from([crate::procs::KERNEL_GROUP.to_string()]),
            level: Level::Rows,
            open_slices: HashSet::new(),
            expanded_units: HashMap::new(),
            filters: HashMap::new(),
            typing_filter: false,
            filter_matches: None,
            filter_before: String::new(),
            pending: None,
            confirm_prompt: String::new(),
            error: None,
            help_open: false,
            failure_reasons: HashMap::new(),
            unit_log: None,
            log_newest_first: true,
            expanded_procs: HashMap::new(),
            came_from: None,
            suspended: None,
            sort: Sort::Cpu,
            sort_descending: Sort::Cpu.default_descending(),
            hints: Vec::new(),
            actions: Vec::new(),
            last_now_ms: 0,
            facts: Facts::default(),
            previous: None,
        }
    }

    pub fn buffer(&self) -> Buffer {
        self.buffer
    }

    /// Whether the caller should skip its timed tick.
    ///
    /// An open process row is being *read*, and its block is the one part
    /// of masys that both moves under the eye - a command line, a
    /// directory, an fd table - and costs the most to re-read: chrome
    /// holds 928 descriptors, each a `readlink`, and the socket tables on
    /// top of that. Freezing while it is open is what makes it readable
    /// and what stops the tick paying for a table nobody is watching
    /// change.
    ///
    /// Advisory, not enforced inside `tick`: `g` must still refresh on
    /// demand, and the caller owns the clock. Folding the row resumes it,
    /// with no state to remember either way - the pause *is* the open
    /// row.
    pub fn auto_refresh_paused(&self) -> bool {
        !self.expanded_procs.is_empty()
    }

    /// Swaps the system port. Exists for tests that need a second sample
    /// with different contents; nothing in the binary calls it.
    pub fn replace_system(&mut self, system: Box<dyn SystemService>) {
        self.system = system;
    }

    /// Samples the machine, runs the v1 triage rules, and rebuilds the
    /// open buffer. `now_ms` and `timestamp` come from the caller rather
    /// than being read here, so `tick` stays as testable as
    /// `masys_domain::triage::evaluate` already is - no hidden clock read
    /// to fake around.
    pub fn tick(&mut self, now_ms: u64, timestamp: String) -> Result<(), MasysError> {
        let snapshot = self.system.sample()?;
        let units = self.system.units()?;
        let boot_pressure = self.platform.boot_pressure()?;
        let pending_reboot = self.platform.pending_reboot()?;

        let mut findings = triage::evaluate(&snapshot, &units, boot_pressure.as_ref(), &self.thresholds, now_ms);
        self.explain_failures(&mut findings, now_ms);

        let overview = Overview {
            machine: snapshot.machine.clone(),
            system_state: snapshot.system_state,
            unit_count: units.len() as u32,
            load_1: snapshot.load.map(|l| l.one),
            load_5: snapshot.load.map(|l| l.five),
            load_15: snapshot.load.map(|l| l.fifteen),
            uptime_secs: snapshot.uptime_secs,
            mem_used_bytes: snapshot.memory.map(|m| m.used_bytes),
            mem_total_bytes: snapshot.memory.map(|m| m.total_bytes),
            zram_percent: snapshot.memory.and_then(|m| m.zram_percent),
            swap_free_bytes: snapshot.memory.map(|m| m.swap_free_bytes),
            clock_synced: snapshot.clock_synced,
            smart_ok: None,
            pending_reboot,
        };

        // Empty on the first tick, which is what hides the Top CPU section
        // until a rate can actually be computed.
        let (rates, disk_rates, net_rates) = match &self.previous {
            Some(previous) => (
                rate::derive(previous, &snapshot, snapshot.clock_ticks_per_sec),
                rate::derive_disks(previous, &snapshot),
                rate::derive_nets(previous, &snapshot),
            ),
            None => (Vec::new(), Vec::new(), Vec::new()),
        };

        self.last_now_ms = now_ms;
        self.facts = Facts {
            findings,
            overview: Some(overview),
            procs: snapshot.procs.clone(),
            rates,
            units,
            filesystems: snapshot.filesystems.clone(),
            disks: snapshot.disks.clone(),
            disk_rates,
            // Preserved across ticks; refreshed below only when it is
            // being looked at.
            journal: std::mem::take(&mut self.facts.journal),
            clock_ticks: snapshot.clock_ticks_per_sec,
            utc_offset_secs: snapshot.utc_offset_secs,
            interfaces: snapshot.interfaces.clone(),
            net_rates,
        };
        // Drained, never waited on: the walk is on its own threads and
        // this is the seam where what it has found so far crosses back.
        if self.scan_root.is_some() {
            self.scan = self.scanner.poll();
        }
        if self.buffer == Buffer::Systemd {
            self.refresh_expansions();
        }
        if self.buffer == Buffer::Log
            && let Some(unit) = self.unit_log.clone()
        {
            self.refresh_unit_log(&unit);
        }
        self.timestamp = timestamp;
        self.previous = Some(snapshot);
        self.rebuild();
        Ok(())
    }

    /// Turns the facts already held into the open buffer's rows, then puts
    /// the cursor somewhere legal. Never samples.
    fn rebuild(&mut self) {
        // A search reaches every row, folded or not. The filter runs over
        // the rows, and the rows are built after folding - so a collapsed
        // section used to contribute nothing to search, and `/` could not
        // find a unit you had just folded out of sight. Searching is how
        // you find a unit you cannot see, and a fold is exactly the state
        // in which you cannot see it.
        //
        // The fold state itself is untouched, so clearing the filter puts
        // the sections back rather than leaving a search having silently
        // unfolded the buffer.
        let folded = HashSet::new();
        let collapsed = if self.filter().is_empty() { &self.collapsed } else { &folded };
        self.rows = match self.buffer {
            Buffer::Status => match self.facts.overview.clone() {
                Some(overview) => build_status_rows(&self.facts.findings, overview),
                // Before the first tick there is nothing to show. The
                // binary ticks once before its first draw, so this is the
                // state only a test can observe.
                None => Vec::new(),
            },
            Buffer::Procs => build_proc_rows(
                &self.facts.procs,
                &self.facts.rates,
                collapsed,
                self.sort,
                self.sort_descending,
                self.facts.clock_ticks,
                self.last_now_ms,
                &self.expanded_procs,
            ),
            Buffer::Systemd => build_unit_rows(&self.facts.units, collapsed, &self.expanded_units, &self.open_slices, self.last_now_ms),
            Buffer::Io => build_io_rows(
                &self.facts.filesystems,
                &self.facts.disks,
                &self.facts.disk_rates,
                &self.facts.interfaces,
                &self.facts.net_rates,
                &self.scan,
                &self.open_dirs,
            ),
            Buffer::Log => {
                build_log_rows(&self.facts.journal, self.unit_log.as_deref(), self.facts.utc_offset_secs, self.log_newest_first, collapsed)
            }
        };
        self.filter_matches = if self.filter().is_empty() {
            // No query, no count. A blank filter line that reported the
            // whole buffer would be answering a question nobody asked.
            None
        } else {
            let needle = self.filter().to_string();
            let (rows, matched) = crate::filter::apply(std::mem::take(&mut self.rows), &needle);
            self.rows = rows;
            Some(matched)
        };
        self.hints = self.keymap.hints(self.buffer);
        let clamped = self.legal_cursor(self.cursor());
        self.set_cursor(clamped);
        // After the clamp, because what is dimmed depends on the row the
        // cursor ends up on rather than the one it started from.
        self.refresh_actions();
    }

    fn filter(&self) -> &str {
        self.filters.get(self.buffer.title()).map(String::as_str).unwrap_or("")
    }

    fn filter_mut(&mut self) -> &mut String {
        self.filters.entry(self.buffer.title()).or_default()
    }

    fn cursor(&self) -> usize {
        self.cursors.get(self.buffer.title()).copied().unwrap_or(0)
    }

    fn set_cursor(&mut self, index: usize) {
        self.cursors.insert(self.buffer.title(), index);
    }

    /// Whether the cursor may rest on `index`.
    ///
    /// Spacers are structure. A `UnitDetail` is content, but it belongs to
    /// the unit above it: every verb here acts on the row under the
    /// cursor, and a cursor on a property line would have nothing to act
    /// on.
    fn selectable(&self, index: usize) -> bool {
        !matches!(self.rows.get(index), Some(Node::Spacer) | Some(Node::UnitDetail { .. }) | Some(Node::ProcDetail { .. }) | None)
    }

    /// The nearest selectable row at or after `wanted`, falling back to
    /// searching backwards. Rows are rebuilt every tick and a buffer can
    /// shrink between them, so a cursor that was legal a moment ago may
    /// now be past the end - and a renderer handed an out-of-range index
    /// would panic.
    fn legal_cursor(&self, wanted: usize) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let start = wanted.min(self.rows.len() - 1);
        (start..self.rows.len()).find(|i| self.selectable(*i)).or_else(|| (0..=start).rev().find(|i| self.selectable(*i))).unwrap_or(0)
    }

    /// Moves to the next or previous section heading.
    ///
    /// A buffer's sections are what its rows are *about* - failed units,
    /// timers, one cgroup - so jumping between them is how you skim a
    /// screen of two hundred rows. magit binds `n`/`p` for this and wagit
    /// followed; masys does too.
    ///
    /// A section here is any heading row: a `SectionHeader`, or a
    /// `ProcGroup`, which is the Procs buffer's heading in everything but
    /// name. Running off either end stays put rather than wrapping, the
    /// same rule ordinary movement follows.
    fn jump_section(&mut self, delta: isize) {
        let is_heading = |node: &Node| matches!(node, Node::SectionHeader { .. } | Node::ProcGroup { .. });
        let mut index = self.cursor() as isize;
        loop {
            index += delta;
            if index < 0 || index as usize >= self.rows.len() {
                return;
            }
            if self.rows.get(index as usize).is_some_and(is_heading) {
                self.set_cursor(index as usize);
                return;
            }
        }
    }

    /// Steps the cursor by one selectable row in `delta`'s direction,
    /// stopping at either end rather than wrapping. Wrapping would make
    /// holding `j` silently loop a buffer that does not fit the screen.
    fn step(&mut self, delta: isize) {
        let mut index = self.cursor() as isize;
        loop {
            let next = index + delta;
            if next < 0 || next as usize >= self.rows.len() {
                return;
            }
            index = next;
            if self.selectable(index as usize) {
                self.set_cursor(index as usize);
                return;
            }
        }
    }

    pub fn handle_key(&mut self, key: Key) -> Flow {
        // The help is a reference card, not a mode with its own verbs:
        // any key dismisses it. Making the reader learn a second set of
        // keys to leave the list of keys would be its own small joke.
        if self.help_open {
            self.help_open = false;
            return Flow::Continue;
        }
        if self.typing_filter {
            return self.type_filter(key);
        }
        // Escape peels one layer at a time, outermost first: the filter
        // being typed (handled above), then a filter in effect, then a
        // drill-down. Committing a filter with Enter used to leave no way
        // out of it but `/`, backspace held down, Enter - and a filter you
        // cannot lift is one that quietly hides rows for the rest of the
        // session.
        //
        // Checked before the keymap so it needs no binding and cannot be
        // rebound away from: there has to be one key that always means
        // "back out of this".
        if key.code == KeyCode::Esc && self.pending.is_none() && self.escape() {
            return Flow::Continue;
        }
        if self.pending.is_some() {
            return self.answer_confirm(key);
        }

        match self.keymap.resolve(self.buffer, key) {
            Some(Action::MoveDown) => self.step(1),
            Some(Action::MoveUp) => self.step(-1),
            // A page is a fixed jump rather than the rendered height: the
            // renderer measures the height and reports it as `Metrics`,
            // but nothing observes that yet.
            Some(Action::PageDown) => (0..PAGE).for_each(|_| self.step(1)),
            Some(Action::PageUp) => (0..PAGE).for_each(|_| self.step(-1)),
            Some(Action::NextSection) => self.jump_section(1),
            Some(Action::PrevSection) => self.jump_section(-1),
            Some(Action::MoveTop) => {
                let top = self.legal_cursor(0);
                self.set_cursor(top);
            }
            Some(Action::MoveBottom) => {
                let bottom = self.legal_cursor(self.rows.len().saturating_sub(1));
                self.set_cursor(bottom);
            }
            Some(Action::Cycle) => self.cycle_section(),
            Some(Action::CycleAll) => self.cycle_all(),
            Some(Action::ToggleDetail) => self.toggle_detail(),
            // From every view, not just Status: a digit reaches any view
            // directly, so there is nothing left for a bury to do.
            Some(Action::Quit) => return Flow::Quit,
            Some(Action::Open(buffer)) => self.open(buffer),
            Some(Action::Help) => self.help_open = true,
            Some(Action::Filter) => {
                self.filter_before = self.filter().to_string();
                self.typing_filter = true;
            }
            Some(Action::Kill(signal)) => self.ask_kill(signal),
            Some(Action::Nice(delta)) => self.nudge_nice(delta),
            Some(Action::Unit(verb)) => self.ask_unit(verb),
            Some(Action::Logs) => self.show_logs(),
            Some(Action::ToggleOrder) => self.toggle_log_order(),
            Some(Action::GoToUnit) => self.go_to_unit(),
            Some(Action::SortBy(sort)) => {
                // The same key again reverses; a different one switches
                // column and takes that column's natural direction, so
                // `n` always starts at a-z rather than inheriting
                // whichever way the last column happened to be pointing.
                self.sort_descending = if self.sort == sort { !self.sort_descending } else { sort.default_descending() };
                self.sort = sort;
                self.rebuild();
            }
            // Refresh is the caller's to perform: it owns the clock, the
            // same reason `tick` takes `now_ms` rather than reading one.
            Some(Action::Refresh) | None => {}
        }
        self.refresh_actions();
        Flow::Continue
    }

    /// Shows or hides the section under the cursor - magit's `TAB`.
    ///
    /// Two states, not three, because this view has one level of section:
    /// a type and the units in it. A row's detail is not a deeper level of
    /// the outline, it is the row's own content, and `enter` owns it.
    ///
    /// A row that is not a section falls through to opening itself, which
    /// is what `TAB` does at a leaf in magit too.
    fn cycle_section(&mut self) {
        // A slice owns a subtree, so `tab` folds it - the same act as
        // folding a section, one level further in. Every other row has no
        // subtree, and `tab` there opens the row itself.
        let subtree = match self.rows.get(self.cursor()) {
            Some(Node::Unit { unit, .. }) if unit.kind == masys_domain::unit::UnitKind::Slice || !unit.triggers.is_empty() => {
                Some(unit.name.clone())
            }
            // A timer's row is its schedule, but it opens like any other.
            Some(Node::Timer { name, .. }) => Some(name.clone()),
            _ => None,
        };
        if let Some(name) = subtree {
            if !self.open_slices.remove(&name) {
                self.open_slices.insert(name);
            }
            return self.rebuild();
        }
        let name = match self.rows.get(self.cursor()) {
            Some(Node::ProcGroup { name, .. }) => name.clone(),
            // Units in the systemd view, days in the log: both are
            // sections whose rows fold away under their heading.
            Some(Node::SectionHeader { title, kind: SectionKind::Units | SectionKind::Journal, .. }) => title.clone(),
            _ => return self.toggle_detail(),
        };
        // Shutting a section shuts what was open inside it: those rows are
        // about to stop existing, and a detail left open would spring back
        // the next time the section did.
        if self.collapsed.remove(&name) {
        } else {
            for unit in self.section_members(self.cursor()) {
                self.expanded_units.remove(&unit);
            }
            self.collapsed.insert(name);
        }
        self.rebuild();
    }

    /// Cycles every section together - magit's `S-TAB`.
    ///
    /// Deliberately ignores the cursor: a global cycle that depended on
    /// where you were standing would be a second per-section cycle with a
    /// harder key. The level is stored rather than derived, because
    /// "everything" has no single state to read back once a row has been
    /// opened by hand.
    fn cycle_all(&mut self) {
        self.level = match self.level {
            Level::Rows => Level::Sections,
            Level::Sections => Level::Rows,
        };
        match self.level {
            Level::Sections => {
                self.collapsed = self.group_names();
                self.open_slices.clear();
                // What was open is inside sections that are about to
                // close, so it shuts with them rather than springing back
                // when they reopen.
                self.expanded_units.clear();
            }
            Level::Rows => self.collapsed.clear(),
        }
        self.rebuild();
    }

    /// The units listed under the section header at `index`, in the rows
    /// as they currently stand. Empty for a collapsed section, which is
    /// correct: the only move available there is to show it.
    fn section_members(&self, index: usize) -> Vec<String> {
        self.rows
            .iter()
            .skip(index + 1)
            .take_while(|row| !matches!(row, Node::SectionHeader { .. } | Node::ProcGroup { .. }))
            .filter_map(|row| match row {
                Node::Unit { unit, .. } => Some(unit.name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Opens or closes the detail of the row under the cursor.
    ///
    /// Distinct from `toggle_fold`, which collapses a *group*. This opens
    /// a *row*, and keeping the two apart is what stops the key that hides
    /// a 267-row section from being the same key that looks inside one of
    /// its rows. A row with no detail does nothing rather than reporting
    /// that it has none.
    fn toggle_detail(&mut self) {
        match self.rows.get(self.cursor()) {
            Some(Node::Unit { unit, .. }) => {
                let name = unit.name.clone();
                if self.expanded_units.remove(&name).is_none() {
                    self.open_unit(&name);
                }
            }
            Some(Node::Proc { proc, .. }) => {
                let pid = proc.pid;
                if self.expanded_procs.remove(&pid).is_none() {
                    self.open_proc(pid);
                }
            }
            // Opening a filesystem is the question "what is using this",
            // and it is the only thing that starts a scan: masys never
            // walks a filesystem nobody asked about.
            Some(Node::Filesystem { filesystem, .. }) => {
                let mount = std::path::PathBuf::from(&filesystem.mount_point);
                if self.scan_root.as_deref() == Some(mount.as_path()) {
                    self.close_scan();
                } else {
                    self.scanner.start(&mount);
                    self.scan = ScanProgress::default();
                    self.open_dirs.clear();
                    self.scan_root = Some(mount);
                }
            }
            // A directory opens from the tree the scan already built, so
            // drilling in costs nothing further - the same reason `dust`
            // can show any depth after one pass.
            Some(Node::DirEntry { path, .. }) => {
                let path = path.clone();
                if !self.open_dirs.remove(&path) {
                    self.open_dirs.insert(path);
                }
            }
            _ => return,
        }
        self.rebuild();
    }

    /// Stops the scan and forgets what it found.
    ///
    /// A scan nobody is looking at is CPU nobody asked to spend, and on a
    /// pool that is several cores of it.
    fn close_scan(&mut self) {
        self.scanner.cancel();
        self.scan = ScanProgress::default();
        self.scan_root = None;
        self.open_dirs.clear();
    }

    /// Opens one process, reading the fields the per-tick sweep does not
    /// carry.
    ///
    /// One read, on the keypress, for the same reason `open_unit` is: the
    /// sweep already touches every process on the host every tick, and
    /// these reads are per-descriptor on top of that. A failed read
    /// leaves the row open with what `Proc` alone carries - most of this
    /// is unreadable for another user's process, and refusing to open the
    /// row would make an unprivileged masys look broken rather than
    /// unprivileged.
    fn open_proc(&mut self, pid: u32) {
        let detail = self.system.proc_detail(pid).ok();
        self.expanded_procs.insert(pid, detail);
    }

    /// Opens one unit, reading the properties that fill its detail row.
    ///
    /// One read, and it is why this is a keypress rather than part of the
    /// poll. No log: `l` opens that in full, scrollable and searchable,
    /// and duplicating a handful of its lines here meant two places to
    /// read the same thing and neither of them the good one. A failed read
    /// leaves the row open with what `Unit` alone carries - a unit is
    /// still worth looking at when the bus is busy.
    fn open_unit(&mut self, unit: &str) {
        let detail = self.system.unit_detail(unit).ok();
        self.expanded_units.insert(unit.to_string(), detail);
    }

    /// Re-reads every open unit. Bounded by how many are open, which is
    /// normally one - and is the reason the read is not in `units()`.
    fn refresh_expansions(&mut self) {
        let open: Vec<String> = self.expanded_units.keys().cloned().collect();
        for unit in open {
            self.open_unit(&unit);
        }
        // A process that exited while its row was open is dropped rather
        // than re-read: `proc_detail` fails for a dead pid, and holding
        // the row open on the last thing it said would let a stale
        // command line outlive the process it described.
        let open: Vec<u32> = self.expanded_procs.keys().copied().collect();
        for pid in open {
            match self.system.proc_detail(pid) {
                Ok(detail) => {
                    self.expanded_procs.insert(pid, Some(detail));
                }
                Err(_) if !self.facts.procs.iter().any(|proc| proc.pid == pid) => {
                    self.expanded_procs.remove(&pid);
                }
                // Still in the sample, just unreadable - which is the
                // ordinary state of another user's process.
                Err(_) => {}
            }
        }
    }

    /// Backs out of one layer, and reports whether there was one.
    ///
    /// A filter in effect goes before a drill-down, so leaving a view
    /// never leaves a filter set on it - coming back to a view you had
    /// narrowed, and finding rows still missing, is the confusing half of
    /// per-view filters.
    fn escape(&mut self) -> bool {
        if !self.filter().is_empty() {
            self.filter_mut().clear();
            self.rebuild();
            return true;
        }
        self.go_back()
    }

    /// Returns to where `l` was pressed, cursor on the unit it was pressed
    /// on. Reports whether there was anywhere to go, so a stray `esc`
    /// outside a drill-down does nothing rather than jumping somewhere
    /// arbitrary.
    fn go_back(&mut self) -> bool {
        let Some((buffer, unit)) = self.came_from.take() else { return false };
        self.buffer = buffer;
        self.rebuild();
        // An empty name is what `u` leaves behind: a process row has no
        // unit of its own, and the Procs buffer's own cursor is already
        // sitting on the row that was jumped from.
        if unit.is_empty() {
            self.refresh_actions();
            return true;
        }
        // By name rather than by index: the rows were rebuilt while the
        // log was open, and the unit may have moved.
        if let Some(index) = self.rows.iter().position(|row| match row {
            Node::Unit { unit: candidate, .. } => candidate.name == unit,
            Node::Timer { name, .. } => *name == unit,
            _ => false,
        }) {
            self.set_cursor(index);
            self.refresh_actions();
        }
        true
    }

    /// The unit that owns the row under the cursor, if any.
    ///
    /// Walks the cgroup path from the leaf upward and takes the first
    /// segment that names a unit the poll actually saw. Leaf-only would
    /// miss a process in a delegated sub-cgroup - a container runtime or
    /// a service that manages its own tree puts processes several levels
    /// below the unit that owns them, and `/system.slice/foo.service/bar`
    /// has a leaf of `bar`, which names nothing.
    ///
    /// Checked against the poll rather than against a list of suffixes,
    /// so a jump only ever offers a unit there is somewhere to jump to.
    fn unit_of_selected_row(&self) -> Option<String> {
        let cgroup = match self.rows.get(self.cursor())? {
            Node::Proc { proc, .. } => proc.cgroup.clone()?,
            // A group row *is* a cgroup, so it needs no process to ask.
            Node::ProcGroup { name, .. } => name.clone(),
            _ => return None,
        };
        cgroup.split('/').rev().find(|segment| self.facts.units.iter().any(|unit| unit.name == *segment)).map(str::to_string)
    }

    /// Opens the systemd view at the unit that owns the row under the
    /// cursor.
    ///
    /// The association already existed in the data - the Procs buffer
    /// groups by cgroup, and on a systemd host a cgroup *is* a unit - it
    /// simply was not reachable without reading the path off the screen
    /// and finding the row by hand.
    ///
    /// Silent when there is no unit: the kernel bucket and a process in
    /// no cgroup have none, and an error line saying so on every stray
    /// keypress would be noise on the one view where `u` is bound.
    fn go_to_unit(&mut self) {
        let Some(unit) = self.unit_of_selected_row() else { return };
        // Back to the row that was under the cursor, not to the unit -
        // `came_from` names a unit because that is what `l` needs, and a
        // process row has no unit name of its own. The Procs buffer keeps
        // its own cursor, so returning to the buffer returns to the row.
        self.came_from = Some((self.buffer, String::new()));
        self.buffer = Buffer::Systemd;

        // Reveal rather than merely navigate. A jump that lands on
        // nothing because the target was folded away, or filtered out, is
        // worse than no jump: it moves you somewhere and shows you
        // nothing, and gives no hint which of the two hid it.
        self.filters.remove(Buffer::Systemd.title());
        if let Some(kind) = self.facts.units.iter().find(|u| u.name == unit).map(|u| u.kind) {
            self.collapsed.remove(kind.plural());
        }
        self.rebuild();

        // By name rather than by index, for the same reason `go_back`
        // does it: the rows were just rebuilt and nothing guarantees the
        // unit is where it was.
        if let Some(index) = self.rows.iter().position(|row| matches!(row, Node::Unit { unit: found, .. } if found.name == unit)) {
            self.set_cursor(index);
        }
        self.refresh_actions();
    }

    /// Switches to `buffer` and rebuilds.
    ///
    /// Nothing is fetched on arrival. The log view is the only one with
    /// content it does not already hold, and it is reached by naming a
    /// unit with `l` rather than by jumping to it - so jumping to it shows
    /// whichever unit was last opened, and says so when that is none.
    fn open(&mut self, buffer: Buffer) {
        self.buffer = buffer;
        self.rebuild();
    }

    /// Fills in *why* each failed unit failed, from its own journal.
    ///
    /// The status buffer could say a unit had failed and with what exit
    /// code, but not what went wrong - and "exit 6" against "curl: (6)
    /// Could not resolve host" is the difference between knowing
    /// something broke and knowing what to do about it.
    ///
    /// Answers are cached against roughly when the unit failed, so a unit
    /// that has been failed for hours costs one query rather than one
    /// every two seconds. A unit that fails again gets a newer age and so
    /// a fresh answer.
    fn explain_failures(&mut self, findings: &mut [Finding], now_ms: u64) {
        for finding in findings.iter_mut() {
            let Finding::FailedUnit { unit, since_ms, reason, .. } = finding else {
                continue;
            };
            // `since_ms` on a finding is an *age*, not a timestamp - it
            // advances every tick - so keying on it directly gave a fresh
            // key every two seconds and the cache never hit once. Undoing
            // the subtraction `triage::failed_units` did recovers the
            // instant the unit failed, which is the thing that stays put
            // while a unit stays broken and moves when it breaks again.
            let key = (unit.clone(), now_ms.saturating_sub(*since_ms) / 1000);
            if let Some(cached) = self.failure_reasons.get(&key) {
                *reason = cached.clone();
                continue;
            }
            let found = self.system.unit_journal(unit, FAILURE_LOG_LINES).ok().and_then(|entries| last_words(&entries, unit));
            self.failure_reasons.insert(key, found.clone());
            *reason = found;
        }
        // A host that churns through failing units should not accumulate
        // every answer it has ever given.
        if self.failure_reasons.len() > 64 {
            self.failure_reasons.clear();
        }
    }

    /// Re-reads one unit's log, keeping the buffer scoped to it.
    fn refresh_unit_log(&mut self, unit: &str) {
        if let Ok(entries) = self.system.unit_journal(unit, LOG_VIEW_LINES) {
            self.facts.journal = entries;
        }
    }

    /// Types into the filter, applying it on every keystroke.
    ///
    /// Escape restores what was there before rather than clearing it: an
    /// accidental `/` should not throw away a filter that was already
    /// narrowing a 300-row buffer.
    fn type_filter(&mut self, key: Key) -> Flow {
        match key.code {
            KeyCode::Esc => {
                self.typing_filter = false;
                let restored = std::mem::take(&mut self.filter_before);
                *self.filter_mut() = restored;
            }
            KeyCode::Enter => self.typing_filter = false,
            KeyCode::Char(c) => self.filter_mut().push(c),
            KeyCode::Backspace => {
                self.filter_mut().pop();
            }
            // Every other key is ignored rather than guessed at: an
            // unmodelled key arrives as `Other`, and treating that as
            // "delete one" would make arrow keys eat the filter.
            _ => {}
        }
        self.rebuild();
        Flow::Continue
    }

    /// Answers a pending confirmation. `y` confirms; anything else does
    /// not - a destructive action should need the one key that means yes,
    /// not merely a key that is not `n`.
    fn answer_confirm(&mut self, key: Key) -> Flow {
        let pending = self.pending.take();
        if key.code == KeyCode::Char('y') {
            match pending {
                Some(Pending::Kill { pid, signal }) => {
                    let result = self.system.kill(pid, signal);
                    self.report(result);
                }
                // Edit is the one verb not run here: it needs the
                // terminal, which the session does not own. It is recorded
                // and the caller is asked for it.
                Some(Pending::Unit { unit, verb: UnitVerb::Edit }) => {
                    self.suspended = Some(unit);
                    self.rebuild();
                    return Flow::Suspend;
                }
                Some(Pending::Unit { unit, verb }) => {
                    let result = match verb {
                        UnitVerb::Start => self.system.start(&unit),
                        UnitVerb::Stop => self.system.stop(&unit),
                        UnitVerb::Restart => self.system.restart(&unit),
                        UnitVerb::TryRestart => self.system.try_restart(&unit),
                        UnitVerb::Reload => self.system.reload(&unit),
                        UnitVerb::Enable => self.system.enable(&unit),
                        UnitVerb::Disable => self.system.disable(&unit),
                        UnitVerb::Mask => self.system.mask(&unit),
                        UnitVerb::Unmask => self.system.unmask(&unit),
                        UnitVerb::ResetFailed => self.system.reset_failed(&unit),
                        UnitVerb::Edit => unreachable!("handled above"),
                    };
                    self.report(result);
                }
                None => {}
            }
        }
        self.rebuild();
        Flow::Continue
    }

    /// Runs whatever `Flow::Suspend` was asked for, with the terminal in
    /// the caller's hands.
    ///
    /// Called by the composition root between releasing the terminal and
    /// restoring it. Does nothing if nothing is waiting, so a caller that
    /// calls it unconditionally is not wrong - which is what makes the
    /// contract hard to get subtly wrong.
    ///
    /// A failure is held on the status line like any other action's, and
    /// is worth holding: `systemctl edit` refuses outright where the unit
    /// directory is read-only, and that refusal is the answer.
    pub fn run_suspended(&mut self) {
        let Some(unit) = self.suspended.take() else { return };
        let result = self.system.edit(&unit);
        self.report(result);
        self.rebuild();
    }

    /// The process under the cursor, if the cursor is on one.
    fn selected_proc(&self) -> Option<(u32, String, i32)> {
        match self.rows.get(self.cursor()) {
            Some(Node::Proc { proc, .. }) => Some((proc.pid, proc.comm.clone(), proc.nice)),
            _ => None,
        }
    }

    fn ask_kill(&mut self, signal: Signal) {
        let Some((pid, name, _)) = self.selected_proc() else { return };
        // The signal is named in the prompt rather than implied by which
        // key was pressed: SIGKILL cannot be caught, and the difference
        // is the whole reason there are two bindings.
        let verb = if signal == Signal::Kill { "SIGKILL" } else { "SIGTERM" };
        self.confirm_prompt = format!("{verb} {name} (pid {pid})?  y / n");
        self.pending = Some(Pending::Kill { pid, signal });
    }

    /// Whether the unit under the cursor has failed.
    fn selected_unit_failed(&self) -> bool {
        matches!(self.rows.get(self.cursor()), Some(Node::Unit { unit, .. }) if unit.active_state == ActiveState::Failed)
    }

    /// The unit under the cursor, if the cursor is on one.
    fn selected_unit(&self) -> Option<String> {
        match self.rows.get(self.cursor()) {
            Some(Node::Unit { unit, .. }) => Some(unit.name.clone()),
            Some(Node::Timer { name, .. }) => Some(name.clone()),
            _ => None,
        }
    }

    /// Asks before running a systemctl verb, and consults the
    /// persistence-ownership guard first for the two that change what
    /// happens at boot.
    ///
    /// The guard is the point: on a declaratively managed host, enabling
    /// a unit either fails outright or is undone by the next rebuild, and
    /// an operator finding that out afterwards is exactly what the design
    /// built `Ownership::Declarative` to prevent. It is named in the
    /// prompt rather than reported after the fact.
    fn ask_unit(&mut self, verb: UnitVerb) {
        let Some(unit) = self.selected_unit() else { return };
        let mut prompt = format!("{} {unit}?", verb.label());
        if verb.is_persistent() {
            match self.platform.unit_ownership(&unit) {
                Ok(Ownership::Declarative { source, note, reverted_by }) => {
                    // The note carries *how* it fails - refused now, or
                    // undone later - which is a different thing to plan
                    // around and is the adapter's to know.
                    prompt.push_str(&format!("  {note}; declared in {source}, changed by {reverted_by}"));
                }
                // An adapter that cannot answer must not be read as "it
                // persists": say so and let the operator decide.
                Err(e) => prompt.push_str(&format!("  (ownership unknown: {e})")),
                Ok(Ownership::Imperative) => {}
            }
        }
        prompt.push_str("  y / n");
        self.confirm_prompt = prompt;
        self.pending = Some(Pending::Unit { unit, verb });
    }

    /// Reads the log the other way round.
    ///
    /// The cursor goes back to the top rather than following the row it
    /// was on: the point of flipping is to look at the other end, and
    /// landing where you already were would make the key look inert.
    fn toggle_log_order(&mut self) {
        self.log_newest_first = !self.log_newest_first;
        self.rebuild();
        let top = self.legal_cursor(0);
        self.set_cursor(top);
    }

    /// Whose log `l` should open for the row under the cursor.
    ///
    /// A unit's own, except for a timer, where it is the unit the timer
    /// *runs*. A timer logs almost nothing itself - on this host
    /// `journalctl -u foo.timer` returns two dozen lines, every one of
    /// them pid 1 saying "Started foo.timer" - while the output anyone
    /// pressing `l` on a timer wants is the job's. It is the same reason
    /// the Timers section exists: a timer's own state, and its own log,
    /// answer the wrong question.
    fn log_target(&self) -> Option<String> {
        match self.rows.get(self.cursor()) {
            Some(Node::Unit { unit, .. }) => Some(unit.name.clone()),
            Some(Node::Timer { activates, .. }) => Some(activates.clone()),
            _ => None,
        }
    }

    /// Shows the log for the unit under the cursor.
    ///
    /// A real `journalctl -u` query rather than filtering the entries the
    /// Journal buffer already had. Text-filtering was measurably useless
    /// for the case this key exists for: on this host the one failed unit
    /// has 320 journal entries and none of them fall inside the Journal
    /// buffer's one-hour window, so pressing `l` on it showed nothing at
    /// all - while a text match would also have caught every other
    /// service that happened to mention the name.
    fn show_logs(&mut self) {
        let Some(unit) = self.log_target() else { return };
        // Back goes to the row that was under the cursor, which for a
        // timer is the timer - not the unit whose log this is.
        let Some(from) = self.selected_unit() else { return };
        self.came_from = Some((self.buffer, from));

        // A fresh look at a unit is a fresh view of it. Filters are per
        // view, which is right for the four fixed ones - but this view's
        // *identity* is whichever unit `l` last named, so a filter that
        // made sense for one unit's log would carry onto the next one's
        // and silently hide most of it. Leaving a view narrowed and coming
        // back is a state you chose; arriving somewhere new already
        // narrowed is not.
        //
        // A tick or `g` does not come through here, so a filter set on the
        // log you are reading survives its own refreshes.
        self.filters.remove(Buffer::Log.title());

        // The scope moves before the read, and the entries are replaced
        // whatever the read returned. Assigning them only on success left
        // a failed fetch showing the *previous* unit's log under the
        // previous unit's name, with an error line underneath that most
        // people would not read before believing the rows.
        self.unit_log = Some(unit.clone());
        match self.system.unit_journal(&unit, LOG_VIEW_LINES) {
            Ok(entries) => {
                self.facts.journal = entries;
                self.error = None;
            }
            Err(e) => {
                self.facts.journal.clear();
                self.report(Err(e));
            }
        }
        self.buffer = Buffer::Log;
        self.rebuild();
    }

    /// Nudges niceness by one, immediately.
    ///
    /// No confirmation: it is reversible, and htop's `[`/`]` taught
    /// everyone that trying it is free. Lowering niceness needs privilege,
    /// so the refusal is reported rather than swallowed.
    fn nudge_nice(&mut self, delta: i32) {
        let Some((pid, name, nice)) = self.selected_proc() else { return };
        let _ = name;
        let result = self.system.renice(pid, nice + delta);
        self.report(result);
    }

    /// Holds a failure for display, and clears the last one on success.
    ///
    /// The design's error table: an action's failure is shown and *held*,
    /// because a refresh two seconds later must not silently erase the
    /// reason something did not happen.
    fn report(&mut self, result: Result<(), MasysError>) {
        self.error = result.err().map(|e| e.to_string());
    }

    /// Recomputes the footer's action keys for wherever the cursor is.
    ///
    /// Called after every keypress rather than from `set_cursor`, which
    /// runs once per step: a page-down would otherwise redo this ten
    /// times to land in one place.
    fn refresh_actions(&mut self) {
        self.actions = self.dim_unavailable(self.keymap.actions(self.buffer));
    }

    /// Marks the keys that will not do what they promise here.
    ///
    /// Dimmed rather than removed: the design's rule is that masys never
    /// hides an action outright, only marks it. A key that vanishes when
    /// it is inapplicable punishes muscle memory and leaves the operator
    /// wondering whether they misremembered the binding; a dimmed one in
    /// its usual place answers the question on sight.
    ///
    /// Today only the persistence guard dims anything: on a host where a
    /// unit is generated from configuration, enable and disable are
    /// either refused outright or undone by the next rebuild. The
    /// judgement is per *unit*, not per host - a hand-placed unit on the
    /// same machine is genuinely imperative - so it follows the cursor.
    fn dim_unavailable(&self, actions: Vec<KeyBinding>) -> Vec<KeyBinding> {
        let on_a_unit = self.selected_unit().is_some();
        let declarative = self
            .selected_unit()
            .and_then(|unit| self.platform.unit_ownership(&unit).ok())
            .is_some_and(|ownership| matches!(ownership, Ownership::Declarative { .. }));
        let failed = self.selected_unit_failed();

        actions
            .into_iter()
            .map(|binding| {
                let Some(Action::Unit(verb)) = self.keymap.action_for(self.buffer, &binding.chord) else {
                    return binding;
                };
                // Two reasons a unit verb will not do what it says: the
                // manifest owns the unit, or the verb only applies to a
                // failure that has not happened. Both are marked rather
                // than removed.
                let dimmed = (verb.is_persistent() && declarative) || (verb.needs_failure() && on_a_unit && !failed);
                KeyBinding { dimmed, ..binding }
            })
            .collect()
    }

    /// Every group currently in the buffer, for collapse-all.
    fn group_names(&self) -> HashSet<String> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Node::ProcGroup { name, .. } => Some(name.clone()),
                Node::SectionHeader { title, kind: SectionKind::Units | SectionKind::Journal, .. } => Some(title.clone()),
                _ => None,
            })
            .collect()
    }

    /// True while a log is on screen, so the session can wake often
    /// enough to follow it.
    ///
    /// Asked of the app rather than assumed by the loop, because "which
    /// view is open" is the app's to know - the same reason `wants_refresh`
    /// exists.
    pub fn following(&self) -> bool {
        self.buffer == Buffer::Log && self.unit_log.is_some()
    }

    /// Picks up whatever has been appended to the open log.
    ///
    /// Reports whether anything arrived, so the session can skip a redraw
    /// it does not need - this runs four times a second and answers "no"
    /// almost every time.
    pub fn follow_log(&mut self) -> bool {
        let Some(unit) = self.unit_log.clone() else { return false };
        match self.system.follow_unit_journal(&unit, LOG_VIEW_LINES) {
            Ok(Some(entries)) => {
                self.facts.journal = entries;
                self.rebuild();
                true
            }
            // Nothing new, or an adapter that cannot follow at all - on
            // which the ordinary tick still refreshes the view.
            _ => false,
        }
    }

    /// True when the key means "sample now", so the caller can do it with
    /// its own clock rather than the session inventing one.
    pub fn wants_refresh(&self, key: Key) -> bool {
        self.keymap.resolve(self.buffer, key) == Some(Action::Refresh)
    }

    /// The popup, if any. At most one is ever open: the help is a
    /// reference card any key dismisses, and a confirmation blocks
    /// everything until answered.
    fn modal(&self) -> Option<ModalView<'_>> {
        if self.help_open {
            // The same dimming as the footer: the two must not disagree
            // about whether a key applies.
            let groups = self
                .keymap
                .describe(self.buffer)
                .into_iter()
                .map(|group| KeyGroup { bindings: self.dim_unavailable(group.bindings), ..group })
                .collect();
            return Some(ModalView::Keys { groups });
        }
        if self.pending.is_some() {
            return Some(ModalView::Confirm { prompt: &self.confirm_prompt });
        }
        // The filter is not a popup: it belongs at the top of the buffer
        // it is narrowing, where it stays visible while the rows move
        // under it. A centred modal hid the very rows it was filtering.
        None
    }

    /// Everything the renderer needs for one frame, and nothing else.
    pub fn view(&self) -> View<'_> {
        View {
            header: match self.buffer {
                Buffer::Status => Header::Status { hostname: &self.hostname, timestamp: &self.timestamp },
                Buffer::Procs => Header::Procs { sort: self.sort, descending: self.sort_descending },
                Buffer::Systemd => Header::Titled("systemd"),
                Buffer::Log => Header::Log { unit: self.unit_log.as_deref().unwrap_or("log"), newest_first: self.log_newest_first },
                Buffer::Io => Header::Titled("IO"),
            },
            rows: &self.rows,
            selected: (!self.rows.is_empty()).then(|| self.cursor()),
            status: match &self.error {
                Some(message) => StatusLine::Error(message),
                None => StatusLine::Hints,
            },
            modal: self.modal(),
            hints: &self.hints,
            actions: &self.actions,
            filter: (!self.filter().is_empty() || self.typing_filter).then_some(self.filter()),
            typing: self.typing_filter,
            filter_matches: self.filter_matches,
            auto_refresh_paused: self.auto_refresh_paused(),
        }
    }
}

/// Rows a page key moves by. Fixed rather than the rendered height for the
/// reason given at its call site.
const PAGE: usize = 10;

/// How many lines the log view fetches. It is a log viewer - scrollable
/// and searchable - so it holds more than the few a glance needs, while
/// staying well short of asking journalctl for a unit's entire history.
const LOG_VIEW_LINES: usize = 2_000;

/// How far back to look for a failed unit's last words. Small: the line
/// that explains a failure is almost always among the last few the unit
/// printed, and this runs once per newly-failed unit.
const FAILURE_LOG_LINES: usize = 30;

/// The unit's own last words, rather than systemd's report that it died.
///
/// journald tags a unit's own output with that unit, while systemd's
/// messages *about* it come from pid 1 and are tagged `init.scope`. So
/// "Main process exited, code=exited" and "Failed with result
/// 'exit-code'" are systemd talking, and the line worth showing is the
/// last one the unit itself printed - here, `curl: (6) Could not resolve
/// host`.
///
/// Falls back to the most recent error-priority line when the unit said
/// nothing of its own, which is the case for one that failed before it
/// could exec.
fn last_words(entries: &[Entry], unit: &str) -> Option<String> {
    entries
        .iter()
        .rev()
        .find(|e| e.unit.as_deref() == Some(unit))
        .map(|e| e.message.clone())
        .or_else(|| {
            entries
                .iter()
                .rev()
                .find(|e| matches!(e.priority, Priority::Emergency | Priority::Alert | Priority::Critical | Priority::Error))
                .map(|e| e.message.clone())
        })
        .map(|message| message.trim().to_string())
        .filter(|message| !message.is_empty())
}
