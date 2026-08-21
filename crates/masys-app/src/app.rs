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

use masys_domain::declarative::{
    DeclarativeService, Generation, HomeMode, InputSource, Inputs, NixOp, Profile, ProfileKind, RebootState, RebuildVerb,
};
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
use masys_view::{Header, KeyBinding, KeyGroup, ModalView, Node, Overview, StatusLine, View};

use crate::buffer::Buffer;
use crate::io_buffer::build_io_rows;
use crate::key::{Key, KeyCode};
use crate::keymap::{Action, Keymap, NixVerb, Sort, UnitVerb};
use crate::log_buffer::build_log_rows;
use crate::nix_buffer::{build_nix_rows, nix_transient};
use crate::procs::build_proc_rows;
use crate::status::build_status_rows;
use crate::systemd_buffer::{build_unit_rows, severity, unit_rows, unit_transient};
use crate::transient::{DefGroup, DefRow, TransientDef};

/// A transient's open text-entry sub-step.
///
/// Holds the verb it will run rather than a closure or a queued `NixOp`,
/// because the operation cannot be built until the value exists - which
/// is the whole reason this state is here. `App::nix_op_typed` turns the
/// pair into an operation at the moment enter is pressed, so a tick that
/// lands mid-typing cannot move a row out from under an argument already
/// resolved.
#[derive(Debug, Clone)]
struct InputState {
    verb: NixVerb,
    prompt: &'static str,
    typed: String,
}

/// What a Nix verb can do on the row the cursor is on.
///
/// Three answers where there used to be two, because the transient
/// brought verbs that need a value nobody has read: a query, an option
/// name, a generation spec. `Asks` is not "cannot act here" - the row is
/// live, and pressing it opens the input sub-step - and folding the two
/// together would mark `search packages` on every host, which is the one
/// operation that always works.
///
/// One function answers all three, which is the invariant `nix_offer`'s
/// doc protects: the footer, the popup and the dispatch all ask it, so a
/// row that is offered is a row that will act.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NixOffer {
    /// Runnable now, arguments resolved.
    Ready(NixOp),
    /// Runnable once the operator types something.
    Asks { prompt: &'static str },
    /// Not runnable here.
    No,
}

/// An action waiting on confirmation.
#[derive(Debug, Clone)]
enum Pending {
    Kill {
        pid: u32,
        signal: Signal,
    },
    Unit {
        unit: String,
        verb: UnitVerb,
    },
    /// A Nix operation that changes the machine. The arguments are
    /// already resolved: which generation, which profile, which
    /// retention. Resolving them when the key is pressed rather than when
    /// the confirmation is answered is what makes the prompt able to name
    /// them, and it is also what keeps a tick that lands in between from
    /// moving the rows under an answer already given.
    Nix {
        op: NixOp,
    },
}

/// What [`Flow::Suspend`] was asked for.
///
/// A bare unit name until now, because `systemctl edit` was the only
/// thing in masys that needed the terminal. Every Nix operation needs it
/// too, and more so: `nixos-rebuild switch` streams for minutes and
/// `nix store diff-closures` pages, which is why
/// `masys_platform_nixos::ops` spawns them with stdio inherited rather
/// than capturing their output.
///
/// So the field carries *what to run* rather than one operation's
/// argument, and [`App::run_suspended`] stays the single place that turns
/// it into a port call. The alternative - a second `Option` field beside
/// the first - would let both be `Some` at once, and nothing in the type
/// would say which of the two the caller was about to run.
#[derive(Debug, Clone)]
enum Suspended {
    EditUnit(String),
    Nix(NixOp),
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
    /// Returned when an action has to run a program that owns the screen:
    /// `systemctl edit`, which spawns `$EDITOR`, and every Nix operation,
    /// which streams its own output for as long as it takes. The caller
    /// releases the terminal, calls [`App::run_suspended`], and restores
    /// it. The session cannot do any of that itself: no layer above the
    /// composition root knows what a terminal is, which is the rule that
    /// keeps every crate here testable without one.
    ///
    /// What is waiting is `Suspended`, not this variant: a payload here
    /// would have to be public, and `Flow` is the one thing the binary
    /// matches on. What comes *back* is [`Aftermath`], because by then
    /// the command has run and there is a second fact to report.
    Suspend,
}

/// What the suspended command left on the screen the caller is about to
/// take back.
///
/// The caller cannot work this out for itself and must not guess: it hands
/// the terminal over blind, and only the session knows whether what ran
/// there was an editor the operator drove by hand or a command that
/// printed and exited. `nix store diff-closures` between generations 427
/// and 438 prints 84 lines and exits in 0.32 s on this host - measured
/// three times, and through masys itself - so a caller that re-entered the
/// alternate screen the moment the command returned took the diff with it.
/// The output survives only in the normal screen's scrollback, which is
/// invisible while masys is drawing.
///
/// A `bool` carries the same two states and is the obvious shape. It is
/// rejected for what it forces the *caller* to be named after:
/// the only honest name for a `bool` at that call site is `should_pause`,
/// and pausing is a terminal decision this crate is not allowed to hold an
/// opinion about - the rule [`Flow::Suspend`] states. The fact this crate
/// owns is what became of the output; what to do about it is the
/// composition root's, and a type named for the fact is what keeps the two
/// from being written as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aftermath {
    /// The operator has already read whatever is up there.
    ///
    /// An editor holds the terminal until it is quit, so by the time it
    /// returns its screen has been read and dismissed by the person who
    /// was reading it. Also the answer when nothing ran at all: there is
    /// no output, so none of it is unread.
    Seen,
    /// Output that ends the instant the command does, and that nobody has
    /// had a chance to read.
    Unseen,
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
    /// The seven below come from the declarative port, and every one of
    /// them is a keep-last-good reading - see `keep_last_good`. They are
    /// carried across the rebuild of `Facts` in `tick` for that reason,
    /// the way `journal` is, and they stay at their defaults for the whole
    /// session on a host that has no such port.
    ///
    /// Five of the seven are `Option`, and for one reason rather than
    /// five: a default that renders as a reading is a reading masys did
    /// not take. `Vec::new()` renders as `0 system generations . 0 home`,
    /// `0` as `0 gc roots`, and a bare `None` retention as a gc job that
    /// keeps nothing - three claims an operator would act on, made on the
    /// strength of a read that failed. Only `reboot` and `nix_inputs`
    /// default honestly, because for those two the port documents `None`
    /// as an answer in its own right.
    ///
    /// `None` here means "no read has succeeded yet", never "the host has
    /// none of these".
    profiles: Option<Vec<Profile>>,
    reboot: Option<RebootState>,
    nix_inputs: Option<Inputs>,
    /// `None` until a read succeeds, which is not the same fact as
    /// `Absent`. `Absent` is the port's answer for "home-manager is not
    /// installed on this host"; a host whose `home_mode` read keeps
    /// failing has not said that, and must not be recorded as having.
    home_mode: Option<HomeMode>,
    nixpkgs_rev: Option<String>,
    /// Doubly optional, because there are three facts. The outer `None`
    /// is "not read"; `Some(None)` is the port's own answer that
    /// `nix-gc.service` declares no `--delete-older-than`. The port's doc
    /// is explicit that its `Err` must not collapse into that inner
    /// `None`, and a single `Option` here is exactly that collapse.
    gc_retention: Option<Option<String>>,
    /// `Option<u32>` where the port answers `u32`, for the reason the
    /// port's own doc gives for refusing to answer `0`: `0` reads as
    /// "nothing is pinning your store". Storing the default would put the
    /// claim back one layer up.
    gc_roots: Option<u32>,
    /// Doubly optional, for `gc_retention`'s reason: the outer `None` is
    /// "not read", and `Some(None)` is the port's own answer that no
    /// flake reference resolves - which is a channels host, not a
    /// failure. Collapsing the two would mark the flake-only rows on a
    /// host whose read merely failed, and mark them for the wrong reason.
    flake_ref: Option<Option<String>>,
    /// And where `<nixos-config>` resolves, doubly optional for the same
    /// reason. `nixos-option` is the one operation that needs it.
    nixos_config: Option<Option<String>>,
}

/// Takes a port's answer, and keeps the last good one where the read
/// failed.
///
/// The rule every declarative read follows, named once rather than
/// restated seven times. `Err` from these methods means "could not find
/// out", and storing what `unwrap_or_default` would produce - no
/// generations, no reboot pending, no gc policy, nothing pinning the
/// store - turns a read failure into a positive claim about the host.
/// Every one of those claims is actionable, and every one of them is
/// wrong.
///
/// `Ok(None)` still overwrites, and has to: the port documents `None` as
/// a real answer wherever it returns one - booted and current are the same
/// store path, this configuration pins nothing - so holding a stale `Some`
/// over it would leave a reboot showing as pending after the reboot.
fn keep_last_good<T>(slot: &mut T, read: Result<T, MasysError>) {
    if let Ok(reading) = read {
        *slot = reading;
    }
}

/// Whether an operation reads and changes nothing.
///
/// The question the confirmation turns on, decided per operation against
/// what `masys_platform_nixos::ops::argv` runs for each, and matched
/// exhaustively rather than with a wildcard: a `NixOp` added later must
/// be looked at here, because the answer a wildcard would give it is the
/// one nobody would notice being wrong.
///
/// The four that only read do so plainly. `nix store diff-closures`
/// prints two closures' difference; `nixos-option` prints this host's
/// value for one option; `nix search nixpkgs` prints matches. `nix flake
/// check` is the one worth arguing about - it *builds* the flake's checks
/// and so adds store paths - but it activates nothing, changes no
/// profile, and leaves nothing an operator would have to undo, which is
/// the line that matters for a prompt.
fn only_reads(op: &NixOp) -> bool {
    match op {
        NixOp::Diff { .. } | NixOp::FlakeCheck | NixOp::OptionValue { .. } | NixOp::SearchPackages { .. } => true,
        // The rest all change something an operator would have to undo: a
        // running system and its boot default, the generations a rollback
        // needs, or the lock file a configuration is pinned by.
        NixOp::Rebuild(_)
        | NixOp::Activate { .. }
        | NixOp::Rollback
        | NixOp::Clean { .. }
        | NixOp::DeleteGenerations { .. }
        | NixOp::FlakeUpdate { .. }
        | NixOp::ChannelUpdate
        | NixOp::ChannelRollback
        | NixOp::HomeSwitch => false,
    }
}

/// What a confirmation must say the operation will do, or `None` for an
/// operation no key produces yet.
///
/// `ask_kill` names the signal because "SIGKILL cannot be caught, and the
/// difference is the whole reason there are two bindings". The same
/// standard here: activating a generation and deleting a fortnight of
/// them are different acts, and a prompt that said only "confirm?" would
/// make them look like one.
fn prompt_for(op: &NixOp) -> Option<String> {
    match op {
        // "and make it the boot default" because that is the second half
        // of what runs: `switch-to-configuration switch` activates *and*
        // installs the bootloader entry, where `test` would activate
        // without touching what boots. An operator who reads this as
        // "just for now" would be wrong in the way that matters.
        NixOp::Activate { generation, .. } => Some(format!("activate system generation {generation} and make it the boot default?")),
        // "in every profile", because that is what happens and it is not
        // what the row under the cursor suggests. nix-collect-garbage(1)
        // for nix 2.34.8 on this host: `--delete-older-than` "deletes all
        // generations of profiles older than the specified amount" across
        // every profile it finds, not the one the cursor is in - and then
        // collects. The generation active at that point in time survives,
        // which is why this does not say "every generation".
        NixOp::Clean { older_than } => Some(format!("delete generations older than {older_than} in every profile and collect the store?")),
        // The transient made the rest reachable, so the rest have
        // sentences now - written here, where there is something to
        // press, exactly as the note this replaces said they would be.
        //
        // Each says what it changes rather than what it is called. A
        // prompt reading "run switch?" tells an operator who already
        // pressed `s` nothing they did not know.
        NixOp::Rebuild(RebuildVerb::Switch) => Some("build this configuration, activate it, and make it the boot default?".to_string()),
        NixOp::Rebuild(RebuildVerb::Boot) => {
            Some("build this configuration and make it the boot default, without activating now?".to_string())
        }
        // "until you reboot" is the whole of the difference from
        // `switch`, and it is the reason `test` is the safe one to try.
        NixOp::Rebuild(RebuildVerb::Test) => Some("build this configuration and activate it until you reboot?".to_string()),
        // Confirmed although it activates nothing: a build writes to the
        // store, takes minutes, and is not free to start by accident.
        NixOp::Rebuild(RebuildVerb::Build) => Some("build this configuration without activating it?".to_string()),
        NixOp::Rebuild(RebuildVerb::DryActivate) => Some("print what activating this configuration would change?".to_string()),
        NixOp::Rollback => Some("activate the previous generation and make it the boot default?".to_string()),
        NixOp::HomeSwitch => Some("build and activate the home-manager configuration?".to_string()),
        // Names the spec rather than a count, because masys did not count
        // them: `+5` and `30d` are what `nix-env` was given, and how many
        // generations that is on this host is a question only `nix-env`
        // answers. A prompt saying "delete 9 generations" would be a
        // number nobody measured.
        NixOp::DeleteGenerations { profile, spec } => Some(format!("delete the generations matching `{spec}` from {profile}?")),
        // "and re-lock every input" because that is the file that
        // changes, and the one a rebuild afterwards will build from.
        NixOp::FlakeUpdate { input: None } => Some("update every flake input and re-lock them?".to_string()),
        NixOp::FlakeUpdate { input: Some(name) } => Some(format!("update the flake input `{name}` and re-lock it?")),
        NixOp::ChannelUpdate => Some("update every channel?".to_string()),
        NixOp::ChannelRollback => Some("roll every channel back to its previous state?".to_string()),
        // The three that only read reach `run` without passing here.
        NixOp::Diff { .. } | NixOp::FlakeCheck | NixOp::OptionValue { .. } | NixOp::SearchPackages { .. } => None,
    }
}

pub struct App {
    system: Box<dyn SystemService>,
    platform: Box<dyn PlatformService>,
    /// The declarative port, on the hosts that have one. `None` is the
    /// ordinary case, and the reason the Nix view is absent rather than
    /// empty everywhere else - see `crate::buffer::Registry`, which holds
    /// the other half of that fact and is owned by the keymap.
    declarative: Option<Box<dyn DeclarativeService>>,
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
    /// The open transient, if one is. Held rather than rebuilt each draw
    /// because a switch toggled in it has to survive to the next frame,
    /// and because the rows it was built from can move under it - see
    /// `TransientDef`.
    ///
    /// Never `Some` at the same time as `pending`: dispatching a row
    /// closes the transient before the action runs, so the confirmation
    /// a row raises is answered against the buffer, not through a popup
    /// that is still covering it.
    transient: Option<TransientDef>,
    /// The open input sub-step, if one is.
    ///
    /// Its own field rather than a variant inside `transient`, because
    /// the two are independent: a transient closes when it dispatches the
    /// row that asked, and the input outlives it. Sharing one field would
    /// mean the popup that opened the prompt had to stay open behind it
    /// to hold the prompt's own state.
    input: Option<InputState>,
    /// Why each failed unit failed, keyed by unit and roughly when.
    failure_reasons: HashMap<(String, u64), Option<String>>,
    /// Whose log the log view is currently showing.
    unit_log: Option<String>,
    /// Which way the log view reads. Newest first by default: `l` is
    /// pressed because something just happened, and the answer should not
    /// be two thousand lines down.
    log_newest_first: bool,
    /// What an approved action is waiting to run, held from the moment it
    /// was approved until the caller has given the terminal up. `None` at
    /// every other moment.
    suspended: Option<Suspended>,
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
            declarative: None,
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
            transient: None,
            input: None,
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

    /// The composition root's entry point once a declarative adapter may
    /// exist. `App::new` keeps its signature and passes `None`, so every
    /// existing call site is unaffected.
    ///
    /// The caller pairs the service with a keymap built against a matching
    /// registry - `Keymap::for_registry(Registry::new(true))` where a
    /// service exists - and this is the only moment both are in one hand,
    /// so it is the only place the pairing can be checked. A mismatch is
    /// silent otherwise, and silent in both directions: a service with a
    /// default keymap builds a view no key reaches, and a Nix-bearing
    /// keymap with no service gives `5` a screen with nothing on it, which
    /// is the exact failure `Registry` was added to prevent.
    ///
    /// `debug_assert` rather than `assert`: the suite and every debug
    /// build catch it, while a release binary degrades to one wrong view
    /// rather than refusing to start. A tool for looking at a sick machine
    /// that will not launch is worse than one missing a screen.
    pub fn with_declarative(
        system: Box<dyn SystemService>,
        platform: Box<dyn PlatformService>,
        scanner: Box<dyn DirScanner>,
        hostname: String,
        keymap: Keymap,
        declarative: Option<Box<dyn DeclarativeService>>,
    ) -> Self {
        let mut app = Self::with_keymap(system, platform, scanner, hostname, keymap);
        debug_assert_eq!(
            app.keymap.registry().contains(Buffer::Nix),
            declarative.is_some(),
            "the keymap's registry and the declarative service disagree about whether this host has a Nix view"
        );
        app.declarative = declarative;
        app
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
            // Carried across, like `journal` above and for a related
            // reason: these are keep-last-good readings, and the sample
            // below replaces only the ones that came back. Rebuilding them
            // from `Default` here would erase the previous tick's answers
            // before the port had a chance to fail.
            profiles: self.facts.profiles.take(),
            reboot: self.facts.reboot.take(),
            nix_inputs: self.facts.nix_inputs.take(),
            home_mode: self.facts.home_mode,
            nixpkgs_rev: self.facts.nixpkgs_rev.take(),
            gc_retention: self.facts.gc_retention.take(),
            gc_roots: self.facts.gc_roots,
            flake_ref: self.facts.flake_ref.take(),
            nixos_config: self.facts.nixos_config.take(),
        };
        // Sampled on the ordinary tick, at whatever cadence the caller
        // runs one - two seconds in the binary.
        //
        // The design's table puts a platform read on 60s, and an earlier
        // draft of this block carried a comment saying so while reading
        // every tick. Only one of the two could be true, and it is this
        // one, because the alternative costs more than it saves: `g`,
        // "refresh now", reaches the session through this same `tick`, so
        // a 60-second gate here would quietly make the one key that
        // promises a fresh sample not deliver one. Telling a timed call
        // from a keyed one means a `tick` signature the composition root
        // has to thread a flag through, for a read this cheap.
        // `masys::TICK` records the same decision for the same reason:
        // one interval for everything until a buffer needs otherwise.
        //
        // What it costs, per tick: `canonicalize` on a handful of links, a
        // `read_dir` of two profile directories and the unit directory, a
        // `flake.lock` parse, and two small file reads. Closure sizes -
        // the one genuinely expensive read - are deferred to `enter` on a
        // row, not read on every tick.
        if let Some(declarative) = self.declarative.as_ref() {
            keep_last_good(&mut self.facts.profiles, declarative.profiles().map(Some));
            keep_last_good(&mut self.facts.reboot, declarative.reboot());
            keep_last_good(&mut self.facts.nix_inputs, declarative.inputs());
            keep_last_good(&mut self.facts.home_mode, declarative.home_mode().map(Some));
            keep_last_good(&mut self.facts.nixpkgs_rev, declarative.running_nixpkgs_rev());
            keep_last_good(&mut self.facts.gc_retention, declarative.gc_retention().map(Some));
            keep_last_good(&mut self.facts.gc_roots, declarative.gc_roots().map(Some));
            keep_last_good(&mut self.facts.flake_ref, declarative.flake_ref().map(Some));
            keep_last_good(&mut self.facts.nixos_config, declarative.nixos_config().map(Some));
        }
        // Drained, never waited on: the walk is on its own threads and
        // this is the seam where what it has found so far crosses back.
        if self.scan_root.is_some() {
            self.scan = self.scanner.poll();
        }
        // The Nix view too, now that it has units of its own: an open
        // detail block that stopped being re-read would sit there showing
        // the memory figure it had when it was opened.
        if matches!(self.buffer, Buffer::Systemd | Buffer::Nix) {
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
            Buffer::Nix => {
                // `/nix` where the store has a filesystem of its own,
                // falling back to `/` where it is a directory on the root
                // one. `None` where neither could be measured:
                // `unwrap_or(0.0)` here would draw "0% used, 0 bytes free"
                // for a store nothing is known about, which is a
                // fabricated reading in the shape of a real one and the
                // failure this whole change keeps having to design
                // against.
                let mount = |wanted: &str| self.facts.filesystems.iter().find(move |fs| fs.mount_point == wanted);
                let nix_fs = mount("/nix").or_else(|| mount("/"));
                build_nix_rows(
                    self.facts.profiles.as_deref(),
                    self.facts.reboot.as_ref(),
                    self.facts.nix_inputs.as_ref(),
                    // `Absent` for a reading that has not come back, which
                    // is not the fabrication it looks like. The builder
                    // uses this for exactly one thing - whether the
                    // home-manager heading says it is activated by
                    // `nixos-rebuild` - and `Absent` is its neutral case,
                    // identical to `Standalone`. The claim, `Module`, is
                    // only ever made from a read that succeeded.
                    self.facts.home_mode.unwrap_or(HomeMode::Absent),
                    self.facts.nixpkgs_rev.as_deref(),
                    self.facts.gc_roots,
                    self.last_now_ms,
                    nix_fs.map(|fs| fs.used_percent),
                    nix_fs.map(|fs| fs.free_bytes),
                    self.nix_policy_rows(),
                    self.nix_unit_rows(),
                    collapsed,
                )
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

    /// The declared maintenance policy, as rows.
    ///
    /// The schedule comes from the timer units masys already polls; only
    /// the retention comes from the declarative port. Asking that port for
    /// a schedule would be it reimplementing systemd, and the two would
    /// then disagree about a timer the operator had overridden.
    ///
    /// A job whose timer unit is not on this host contributes no row.
    /// Absent is not empty: "nix-optimise is not scheduled" and
    /// "nix-optimise is scheduled and has never run" send an operator
    /// looking in two different places.
    fn nix_policy_rows(&self) -> Vec<Node> {
        [("gc", "nix-gc.timer"), ("optimise", "nix-optimise.timer")]
            .into_iter()
            .filter_map(|(job, unit_name)| {
                let unit = self.facts.units.iter().find(|unit| unit.name == unit_name)?;
                let timer = unit.timer.as_ref();
                Some(Node::NixPolicy {
                    job: job.to_string(),
                    unit: unit_name.to_string(),
                    // Only the gc job has one. Nothing is retained by
                    // optimising, so a retention on that row would be a
                    // column borrowed from its neighbour.
                    // `Some(None)` for optimise, not `None`: optimising
                    // retains nothing at all, which is a fact about the
                    // job rather than a reading that failed. `-` would
                    // say masys could not find out.
                    retention: if job == "gc" { self.facts.gc_retention.clone() } else { Some(None) },
                    next_in_ms: timer.and_then(|timer| timer.next_ms).map(|next| next.saturating_sub(self.last_now_ms)),
                    last_ago_ms: timer.and_then(|timer| timer.last_ms).map(|last| self.last_now_ms.saturating_sub(last)),
                    enabled: unit.enabled,
                })
            })
            .collect()
    }

    /// Nix's own units, as rows.
    ///
    /// The cheapest section in the view: a filter over the units masys
    /// already polls every tick, so it costs one predicate and no read.
    /// It answers what nothing else in this view does - whether the daemon
    /// is up, whether this boot's home-manager activation actually
    /// succeeded - which is the running end of declared, realised,
    /// running.
    ///
    /// Built here rather than in `build_nix_rows` for the reason
    /// `nix_policy_rows` is: this is the layer that holds the poll and the
    /// map of which units are open.
    ///
    /// Worst first, by `systemd_buffer::severity`, so a failed
    /// `nix-gc.service` is the first row under the heading rather than
    /// somewhere in an alphabetical six.
    fn nix_unit_rows(&self) -> Vec<Node> {
        let mut units: Vec<&masys_domain::unit::Unit> =
            self.facts.units.iter().filter(|unit| crate::nix_buffer::is_nix_unit(&unit.name)).collect();
        units.sort_by(|a, b| severity(a.active_state).cmp(&severity(b.active_state)).then_with(|| a.name.cmp(&b.name)));
        // Flat, at depth 0 and with no `children` marker: the causal tree
        // the systemd view draws under a socket or a timer would list
        // `nix-daemon.service` twice in a section of six, once on its own
        // and once under its socket.
        units.into_iter().flat_map(|unit| unit_rows(unit, &self.expanded_units, self.last_now_ms, 0, false)).collect()
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
        // The input sub-step is innermost: it is open only while a
        // transient row is waiting on a value, and every printable key
        // belongs to it.
        if self.input.is_some() {
            return self.answer_input(key);
        }
        // A transient answers its own keys, ahead of the keymap and ahead
        // of the escape ladder below - the same precedence
        // `answer_confirm` has, and for the same reason: a popup taking
        // keystrokes must not let one through to the buffer underneath.
        if self.transient.is_some() {
            return self.answer_transient(key);
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

        self.dispatch(self.keymap.resolve(self.buffer, key))
    }

    /// Performs one resolved action.
    ///
    /// Split out of [`App::handle_key`] because a transient row dispatches
    /// the same `Action` a keypress does - that is what the definition
    /// carrying its own `Action` buys - and two copies of this match would
    /// be two answers to "what does this verb do".
    ///
    /// Takes an `Option` because `Keymap::resolve` returns one and an
    /// unbound key is not an error: it does nothing, and doing nothing
    /// still refreshes the footer.
    fn dispatch(&mut self, action: Option<Action>) -> Flow {
        match action {
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
            // Returns rather than falling through to the footer refresh,
            // for the reason `Action::Quit` does: this is the only
            // view-local action that can ask for the terminal, and a
            // `Flow` the match swallowed would leave the caller holding
            // it. Nothing is lost - `run_suspended` rebuilds once the
            // command is done, and the next keypress refreshes the offer.
            Some(Action::Nix(verb)) => {
                if self.nix(verb) == Flow::Suspend {
                    return Flow::Suspend;
                }
            }
            Some(Action::Transient) => self.open_row_transient(),
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
            // Units in the systemd view, days in the log, generations and
            // inputs in the Nix view: all are sections whose rows fold
            // away under their heading. Which kinds those are, and what
            // each folds under, both live on `SectionKind` - the two used
            // to be spelled out here and again in `group_names`, kept in
            // step by a comment.
            Some(Node::SectionHeader { title, kind, .. }) if kind.folds() => kind.fold_key(title),
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
    /// Opens the transient for the row the cursor is on.
    ///
    /// Only a unit row has one today. A row that has none opens nothing
    /// rather than opening an empty popup, which is the same choice
    /// `crate::buffer` makes about a digit with no buffer behind it: a
    /// screen that explains why it is empty is worse than a key that does
    /// nothing.
    ///
    /// A failed ownership read is passed on as `None` rather than being
    /// flattened into `Imperative`. The two are different answers, and
    /// only one of them was measured.
    fn open_row_transient(&mut self) {
        if let Some(unit) = self.selected_unit() {
            let ownership = self.platform.unit_ownership(&unit).ok();
            self.transient = Some(unit_transient(&unit, ownership.as_ref()));
            return;
        }
        // The Nix buffer's own popup, on any row of it that is not a
        // unit. Unlike the unit popup it is not about the row: only its
        // Generation group is, and that group is simply absent elsewhere.
        if self.buffer == Buffer::Nix {
            let generation = self.selected_generation().map(|(generation, _)| generation.id);
            let retention = self.facts.gc_retention.clone().flatten();
            self.transient = Some(self.mark_nix(nix_transient(generation, retention.as_deref())));
        }
    }

    /// Marks the rows of a Nix transient that cannot act here, and says
    /// why.
    ///
    /// Asked of `nix_offer`, which is also what the footer asks, so a
    /// marked row and a dim key cannot disagree about the same verb.
    /// `NixOffer::Asks` is *not* marked: the row is live, and pressing it
    /// opens the prompt.
    fn mark_nix(&self, def: TransientDef) -> TransientDef {
        TransientDef {
            groups: def
                .groups
                .into_iter()
                .map(|group| DefGroup {
                    rows: group
                        .rows
                        .into_iter()
                        .map(|row| match row.action {
                            Action::Nix(verb) => DefRow { dimmed: self.nix_offer(verb) == NixOffer::No, note: self.nix_note(verb), ..row },
                            _ => row,
                        })
                        .collect(),
                    ..group
                })
                .collect(),
            ..def
        }
    }

    /// What to say beside a Nix row - why it will not act, or what it
    /// will act with.
    ///
    /// A note is not the same question as a mark. `clean` is live and
    /// still worth annotating with the period it will use, because
    /// "delete old generations" without a number is the row an operator
    /// should not press.
    fn nix_note(&self, verb: NixVerb) -> Option<String> {
        let flake = self.facts.flake_ref.as_ref().and_then(|read| read.as_ref()).is_some();
        let channels = matches!(self.facts.nix_inputs.as_ref().map(|inputs| &inputs.source), Some(InputSource::Channels));
        match verb {
            NixVerb::FlakeUpdate | NixVerb::FlakeCheck if !flake => Some("no flake configured".to_string()),
            NixVerb::ChannelUpdate | NixVerb::ChannelRollback if !channels => Some("no channels on this host".to_string()),
            // Names what is missing rather than the paradigm, because a
            // flake host *can* have one: `nix.nixPath` puts it there, and
            // then the row is live.
            NixVerb::OptionValue if self.facts.nixos_config.as_ref().and_then(|read| read.as_ref()).is_none() => {
                Some("no nixos-config on the search path".to_string())
            }
            // The design's own wording. In `Module` there is no
            // `home-manager` binary at all, so the row is not merely
            // ineffective - the command does not exist.
            NixVerb::HomeSwitch => match self.facts.home_mode {
                Some(HomeMode::Module) => Some("activated by nixos-rebuild switch".to_string()),
                Some(HomeMode::Absent) => Some("home-manager not installed".to_string()),
                Some(HomeMode::Standalone) => None,
                None => Some("home-manager mode unread".to_string()),
            },
            // The host's declared retention, on the row that will use it.
            // Where none is declared the row is marked, and this says
            // which of the two absences it is - masys refusing to pick a
            // period is not the same as masys failing to read one.
            // The privilege reason comes first where it applies. A row
            // marked `--delete-older-than 14d` reads as one that will do
            // that, and on an unprivileged masys it will not - the
            // retention is not why it is marked.
            NixVerb::Clean => self.unwritable_note().or_else(|| match self.facts.gc_retention.clone() {
                Some(Some(period)) => Some(format!("--delete-older-than {period}")),
                Some(None) => Some("no retention declared".to_string()),
                None => Some("retention unread".to_string()),
            }),
            // The three that write a profile. `nix-env` has no elevation
            // flag - no polkit action to be granted, no `--elevate` to
            // pass - so the only lever masys has is to say so before the
            // key is pressed. A marked row with no reason beside it is
            // the one thing the design's marking rule exists to prevent.
            NixVerb::Activate => match self.selected_generation() {
                Some((_, ProfileKind::System)) => self.profile_note(ProfileKind::System),
                // The second command is the profile's own
                // `bin/switch-to-configuration`, and only a system
                // generation has one.
                Some(_) => Some("only a system generation activates".to_string()),
                None => None,
            },
            NixVerb::DeleteHere => self.selected_generation().and_then(|(_, kind)| self.profile_note(kind)),
            NixVerb::DeleteGenerations => self.profile_note(ProfileKind::System),
            // The five rebuild verbs differ only in what they do, and
            // their names do not say it - `boot` and `test` are nearly
            // opposites and both are one word. The confirmation's own
            // sentence is the row's note, so there is one description of
            // each verb rather than two that can drift apart.
            NixVerb::Rebuild(_) | NixVerb::Rollback => Some(verb.label().to_string()),
            _ => None,
        }
    }

    /// One keypress against the open transient.
    ///
    /// Four things can happen, and the order is the point. An action row
    /// wins over a switch, so a transient that gives a switch a letter
    /// its actions already use loses the switch rather than the action.
    /// Dispatching *closes* the transient first, the way magit's does:
    /// the action may raise a confirmation, and answering one through a
    /// popup that is still covering the rows it names is worse than
    /// having to press the key again.
    fn answer_transient(&mut self, key: Key) -> Flow {
        // Escape leaves. Checked before the chords so no transient can
        // bind its way out of being closeable - the same rule the buffer
        // escape ladder follows.
        if key.code == KeyCode::Esc {
            self.transient = None;
            self.refresh_actions();
            return Flow::Continue;
        }
        let KeyCode::Char(chord) = key.code else {
            return Flow::Continue;
        };
        let Some(def) = self.transient.as_mut() else {
            return Flow::Continue;
        };

        match def.action(chord) {
            Some(action) => {
                self.transient = None;
                self.dispatch(Some(action))
            }
            // A switch toggles in place: the popup stays open, because
            // the point of a switch is to set several before running
            // anything.
            None => {
                def.toggle(chord);
                Flow::Continue
            }
        }
    }

    /// One keypress against the open input sub-step.
    ///
    /// Escape backs out, enter commits, backspace deletes, and every
    /// other printable character is typed. Nothing else is bound: this is
    /// a prompt, and a prompt that swallowed a movement key would leave
    /// the operator unable to tell it from the buffer.
    ///
    /// An empty value does not commit. `nix search nixpkgs ""` matches
    /// every package in nixpkgs, and `nix-env --delete-generations ""`
    /// is an argument error - neither is what pressing enter on an empty
    /// prompt meant.
    fn answer_input(&mut self, key: Key) -> Flow {
        let Some(state) = self.input.as_mut() else { return Flow::Continue };
        match key.code {
            KeyCode::Esc => {
                self.input = None;
                self.refresh_actions();
                Flow::Continue
            }
            KeyCode::Backspace => {
                state.typed.pop();
                Flow::Continue
            }
            KeyCode::Enter => {
                if state.typed.is_empty() {
                    return Flow::Continue;
                }
                let (verb, typed) = (state.verb, state.typed.clone());
                self.input = None;
                match self.nix_op_typed(verb, &typed) {
                    Some(op) => self.run_nix(op),
                    None => Flow::Continue,
                }
            }
            KeyCode::Char(typed) => {
                state.typed.push(typed);
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

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
                    self.suspended = Some(Suspended::EditUnit(unit));
                    self.rebuild();
                    return Flow::Suspend;
                }
                // Like edit, and for the same reason: every one of these
                // owns the screen while it runs, so the session records
                // it and asks the caller for the terminal.
                Some(Pending::Nix { op }) => {
                    self.suspended = Some(Suspended::Nix(op));
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
    /// the caller's hands, and says what it left on the screen.
    ///
    /// Called by the composition root between releasing the terminal and
    /// restoring it. Does nothing if nothing is waiting, so a caller that
    /// calls it unconditionally is not wrong - which is what makes the
    /// contract hard to get subtly wrong.
    ///
    /// A failure is held on the status line like any other action's, and
    /// is worth holding: `systemctl edit` refuses outright where the unit
    /// directory is read-only, and `nixos-rebuild` exits non-zero on an
    /// evaluation error. In both cases the command's own message has
    /// already reached the terminal the caller handed over; what the
    /// status line adds is that it failed at all, which the operator
    /// would otherwise lose the moment the screen was redrawn.
    ///
    /// The same failure is also why an `Err` is always [`Aftermath::Unseen`],
    /// including the editor's. `Seen` rests entirely on the editor having
    /// held the terminal until the operator quit it - and a `systemctl
    /// edit` that was *refused* never opened one. What is on the screen
    /// then is a single line of systemctl's own text, in exactly the
    /// position the diff was in: printed and then swallowed. The two
    /// mistakes are not the same size either. Pausing where it was not
    /// needed costs one keypress; not pausing costs the only full account
    /// of why an operation failed, so this leans toward pausing.
    pub fn run_suspended(&mut self) -> Aftermath {
        let Some(suspended) = self.suspended.take() else { return Aftermath::Seen };
        // Whether the program holds the terminal until the operator leaves
        // it, or prints and exits. Answered beside the call rather than by
        // matching on the variant again further down: a third variant
        // added to `Suspended` cannot then compile without an answer.
        let (result, interactive) = match suspended {
            Suspended::EditUnit(unit) => (self.system.edit(&unit), true),
            Suspended::Nix(op) => {
                let result = match &self.declarative {
                    Some(port) => port.run(&op),
                    // Unreachable as things stand: `with_declarative` asserts
                    // that the registry and the port agree, so a host without
                    // one has no Nix view and no key that could queue this.
                    // An `Err` rather than a panic anyway, on the reasoning
                    // `ops::spawn` gives for its own unreachable guard - the
                    // cost of being wrong is masys aborting with the terminal
                    // already handed away, and a future caller that queues an
                    // operation from somewhere else should surface as a
                    // refusal on the status line.
                    None => Err(MasysError::Platform("this host has no declarative service".to_string())),
                };
                (result, false)
            }
        };
        let aftermath = if interactive && result.is_ok() { Aftermath::Seen } else { Aftermath::Unseen };
        self.report(result);
        self.rebuild();
        aftermath
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

    /// The generation under the cursor, and which profile it came from.
    ///
    /// The profile as well as the generation, because an id alone does not
    /// identify one: this host's system profile is at generation 438 and
    /// its home profile at 47, and both numbering schemes start at 1. Every
    /// operation on a generation needs to know whose it is.
    fn selected_generation(&self) -> Option<(&Generation, ProfileKind)> {
        match self.rows.get(self.cursor()) {
            Some(Node::Generation { generation, kind, .. }) => Some((generation, *kind)),
            _ => None,
        }
    }

    /// The store path a profile is pointing at now.
    ///
    /// Read from the facts rather than from the rows: the current
    /// generation's row can be folded away or filtered out while the
    /// cursor sits on an old one, and "against current" must not change
    /// meaning because a section was collapsed.
    ///
    /// `None` where no generation is marked current, and - separately -
    /// where the current one's link could not be resolved. Both are
    /// unknowns rather than answers, and an operation built on one would
    /// name a store path masys never read.
    fn current_store_path(&self, kind: ProfileKind) -> Option<String> {
        let profiles = self.facts.profiles.as_ref()?;
        let profile = profiles.iter().find(|profile| profile.kind == kind)?;
        profile.generations.iter().find(|generation| generation.current)?.store_path.clone()
    }

    /// One profile, from the last good read.
    ///
    /// The whole record rather than just its path, because an operation
    /// needs two things off it: where the profile lives, and whether this
    /// process can write it. Both are the adapter's to report rather than
    /// this crate's to work out - the home profile moved from
    /// `~/.nix-profile` to `~/.local/state/nix/profiles/profile`, and
    /// masys-app performs no I/O at all.
    fn profile(&self, kind: ProfileKind) -> Option<&Profile> {
        self.facts.profiles.as_ref()?.iter().find(|profile| profile.kind == kind)
    }

    /// The operation a key means here, or `None` where the key will not
    /// act - because the row cannot supply the arguments, or because
    /// masys cannot write what the operation writes.
    ///
    /// The one place that turns an intent into arguments, and the same
    /// one the footer asks whether to dim a key - so a key that is offered
    /// is a key that will act, and a key that will not act is marked. Two
    /// answers derived from one function cannot disagree; the version
    /// where the footer had a rule of its own is exactly how a footer
    /// comes to advertise a key that does nothing.
    fn nix_offer(&self, verb: NixVerb) -> NixOffer {
        match verb {
            // A bare `nixos-rebuild switch` is the correct command on a
            // channels host, not a broken one, so the rebuild verbs are
            // live whether or not a flake reference resolves. The adapter
            // appends `--flake <ref>` where one does.
            NixVerb::Rebuild(verb) => NixOffer::Ready(NixOp::Rebuild(verb)),
            NixVerb::Rollback => NixOffer::Ready(NixOp::Rollback),
            // Standalone only. In `Module` home-manager is activated by
            // `nixos-rebuild switch` and there is no `home-manager`
            // binary to run; `Absent` has no home-manager at all, and an
            // unread `home_mode` has not said either - none of the three
            // is a host this can run on.
            NixVerb::HomeSwitch => match self.facts.home_mode {
                Some(HomeMode::Standalone) => NixOffer::Ready(NixOp::HomeSwitch),
                _ => NixOffer::No,
            },
            // The flake-only pair. Asked of the *reference*, not of the
            // paradigm `Inputs::source` reports, because the adapter
            // builds its argv from the reference: marking a row from one
            // fact and running it from another is how a popup comes to
            // mark a row that would have worked.
            NixVerb::FlakeUpdate => self.with_flake(NixOp::FlakeUpdate { input: None }),
            NixVerb::FlakeCheck => self.with_flake(NixOp::FlakeCheck),
            // And its mirror. `nix-channel --update` on a flake host
            // updates channels the configuration does not read.
            NixVerb::ChannelUpdate => self.with_channels(NixOp::ChannelUpdate),
            NixVerb::ChannelRollback => self.with_channels(NixOp::ChannelRollback),
            // Neither has a source but the operator. A query is not a row
            // and an option name is not a reading, so these are the two
            // that would be unreachable without the input sub-step.
            NixVerb::SearchPackages => NixOffer::Asks { prompt: "search nixpkgs" },
            // `nixos-option` reads `<nixos-config>` and there is no flag
            // to hand it one. On a flake host that sets no `nix.nixPath`
            // it fails with *file 'nixos-config' was not found in the Nix
            // search path* before it evaluates anything - measured on the
            // development host, which is exactly that shape.
            NixVerb::OptionValue => match self.facts.nixos_config.as_ref().and_then(|read| read.as_ref()) {
                Some(_) => NixOffer::Asks { prompt: "option" },
                None => NixOffer::No,
            },
            // The generation under the cursor supplies the spec, so this
            // one asks for nothing. The profile still has to be writable
            // - `nix-env` has no elevation flag, the same reason
            // `Activate` checks.
            NixVerb::DeleteHere => {
                let Some((generation, kind)) = self.selected_generation() else { return NixOffer::No };
                let Some(profile) = self.profile(kind) else { return NixOffer::No };
                if profile.writable != Some(true) {
                    return NixOffer::No;
                }
                NixOffer::Ready(NixOp::DeleteGenerations { profile: profile.path.clone(), spec: generation.id.to_string() })
            }
            // The escape hatch: `+5`, `30d` and a list of numbers are all
            // specs and none of them is a row. Offered against the system
            // profile, which is the one a generation row is about.
            NixVerb::DeleteGenerations => match self.profile(ProfileKind::System) {
                Some(profile) if profile.writable == Some(true) => NixOffer::Asks { prompt: "delete generations" },
                _ => NixOffer::No,
            },
            NixVerb::Activate => {
                let Some((generation, kind)) = self.selected_generation() else { return NixOffer::No };
                // System generations only, and not out of caution.
                // `NixOp::Activate`'s second command is the profile's own
                // `bin/switch-to-configuration switch`, and that script
                // exists in a system generation and nowhere else -
                // measured here: `/nix/var/nix/profiles/system/bin` holds
                // exactly that one file, while
                // `~/.local/state/nix/profiles/profile/bin` holds no
                // activation script at all. Offering the key on a home or
                // channels row would switch the profile and then fail on
                // a missing file, leaving it moved with nothing
                // activated. Widening this means teaching
                // `ops::activation_argv` the other profiles' scripts
                // first.
                if kind != ProfileKind::System {
                    return NixOffer::No;
                }
                let Some(profile) = self.profile(kind) else { return NixOffer::No };
                // The profile directory has to be writable, and this is
                // the one place in masys where a privilege is checked
                // ahead of an operation rather than left to the command.
                // Every other privileged path has a mechanism of its own:
                // a unit verb shells out to `systemctl`, which triggers
                // polkit, so an unprivileged masys can be granted the
                // action; `nixos-rebuild` has `--elevate {none,sudo,run0}`
                // and can deploy as a non-root user. `nix-env` has
                // neither - its synopsis carries no such flag - so there
                // is nothing to grant and nothing to ask.
                //
                // Refused rather than attempted, even though this half is
                // safe on its own: an unprivileged `a` fails at
                // `nix-env`, changes nothing, and would cost only a
                // confirmation and an error. It is marked anyway so that
                // `a` and `c` answer the same question the same way, and
                // because a key that has never once worked for the
                // ordinary invocation should say so before it is pressed
                // and not after. Run masys as root and the directory is
                // writable and the key is live.
                if profile.writable != Some(true) {
                    return NixOffer::No;
                }
                NixOffer::Ready(NixOp::Activate { profile: profile.path.clone(), generation: generation.id })
            }
            NixVerb::Diff => {
                let Some((generation, kind)) = self.selected_generation() else { return NixOffer::No };
                // Both paths must be *known*. A dangling generation link
                // carries `None`, and `nix store diff-closures` given one
                // path would either fail or - worse, if the two unknowns
                // were papered over with a default - diff something
                // against itself and report no change.
                let Some(from) = generation.store_path.clone() else { return NixOffer::No };
                let Some(to) = self.current_store_path(kind) else { return NixOffer::No };
                // The row under the cursor is the left-hand side and
                // current is the right, so the output reads forwards:
                // what changed getting from there to here.
                NixOffer::Ready(NixOp::Diff { from, to })
            }
            // This host's own declared retention, never a number masys
            // chose. `nix-gc.service` runs `--delete-older-than 14d`
            // here, and that value is one somebody wrote into their
            // configuration; a built-in default would be masys deleting
            // generations on a number nobody chose, which is the
            // fabrication rule applied to an act instead of to a reading.
            //
            // Both `None`s mean the same thing for this purpose and both
            // dim the key: the outer is "no read has succeeded", the
            // inner is the port's own answer that the gc job declares no
            // retention. Neither is a period to delete generations by.
            NixVerb::Clean => {
                // Every profile, and this one is not caution - it is the
                // difference between a refusal and an irreversible half
                // of an act. nix-collect-garbage(1) for nix 2.34.8 here:
                // "it looks in a few locations, and acts on all profiles
                // it finds there", and "Deleting previous configurations
                // makes rollbacks to them impossible." Run unprivileged
                // on this host it would delete the operator's own
                // home-manager generations - `~/.local/state/nix/profiles`
                // is owned by the operator and world-unwritable - and then
                // fail on the
                // root-owned system profile, reporting a non-zero exit
                // and nothing about what had already gone.
                //
                // `nix-collect-garbage`'s synopsis is `[--delete-old]
                // [-d] [--delete-older-than period] [--max-freed bytes]
                // [--dry-run]`: no elevation flag, so unlike a unit verb
                // there is no polkit action to be granted and unlike
                // `nixos-rebuild` no `--elevate` to pass. Declining to
                // start it is the only lever masys has.
                //
                // `all` over a *non-empty* list. An empty one is
                // vacuously true, and it means masys found no profile to
                // check rather than that every profile passed - the same
                // shape as a fabricated zero, one level up. Nix also
                // finds profiles masys never reports, per-user ones among
                // them; this is an approximation, and it is sound in the
                // direction that matters, because a host where
                // `/nix/var/nix/profiles` is writable is a host running
                // as root, where the rest are too.
                let Some(profiles) = self.facts.profiles.as_ref() else { return NixOffer::No };
                let all_writable = !profiles.is_empty() && profiles.iter().all(|profile| profile.writable == Some(true));
                if !all_writable {
                    return NixOffer::No;
                }
                match self.facts.gc_retention.clone().flatten() {
                    Some(older_than) => NixOffer::Ready(NixOp::Clean { older_than }),
                    None => NixOffer::No,
                }
            }
        }
    }

    /// `Ready(op)` where this host has a flake reference, `No` where it
    /// has none or the read has not come back.
    fn with_flake(&self, op: NixOp) -> NixOffer {
        match self.facts.flake_ref.as_ref().and_then(|read| read.as_ref()) {
            Some(_) => NixOffer::Ready(op),
            None => NixOffer::No,
        }
    }

    /// And its mirror, for the two channel operations.
    ///
    /// Asked of `Inputs::source` rather than of "no flake reference",
    /// because those are not complements: a host can have neither, and
    /// offering `nix-channel --update` on one that has never had a
    /// channel would be a row live for want of a flake rather than for
    /// having channels.
    fn with_channels(&self, op: NixOp) -> NixOffer {
        match self.facts.nix_inputs.as_ref().map(|inputs| &inputs.source) {
            Some(InputSource::Channels) => NixOffer::Ready(op),
            _ => NixOffer::No,
        }
    }

    /// The operation an `Asks` verb means once the operator has typed a
    /// value. Separate from [`App::nix_offer`] only because the value
    /// does not exist when the offer is made; every precondition is still
    /// asked there and nowhere else.
    fn nix_op_typed(&self, verb: NixVerb, typed: &str) -> Option<NixOp> {
        match verb {
            NixVerb::SearchPackages => Some(NixOp::SearchPackages { query: typed.to_string() }),
            NixVerb::OptionValue => Some(NixOp::OptionValue { name: typed.to_string() }),
            NixVerb::DeleteGenerations => {
                let profile = self.profile(ProfileKind::System)?;
                Some(NixOp::DeleteGenerations { profile: profile.path.clone(), spec: typed.to_string() })
            }
            // Every other verb resolved its arguments at offer time.
            _ => None,
        }
    }

    /// Why a profile cannot be written, or `None` where it can.
    fn profile_note(&self, kind: ProfileKind) -> Option<String> {
        match self.profile(kind) {
            Some(profile) if profile.writable == Some(true) => None,
            Some(_) => Some("profile is read-only - run masys as root".to_string()),
            None => Some("no profile read".to_string()),
        }
    }

    /// The same, for `clean`, which acts on every profile it finds rather
    /// than the one under the cursor.
    fn unwritable_note(&self) -> Option<String> {
        let profiles = self.facts.profiles.as_ref()?;
        let all_writable = !profiles.is_empty() && profiles.iter().all(|profile| profile.writable == Some(true));
        (!all_writable).then(|| "every profile must be writable - run masys as root".to_string())
    }

    /// Runs what the key names, or does nothing where the row under the
    /// cursor cannot supply the arguments.
    ///
    /// Returning early on the wrong row is `ask_kill`'s shape - `k` on a
    /// group row resolves and then quietly does nothing - and the footer
    /// dims the key in exactly that case, so quiet is not silent.
    ///
    /// What changes the machine asks first, through the same `Pending`
    /// and `ModalView::Confirm` the ten unit verbs and both signals
    /// already use. What only reads suspends straight from the keypress:
    /// a confirmation on an operation that changes nothing teaches the
    /// operator that confirmations are noise, and the next one they
    /// dismiss will be a rebuild.
    fn nix(&mut self, verb: NixVerb) -> Flow {
        let op = match self.nix_offer(verb) {
            NixOffer::Ready(op) => op,
            // The row is live and the value is what is missing, so the
            // input sub-step opens rather than the operation running.
            NixOffer::Asks { prompt } => {
                self.input = Some(InputState { verb, prompt, typed: String::new() });
                return Flow::Continue;
            }
            NixOffer::No => return Flow::Continue,
        };
        self.run_nix(op)
    }

    /// Confirms an operation, or suspends straight into it.
    fn run_nix(&mut self, op: NixOp) -> Flow {
        if only_reads(&op) {
            self.suspended = Some(Suspended::Nix(op));
            return Flow::Suspend;
        }
        // An operation with no sentence is not offered. Unreachable while
        // `NixVerb` names three - both of the ones that change something
        // have one below - and the fail-safe direction if a fourth
        // arrives without one: refusing costs a keypress, while
        // approving an act nobody could describe costs a machine.
        let Some(what) = prompt_for(&op) else { return Flow::Continue };
        self.confirm_prompt = format!("{what}  y / n");
        self.pending = Some(Pending::Nix { op });
        Flow::Continue
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
    /// Every guard here asks one question: will this key act on the row
    /// the cursor is on. A unit verb needs a unit under it, needs the
    /// manifest not to own that unit where the verb writes to boot - the
    /// judgement is per *unit*, not per host, because a hand-placed unit
    /// on a declarative machine is genuinely imperative - and, for reset
    /// failed, needs a failure. A Nix key needs `nix_op` to be able to
    /// build its operation - the row supplying the arguments, and the
    /// profile being writable where the operation writes one - which is
    /// asked by calling `nix_op` rather than by restating it.
    fn dim_unavailable(&self, actions: Vec<KeyBinding>) -> Vec<KeyBinding> {
        let on_a_unit = self.selected_unit().is_some();
        let declarative = self
            .selected_unit()
            .and_then(|unit| self.platform.unit_ownership(&unit).ok())
            .is_some_and(|ownership| matches!(ownership, Ownership::Declarative { .. }));
        let failed = self.selected_unit_failed();

        actions
            .into_iter()
            .map(|binding| match self.keymap.action_for(self.buffer, &binding.chord) {
                // Three reasons a unit verb will not do what it says.
                // The row under the cursor is not a unit at all, which is
                // most of the Nix view: these keys resolve there because
                // that view has a units section, but the cursor spends
                // its time on generations and inputs, where `ask_unit`
                // returns without asking anything. Or the manifest owns
                // the unit. Or the verb applies only to a failure that has
                // not happened. All three are marked rather than removed.
                Some(Action::Unit(verb)) => {
                    let dimmed = !on_a_unit || (verb.is_persistent() && declarative) || (verb.needs_failure() && !failed);
                    KeyBinding { dimmed, ..binding }
                }
                // One question for a Nix key, and it is the same one the
                // handler asks: can `nix_op` build the operation here.
                // Asking it rather than restating its conditions is what
                // keeps the footer from claiming a key the handler will
                // refuse - and it is why the privilege check needed no
                // second home in the footer.
                Some(Action::Nix(verb)) => KeyBinding { dimmed: self.nix_offer(verb) == NixOffer::No, ..binding },
                // And `l`, which needs a unit whose log to open. Asked of
                // `log_target` rather than of `selected_unit` because
                // they differ on a timer row: `l` there opens the log of
                // the unit the timer *runs*, so the key applies where the
                // other question would have said it did not.
                Some(Action::Logs) => KeyBinding { dimmed: self.log_target().is_none(), ..binding },
                // The transient opens on a unit row and nowhere else yet,
                // so off one it is dim - and only for that reason. It
                // does *not* dim because the popup's own rows are dim:
                // the ownership guard marks `enable` inside the popup,
                // and a key that refused to open a popup whose rows are
                // marked would hide the very explanation the mark exists
                // to give.
                Some(Action::Transient) => KeyBinding { dimmed: !on_a_unit, ..binding },
                _ => binding,
            })
            .collect()
    }

    /// Every group currently in the buffer, for collapse-all.
    fn group_names(&self) -> HashSet<String> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Node::ProcGroup { name, .. } => Some(name.clone()),
                // The same two questions `cycle_section` asks, asked of
                // the same two methods, so collapse-all and one section's
                // `TAB` cannot disagree about what a section is or about
                // what it is filed under.
                Node::SectionHeader { title, kind, .. } if kind.folds() => Some(kind.fold_key(title)),
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
        if let Some(state) = &self.input {
            return Some(ModalView::Input { prompt: state.prompt, typed: &state.typed, candidates: None });
        }
        if let Some(def) = &self.transient {
            return Some(def.view());
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
                Buffer::Nix => Header::Titled("Nix"),
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
