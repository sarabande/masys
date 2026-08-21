//! The keymap: a base map shared by every buffer, plus per-buffer
//! overlays, and the self-description both the footer and the `?` help
//! are built from.
//!
//! The design's one test-enforced invariant lives here: **an overlay may
//! not shadow a protected key**. Without that rule per-buffer keymaps are
//! what make a TUI unlearnable - and it is not a theoretical concern, the
//! design's own first draft had the Procs buffer bind `g` to grouping,
//! shadowing the global refresh. `resolve` enforces it at lookup time
//! rather than trusting the tables to be written correctly, so a future
//! overlay cannot reintroduce the bug by being careless.
//!
//! The protected set is *derived* from what is bound rather than being a
//! hardcoded list of letters. The design defines it as "the movement
//! keys, `q`, `?`, `g`, and `b`" - and which keys are the movement keys
//! is a layout choice. Hardcoding `j`/`k` would protect nothing on a
//! layout that does not use them, and would keep protecting them after
//! they stopped meaning anything.

use std::collections::{HashMap, HashSet};

use masys_view::{KeyBinding, KeyGroup};

use crate::buffer::{Buffer, Registry};
use crate::key::{Key, KeyCode};
use masys_domain::declarative::RebuildVerb;
use masys_domain::service::Signal;

/// Re-exported so `crate::procs` and `crate::app` name one type, while
/// the header that displays it lives in masys-view.
pub use masys_view::ProcSort as Sort;

/// What a key does. Named for the verb rather than the binding, so a
/// config-selected layout changes the table and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    MoveDown,
    MoveUp,
    PageDown,
    PageUp,
    MoveTop,
    MoveBottom,
    /// Cycle the section under the cursor through its levels - magit's
    /// `TAB`. Hidden, then its rows, then its rows opened, then hidden.
    Cycle,
    /// Cycle the whole buffer through the same levels - magit's `S-TAB`.
    /// Ignores the cursor: that is what makes it the global one.
    CycleAll,
    /// Open or close the detail of the *row* under the cursor. Global for
    /// the same reason `Cycle` is: "show me what is inside this" is one
    /// act, whichever view is asking.
    ToggleDetail,
    /// Quit, from any view. It used to bury back to Status first, which
    /// failed the rule this keymap is built on: a global must mean the
    /// same thing everywhere, and "go back, except here, where I exit" is
    /// two meanings on one key.
    Quit,
    /// Force a sample now rather than waiting for the tick.
    Refresh,
    /// Open or close the key help.
    Help,
    /// Jump straight to a buffer - and only to one this host has, which
    /// is what `crate::buffer::Registry` decides. Bound to a digit rather
    /// than to a letter; that module's doc says what the letters cost.
    Open(Buffer),

    // Per-buffer actions below. These are the ones an overlay binds, and
    // the ones the protected set exists to keep from shadowing anything.
    /// Procs: order by a named column.
    SortBy(Sort),
    /// Filter the open buffer. `/` because htop, lnav, k9s, less and vim
    /// all use it - a key people already know beats a key we chose.
    Filter,
    /// Procs: signal the process under the cursor. `k` follows htop,
    /// btop and top.
    Kill(Signal),
    /// Procs: nudge niceness by one, htop's `[` and `]`. Immediate rather
    /// than prompted, because the common case is one step and htop taught
    /// everyone that it is free to try.
    Nice(i32),
    /// Jump to the next or previous section heading. wagit binds `n`/`p`
    /// for the same thing, and magit before it.
    NextSection,
    PrevSection,

    /// Units: a systemctl verb against the unit under the cursor.
    Unit(UnitVerb),
    /// Nix: an operation against the row under the cursor.
    Nix(NixVerb),
    /// Open the transient for the row under the cursor.
    ///
    /// One key across every buffer, which is what the base map's own rule
    /// demands of a global: "open the menu for this thing" must not be
    /// `e` in one buffer and something else in another. It cost
    /// `unit_enable` its top-level key - enable now lives inside the
    /// popup, which is where the design's mockup already drew it, and
    /// where its ownership guard can dim it with a reason attached.
    Transient,
    /// Units: show this unit's log. `l` follows k9s.
    Logs,
    /// Log: read the other way round - newest first, or oldest first.
    ///
    /// One key rather than a sort-by-column key, because a log has
    /// exactly one column worth ordering by and offering a choice of
    /// columns would be answering a question nobody asked.
    ToggleOrder,
    /// Procs: open the systemd view at the unit that owns the row under
    /// the cursor.
    ///
    /// The Procs buffer already groups by cgroup, and on a systemd host a
    /// cgroup *is* a unit - so the association exists in the data and was
    /// simply not reachable. `esc` comes back, the same way it does after
    /// `l`, which is what makes this a drill-down rather than a one-way
    /// jump.
    GoToUnit,
}

/// The operations the Nix buffer offers on a key of its own.
///
/// Not `masys_domain::declarative::NixOp`, and the difference is the
/// point: a `NixOp` carries the arguments the command needs - a store
/// path, a profile, a retention - and every one of those comes from the
/// row the cursor is on or from what the last tick read. The keymap can
/// see neither. So this is the *intent*, `Action` stays `Copy` and
/// table-sized, and `app.rs` is the one place that turns an intent into
/// an operation with arguments in it.
///
/// The full set, now that the transient exists to hold it. It was three
/// while `a`, `d` and `c` were the only way in - a key each for thirteen
/// operations would have spent the letter namespace on the ones nobody
/// reaches for - and those three top-level keys are gone: every one of
/// these is a row in the popup `e` opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NixVerb {
    /// `nixos-rebuild <verb>`, with `--flake <ref>` where one resolves.
    Rebuild(RebuildVerb),
    /// `nixos-rebuild switch --rollback` - the previous generation,
    /// naming none. Distinct from [`NixVerb::Activate`], which names the
    /// one under the cursor.
    Rollback,
    /// `home-manager switch`, in standalone mode only.
    HomeSwitch,
    /// `nix-env --delete-generations` for the generation under the
    /// cursor. The row supplies the spec, so this one takes no input -
    /// unlike [`NixVerb::DeleteGenerations`], which is the escape hatch
    /// and asks for one.
    DeleteHere,
    /// `nix-env --delete-generations <spec>`, the spec typed. `+5`, `30d`
    /// and a list of numbers are all specs, and none of them is a row.
    DeleteGenerations,
    /// `nix flake update`, every input.
    FlakeUpdate,
    /// `nix flake check`.
    FlakeCheck,
    /// `nix-channel --update`, and its rollback.
    ChannelUpdate,
    ChannelRollback,
    /// `nix search nixpkgs <query>`, the query typed.
    SearchPackages,
    /// `nixos-option <name>`, the name typed.
    OptionValue,
    /// Switch the profile to the generation under the cursor and run its
    /// activation script - `NixOp::Activate`.
    ///
    /// Named for what it does rather than "rollback", which is what the
    /// design's table called it. The cursor can sit on a generation
    /// *newer* than current - after booting an older one from the
    /// bootloader, every row above it is - and calling that a rollback
    /// would be a footer telling the operator the opposite of what the
    /// key is about to do. `NixOp::Rollback` is also a different
    /// operation: `nixos-rebuild switch --rollback` goes one step back
    /// and names no generation at all.
    Activate,
    /// `nix store diff-closures` between the generation under the cursor
    /// and the one the profile currently points at.
    Diff,
    /// `nix-collect-garbage --delete-older-than <the host's own policy>`.
    Clean,
}

impl NixVerb {
    /// What a confirmation calls this. Long enough to name the act
    /// without the operator having to remember which key they pressed.
    pub fn label(self) -> &'static str {
        match self {
            NixVerb::Rebuild(RebuildVerb::Switch) => "build, activate, and make it the boot default",
            NixVerb::Rebuild(RebuildVerb::Boot) => "build and make it the boot default",
            NixVerb::Rebuild(RebuildVerb::Test) => "build and activate, without touching the boot default",
            NixVerb::Rebuild(RebuildVerb::Build) => "build only",
            NixVerb::Rebuild(RebuildVerb::DryActivate) => "print what activating would change",
            NixVerb::Rollback => "activate the previous generation",
            NixVerb::HomeSwitch => "build and activate the home-manager configuration",
            NixVerb::Activate => "activate this generation",
            NixVerb::Diff => "diff against current",
            NixVerb::DeleteHere => "delete this generation",
            NixVerb::DeleteGenerations => "delete the generations matching a spec",
            NixVerb::FlakeUpdate => "update every flake input",
            NixVerb::FlakeCheck => "evaluate the flake's checks",
            NixVerb::ChannelUpdate => "update every channel",
            NixVerb::ChannelRollback => "roll the channels back",
            NixVerb::SearchPackages => "search nixpkgs",
            NixVerb::OptionValue => "read this host's value for an option",
            NixVerb::Clean => "delete old generations, collect the store",
        }
    }

    /// The popup row's version - a word or two, because a transient reads
    /// as a list and a sentence per row would not.
    pub fn short_label(self) -> &'static str {
        match self {
            NixVerb::Rebuild(RebuildVerb::Switch) => "switch",
            NixVerb::Rebuild(RebuildVerb::Boot) => "boot",
            NixVerb::Rebuild(RebuildVerb::Test) => "test",
            NixVerb::Rebuild(RebuildVerb::Build) => "build",
            NixVerb::Rebuild(RebuildVerb::DryActivate) => "dry-activate",
            NixVerb::Rollback => "rollback",
            NixVerb::HomeSwitch => "switch",
            NixVerb::Activate => "activate",
            NixVerb::Diff => "diff",
            NixVerb::DeleteHere => "delete",
            NixVerb::DeleteGenerations => "delete generations",
            NixVerb::FlakeUpdate => "flake update",
            NixVerb::FlakeCheck => "flake check",
            NixVerb::ChannelUpdate => "channel update",
            NixVerb::ChannelRollback => "channel rollback",
            NixVerb::SearchPackages => "packages",
            NixVerb::OptionValue => "option value",
            NixVerb::Clean => "clean",
        }
    }
}

/// The systemctl verbs the Units buffer offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitVerb {
    Start,
    Stop,
    Restart,
    /// Restart, but only if the unit is already running. The safe form
    /// for picking up a new config: `restart` would start whatever
    /// somebody had deliberately stopped, and this leaves it stopped.
    TryRestart,
    /// Re-read the unit's configuration without dropping what it is
    /// serving. On the port since the domain crate landed and unbound
    /// until now, which made it unreachable.
    Reload,
    Enable,
    Disable,
    /// A symlink to `/dev/null`: the unit becomes unstartable by
    /// anything, its own dependencies included. Stronger than disable,
    /// which only takes it off the boot path.
    Mask,
    Unmask,
    /// Clears a failed state. Not a lifecycle verb - nothing starts or
    /// stops - which is why it is the one that applies only to a unit
    /// that has actually failed.
    ResetFailed,
    /// `systemctl edit` - the unit's drop-in override, in `$EDITOR`.
    Edit,
}

impl UnitVerb {
    pub fn label(self) -> &'static str {
        match self {
            UnitVerb::Start => "start",
            UnitVerb::Stop => "stop",
            UnitVerb::Restart => "restart",
            UnitVerb::TryRestart => "try-restart",
            UnitVerb::Reload => "reload",
            UnitVerb::Enable => "enable",
            UnitVerb::Disable => "disable",
            UnitVerb::Mask => "mask",
            UnitVerb::Unmask => "unmask",
            UnitVerb::ResetFailed => "reset failed",
            UnitVerb::Edit => "edit",
        }
    }

    /// Whether the verb changes what happens at *boot* rather than now.
    ///
    /// Only these two consult the persistence-ownership guard: starting a
    /// unit on a declaratively managed host is perfectly normal and
    /// nothing reverts it, while enabling one either fails or is undone
    /// by the next rebuild.
    pub fn is_persistent(self) -> bool {
        // Edit belongs here: `systemctl edit` writes a drop-in under
        // `/etc/systemd/system`, which on a declaratively managed host is
        // either read-only or rewritten by the next rebuild. Finding that
        // out *after* the editor has opened is exactly the surprise the
        // guard exists to prevent.
        // Mask and unmask belong here for the same reason enable and
        // disable do, and more plainly: both write a symlink under
        // `/etc/systemd/system`, which a declaratively managed host
        // either refuses outright or rewrites on the next rebuild.
        matches!(self, UnitVerb::Enable | UnitVerb::Disable | UnitVerb::Edit | UnitVerb::Mask | UnitVerb::Unmask)
    }

    /// Whether the verb only makes sense on a unit that has failed.
    pub fn needs_failure(self) -> bool {
        matches!(self, UnitVerb::ResetFailed)
    }
}

impl Action {
    fn is_movement(&self) -> bool {
        matches!(self, Action::MoveDown | Action::MoveUp | Action::PageDown | Action::PageUp | Action::MoveTop | Action::MoveBottom)
    }

    fn label(&self) -> &'static str {
        match self {
            Action::MoveDown => "down",
            Action::MoveUp => "up",
            Action::PageDown => "page down",
            Action::PageUp => "page up",
            Action::MoveTop => "first row",
            Action::MoveBottom => "last row",
            Action::Cycle => "cycle this section",
            Action::CycleAll => "cycle every section",
            Action::ToggleDetail => "open / close this row",
            Action::Quit => "quit",
            Action::Refresh => "refresh now",
            Action::Help => "these keys",
            Action::Transient => "actions here",
            // Labelled per buffer by `describe`, which knows the title.
            Action::Open(_) => "buffer",
            Action::SortBy(sort) => match sort {
                Sort::Cpu => "sort by cpu",
                Sort::Memory => "sort by memory",
                Sort::Name => "sort by name",
            },
            Action::Filter => "filter",
            Action::NextSection => "next section",
            Action::PrevSection => "previous section",
            Action::Kill(Signal::Kill) => "kill (SIGKILL)",
            Action::Kill(_) => "terminate (SIGTERM)",
            Action::Nice(n) if *n < 0 => "raise priority",
            Action::Nice(_) => "lower priority",
            Action::Unit(verb) => verb.label(),
            Action::Nix(verb) => verb.label(),
            Action::Logs => "this unit's log",
            Action::ToggleOrder => "oldest / newest first",
            Action::GoToUnit => "this row's unit",
        }
    }
}

/// A footer-length label.
///
/// The help has room for a sentence; one row of a terminal does not. The
/// long form truncated at 100 columns and silently lost the rightmost
/// keys - which were the action keys, the same failure the help popup had.
fn short_label(action: &Action) -> &'static str {
    match action {
        Action::SortBy(Sort::Cpu) => "cpu",
        Action::SortBy(Sort::Memory) => "mem",
        Action::SortBy(Sort::Name) => "name",
        Action::Kill(Signal::Kill) => "kill -9",
        Action::Kill(_) => "term",
        Action::Nice(n) if *n < 0 => "nice-",
        Action::Nice(_) => "nice+",
        Action::Unit(verb) => verb.label(),
        Action::Nix(verb) => verb.short_label(),
        Action::Logs => "logs",
        Action::ToggleOrder => "order",
        Action::GoToUnit => "unit",
        Action::Filter => "filter",
        other => other.label(),
    }
}

#[derive(Debug, Clone)]
pub struct Keymap {
    base: Vec<(Key, Action)>,
    overlays: HashMap<&'static str, Vec<(Key, Action)>>,
    protected: HashSet<Key>,
    /// Which views this host has. Held here rather than beside the keymap
    /// because every consumer of it is a method on this type: the digit
    /// that opens a view, the footer that offers it, and the `?` help that
    /// lists it. A registry the caller passed in at each of those three
    /// call sites would be three chances to pass the wrong one - and the
    /// static catalogue would still be in scope as the easy wrong answer.
    /// Owning it makes the wrong answer unwritable: `Keymap` has no other
    /// source of buffers.
    registry: Registry,
}

/// The bindings every layout shares. Movement *letters* are the only thing
/// a layout preset changes; arrows and the global verbs are not a layout
/// choice.
/// The default binding for every action, by name.
///
/// One table rather than several: this is what a config file overrides,
/// what the help lists, and what the defaults are - keeping them separate
/// is how a keymap grows a binding that can be configured but never
/// fires, or defaulted but never named.
pub const DEFAULT_BINDINGS: &[(&str, &str)] = &[
    // Movement is on arrows and named keys; which *letters* mean
    // movement is still unsettled, and the letter namespace has been
    // spent on buffers, sorts and actions.
    ("move_down", "down"),
    ("move_up", "up"),
    ("page_down", "pgdn"),
    ("page_up", "pgup"),
    ("move_top", "home"),
    ("move_bottom", "end"),
    // magit's and wagit's binding for the same thing.
    ("next_section", "n"),
    ("prev_section", "p"),
    ("cycle", "tab"),
    ("cycle_all", "shift+tab"),
    ("toggle_detail", "enter"),
    ("quit", "q"),
    ("refresh", "g"),
    ("help", "?"),
    ("filter", "/"),
    // Digits: a view jump is a menu, not navigation within a view, so it
    // has no claim on the letters the views need.
    ("view_status", "1"),
    ("view_procs", "2"),
    ("view_systemd", "3"),
    ("view_io", "4"),
    ("view_nix", "5"),
    // Procs. `a` for alphabetical, `n` having gone to sections.
    ("sort_cpu", "c"),
    ("sort_memory", "m"),
    ("sort_name", "a"),
    ("terminate", "k"),
    ("kill", "K"),
    ("nice_down", "]"),
    ("nice_up", "["),
    // Services
    ("unit_restart", "r"),
    // `alt+r` next to `r`: the same verb, held back. `R` for reload,
    // which is neither a restart nor spelled like one.
    ("unit_try_restart", "alt+r"),
    ("unit_reload", "R"),
    ("unit_start", "S"),
    ("unit_stop", "X"),
    ("unit_disable", "D"),
    ("unit_reset_failed", "F"),
    ("unit_edit", "E"),
    // `M` and `U` after edit: the three keys that write under
    // `/etc/systemd/system`, and the three the ownership guard dims
    // together.
    ("unit_mask", "M"),
    ("unit_unmask", "U"),
    ("unit_logs", "l"),
    // `e` was `unit_enable` until the transient engine landed. Enable is
    // reachable from the popup, and the popup needs one key everywhere.
    ("transient", "e"),
    // Nix has no top-level keys of its own. `a`, `d` and `c` were
    // activate, diff and clean while the transient did not exist - Phase
    // 1 said they were temporary and this is where they go. All eighteen
    // operations are rows in the popup `e` opens, which is what the
    // design drew and what stops the Nix buffer from spending three
    // letters on three of its thirteen operations.
    // `u` for unit. A drill-down out of Procs, so it lives with the
    // Procs keys rather than the systemd ones.
    ("go_to_unit", "u"),
    // Log. `o` for order, and the view's only key of its own.
    ("log_order", "o"),
];

/// Which views an action belongs to.
///
/// A slice rather than one buffer because a unit verb is meaningful in any
/// view that shows units, and two now do: the systemd view and the Nix
/// view's own units section. Filing it under one was what made the Nix
/// section's stated justification - that `s`, `r` and `l` work on it
/// through machinery that already exists - untrue in practice: `owner`
/// sent every unit verb to `Buffer::Systemd` alone, so the same key that
/// restarted a unit from the systemd view did nothing to the identical row
/// shown in the Nix view.
///
/// Empty means global.
fn owner(action: Action) -> &'static [Buffer] {
    match action {
        Action::SortBy(_) | Action::Kill(_) | Action::Nice(_) | Action::GoToUnit => &[Buffer::Procs],
        // Every verb, not a hand-picked subset: `app.rs`'s `selected_unit`
        // reads `Node::Unit`/`Node::Timer` off the row under the cursor with
        // no branch on which buffer it came from, and `nix_buffer.rs` builds
        // the Nix units section out of that same `Node::Unit`. So
        // `nix-daemon.service` is the identical row whether reached from
        // there or from the systemd view, and giving it fewer verbs in one
        // of the two would make one row answer to different keys depending
        // which buffer happened to be showing it - not a smaller keymap, an
        // inconsistent one.
        Action::Unit(_) | Action::Logs | Action::Transient => &[Buffer::Systemd, Buffer::Nix],
        // The Nix view alone. A generation row exists nowhere else, and
        // the store-wide operations are what that view is about.
        Action::Nix(_) => &[Buffer::Nix],
        Action::ToggleOrder => &[Buffer::Log],
        _ => &[],
    }
}

/// Whether this host can reach the action at all.
///
/// Two ways it cannot: the jump names a view the host does not have, or
/// the action lives *in* one. The second is not the same question as
/// [`owner`] - `owner` says which views an action belongs to on a host
/// that has them, and this says whether the host has them.
///
/// The Nix verbs are the only actions in the second class today, because
/// `Buffer::Nix` is the only optional view. Without the arm they were
/// built into a `Buffer::Nix` overlay on every host, Debian included,
/// with no user-visible effect only because nothing can put the cursor in
/// a buffer that is not registered. `Registry` is the one owner of which
/// views exist, and an overlay for a view this host does not have is a
/// second, quieter answer to that question sitting where the first one
/// should be - the same shape as the footer that advertised `[5] nix` on
/// a host where `5` did nothing. Removing a bug's habitat beats fixing
/// the bug.
fn reachable(registry: &Registry, action: Action) -> bool {
    match action {
        Action::Open(buffer) => registry.contains(buffer),
        Action::Nix(_) => registry.contains(Buffer::Nix),
        _ => true,
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self::for_registry(Registry::default())
    }
}

impl Keymap {
    /// The keymap of a host with `registry`'s views and no config file.
    pub fn for_registry(registry: Registry) -> Self {
        Self::with_overrides_for(registry, &[]).0
    }

    /// The keymap of a host with no declarative service, with `overrides`
    /// applied.
    ///
    /// Kept as its own name because it is what a caller with no registry
    /// to offer should get: `Registry::default()` is the host that has the
    /// views every Linux box has, which is the safe answer to give someone
    /// who has not asked the question yet.
    pub fn with_overrides(overrides: &[(String, String)]) -> (Self, Vec<String>) {
        Self::with_overrides_for(Registry::default(), overrides)
    }

    /// Builds the keymap for a host from the defaults, with `overrides`
    /// applied by action name.
    ///
    /// Returns the map and any override that could not be used, so the
    /// caller can tell the operator rather than silently ignoring a line
    /// of their config. A binding that fails to parse leaves the default
    /// in place: a typo should cost you the customisation, not the key.
    pub fn with_overrides_for(registry: Registry, overrides: &[(String, String)]) -> (Self, Vec<String>) {
        let mut problems = Vec::new();
        // A view this host does not have is dropped here rather than at
        // the end, so nothing is reported as having "lost" a key to a
        // binding that was never going to exist.
        let mut bindings: Vec<(&'static str, String)> = DEFAULT_BINDINGS
            .iter()
            .filter(|(name, _)| Action::from_name(name).is_none_or(|action| reachable(&registry, action)))
            .map(|(name, key)| (*name, (*key).to_string()))
            .collect();

        for (name, spelling) in overrides {
            let Some((known, _)) = ACTIONS.iter().find(|(known, _)| known == name) else {
                problems.push(format!("no action called `{name}`"));
                continue;
            };
            if Key::parse(spelling).is_none() {
                problems.push(format!("`{spelling}` is not a key masys understands (for `{name}`)"));
                continue;
            }
            match bindings.iter_mut().find(|(n, _)| n == known) {
                Some(slot) => slot.1 = spelling.clone(),
                None => bindings.push((known, spelling.clone())),
            }
        }

        // An override onto a key some default already holds would
        // otherwise lose to it, because `resolve` takes the first match -
        // so rebinding `help` to `g` silently did nothing at all. The
        // explicit choice wins and the displaced default is dropped and
        // reported, because two actions on one key means one of them is
        // unreachable and picking it by table order is the worst way to
        // decide which.
        let chosen: Vec<&str> = overrides.iter().map(|(name, _)| name.as_str()).collect();
        let mut displaced = Vec::new();
        for (name, spelling) in bindings.clone() {
            if chosen.contains(&name) {
                continue;
            }
            if let Some((winner, _)) =
                bindings.iter().find(|(other, other_key)| *other != name && other_key == &spelling && chosen.contains(other))
            {
                problems.push(format!("`{name}` lost `{spelling}` to `{winner}` and is now unbound"));
                displaced.push(name);
            }
        }
        bindings.retain(|(name, _)| !displaced.contains(name));

        let mut base: Vec<(Key, Action)> = Vec::new();
        let mut overlays: HashMap<&'static str, Vec<(Key, Action)>> = HashMap::new();
        for (name, spelling) in &bindings {
            let (Some(action), Some(key)) = (Action::from_name(name), Key::parse(spelling)) else {
                continue;
            };
            // And again here, because an override can name an action the
            // defaults were filtered of. Silently, rather than reported: a
            // config shared between a NixOS box and a Debian one is not
            // wrong for mentioning the Nix view, it is just describing a
            // view this host does not have.
            if !reachable(&registry, action) {
                continue;
            }
            match owner(action) {
                [] => base.push((key, action)),
                buffers => {
                    for buffer in buffers {
                        overlays.entry(buffer.title()).or_default().push((key, action));
                    }
                }
            }
        }

        // Every global, and nothing else. `base` holds exactly the actions
        // `owner` assigns to no view, which is the same set as "means the
        // same thing in every view" - so the protected set is not a
        // judgement made here, it is a restatement of that table.
        //
        // The old version filtered by action kind and so left `/` and
        // `tab` unprotected: both were global in behaviour, and an overlay
        // could quietly take either.
        let protected = base.iter().map(|(key, _)| *key).collect();

        (Keymap { base, overlays, protected, registry }, problems)
    }

    /// The views this host has.
    ///
    /// Exposed so a caller that needs the same answer - which buffer a
    /// digit reaches, whether a buffer may be opened at all - reads the
    /// one the keymap was built from rather than constructing a second
    /// registry that could differ from it.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Which key currently runs `action`, spelled as the footer and the
    /// help print it.
    ///
    /// The reason both of those go through here rather than through a
    /// table of their own: a chord written down anywhere but the keymap
    /// is a chord that keeps its old value after a rebinding, and the
    /// footer is where most keys are read from. `None` for an action
    /// nothing is bound to, so a displaced binding disappears from the
    /// offer instead of being advertised at a key that does something
    /// else now.
    pub fn chord_for(&self, action: Action) -> Option<String> {
        self.base.iter().chain(self.overlays.values().flatten()).find(|(_, bound)| *bound == action).map(|(key, _)| key.spelling())
    }

    pub fn is_protected(&self, key: Key) -> bool {
        self.protected.contains(&key)
    }

    /// What `key` means in `buffer`.
    ///
    /// The overlay is consulted first, *except* for protected keys, which
    /// always resolve from the base map. Enforcing it here rather than
    /// when the tables are built is deliberate: a table can be written
    /// wrongly, a lookup cannot.
    pub fn resolve(&self, buffer: Buffer, key: Key) -> Option<Action> {
        if !self.is_protected(key)
            && let Some(overlay) = self.overlays.get(buffer.title())
            && let Some((_, action)) = overlay.iter().find(|(k, _)| *k == key)
        {
            return Some(*action);
        }
        self.base.iter().find(|(k, _)| *k == key).map(|(_, action)| *action)
    }

    /// What `key` means with no buffer in play - used by the shadowing
    /// test to state what a protected key must keep meaning.
    pub fn base_action(&self, key: Key) -> Option<Action> {
        self.base.iter().find(|(k, _)| *k == key).map(|(_, action)| *action)
    }

    /// The buffer a key opens, if any.
    ///
    /// Kept as its own lookup rather than folded into `resolve` so the
    /// registry stays the single source of which letter reaches which
    /// buffer - `resolve` answers "what does this key do", this answers
    /// "which buffer is that".
    pub fn resolve_buffer(&self, key: Key) -> Option<Buffer> {
        let KeyCode::Char(c) = key.code else { return None };
        self.registry.by_key(c)
    }

    /// The footer's offer: how to reach every other buffer, then the
    /// verbs that work everywhere. Built from this host's registry rather
    /// than written out, so a new buffer appears in the footer by
    /// existing - and a buffer this host does not have never does.
    pub fn hints(&self, buffer: Buffer) -> Vec<KeyBinding> {
        let mut hints: Vec<KeyBinding> = self
            .registry
            .specs()
            .iter()
            .filter(|spec| spec.key.is_some())
            .filter_map(|spec| {
                let mut hint = self.binding(Action::Open(spec.buffer), &spec.title.to_lowercase())?;
                hint.active = spec.buffer == buffer;
                Some(hint)
            })
            .collect();
        hints.extend(
            [self.binding(Action::Refresh, "refresh"), self.binding(Action::Help, "keys"), self.binding(Action::Quit, "quit")]
                .into_iter()
                .flatten(),
        );
        hints
    }

    /// One footer entry, or nothing when the action has no key.
    fn binding(&self, action: Action, label: &str) -> Option<KeyBinding> {
        Some(KeyBinding { chord: self.chord_for(action)?, label: label.to_string(), dimmed: false, active: false })
    }

    /// Which action a chord runs, for the caller to decide whether it is
    /// currently applicable.
    pub fn action_for(&self, buffer: Buffer, chord: &str) -> Option<Action> {
        self.overlays.get(buffer.title())?.iter().find(|(key, _)| key.spelling() == chord).map(|(_, action)| *action)
    }

    /// The open buffer's own keys, for the footer's second row.
    pub fn actions(&self, buffer: Buffer) -> Vec<KeyBinding> {
        let mut actions: Vec<KeyBinding> = self
            .overlays
            .get(buffer.title())
            .map(|overlay| {
                overlay
                    .iter()
                    .map(|(key, action)| KeyBinding {
                        chord: key.spelling(),
                        label: short_label(action).to_string(),
                        dimmed: false,
                        active: false,
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Filtering is a base binding, but it is an action rather than
        // navigation and belongs where the eye looks for actions.
        actions.extend(self.binding(Action::Filter, "filter"));
        actions
    }

    /// Every binding available in `buffer`, grouped for the `?` help.
    pub fn describe(&self, buffer: Buffer) -> Vec<KeyGroup> {
        let bindings = |wanted: fn(&Action) -> bool| -> Vec<KeyBinding> {
            self.base
                .iter()
                .filter(|(_, action)| wanted(action))
                .map(|(key, action)| KeyBinding { chord: key.spelling(), label: action.label().to_string(), dimmed: false, active: false })
                .collect()
        };

        let mut groups = vec![
            KeyGroup { heading: "Movement".to_string(), bindings: bindings(|a| a.is_movement()) },
            // Everything else the base map binds, rather than a list of
            // the four variants that existed when this was written -
            // `/`, `n` and `p` were all added afterwards and none of
            // them ever appeared in the help.
            KeyGroup { heading: "Global".to_string(), bindings: bindings(|a| !a.is_movement() && !matches!(a, Action::Open(_))) },
            KeyGroup {
                heading: "Buffers".to_string(),
                // This host's registry, for the same reason the footer
                // uses it: the help is where a key is looked up when it
                // did not work, so listing one that cannot is the worst
                // possible place to be wrong.
                bindings: self
                    .registry
                    .specs()
                    .iter()
                    .filter_map(|spec| self.binding(Action::Open(spec.buffer), &spec.title.to_lowercase()))
                    .collect(),
            },
        ];

        // The open buffer's own section, last and titled after it, so it
        // is obvious which keys follow you between screens and which do
        // not.
        if let Some(overlay) = self.overlays.get(buffer.title())
            && !overlay.is_empty()
        {
            groups.push(KeyGroup {
                heading: format!("{} only", buffer.title()),
                bindings: overlay
                    .iter()
                    .map(|(key, action)| KeyBinding {
                        chord: key.spelling(),
                        label: action.label().to_string(),
                        dimmed: false,
                        active: false,
                    })
                    .collect(),
            });
        }
        groups
    }
}

/// Every action, by the name a config file uses for it.
///
/// One table, so the defaults below and a user's overrides resolve
/// against exactly the same set - a name that can be configured but not
/// defaulted, or the reverse, is how a keymap ends up with bindings
/// nothing can reach.
pub const ACTIONS: &[(&str, Action)] = &[
    ("move_down", Action::MoveDown),
    ("move_up", Action::MoveUp),
    ("page_down", Action::PageDown),
    ("page_up", Action::PageUp),
    ("move_top", Action::MoveTop),
    ("move_bottom", Action::MoveBottom),
    ("next_section", Action::NextSection),
    ("prev_section", Action::PrevSection),
    ("cycle", Action::Cycle),
    ("cycle_all", Action::CycleAll),
    ("toggle_detail", Action::ToggleDetail),
    ("go_to_unit", Action::GoToUnit),
    ("quit", Action::Quit),
    ("refresh", Action::Refresh),
    ("help", Action::Help),
    ("filter", Action::Filter),
    ("view_status", Action::Open(Buffer::Status)),
    ("view_procs", Action::Open(Buffer::Procs)),
    ("view_systemd", Action::Open(Buffer::Systemd)),
    ("view_io", Action::Open(Buffer::Io)),
    ("view_nix", Action::Open(Buffer::Nix)),
    ("sort_cpu", Action::SortBy(Sort::Cpu)),
    ("sort_memory", Action::SortBy(Sort::Memory)),
    ("sort_name", Action::SortBy(Sort::Name)),
    ("terminate", Action::Kill(Signal::Term)),
    ("kill", Action::Kill(Signal::Kill)),
    ("nice_down", Action::Nice(1)),
    ("nice_up", Action::Nice(-1)),
    ("unit_start", Action::Unit(UnitVerb::Start)),
    ("unit_stop", Action::Unit(UnitVerb::Stop)),
    ("unit_restart", Action::Unit(UnitVerb::Restart)),
    ("unit_try_restart", Action::Unit(UnitVerb::TryRestart)),
    ("unit_reload", Action::Unit(UnitVerb::Reload)),
    ("unit_enable", Action::Unit(UnitVerb::Enable)),
    ("transient", Action::Transient),
    ("unit_disable", Action::Unit(UnitVerb::Disable)),
    ("unit_reset_failed", Action::Unit(UnitVerb::ResetFailed)),
    ("unit_edit", Action::Unit(UnitVerb::Edit)),
    ("unit_mask", Action::Unit(UnitVerb::Mask)),
    ("unit_unmask", Action::Unit(UnitVerb::Unmask)),
    ("unit_logs", Action::Logs),
    ("nix_rebuild_switch", Action::Nix(NixVerb::Rebuild(RebuildVerb::Switch))),
    ("nix_rebuild_boot", Action::Nix(NixVerb::Rebuild(RebuildVerb::Boot))),
    ("nix_rebuild_test", Action::Nix(NixVerb::Rebuild(RebuildVerb::Test))),
    ("nix_rebuild_build", Action::Nix(NixVerb::Rebuild(RebuildVerb::Build))),
    ("nix_dry_activate", Action::Nix(NixVerb::Rebuild(RebuildVerb::DryActivate))),
    ("nix_rollback", Action::Nix(NixVerb::Rollback)),
    ("nix_home_switch", Action::Nix(NixVerb::HomeSwitch)),
    ("nix_activate", Action::Nix(NixVerb::Activate)),
    ("nix_diff", Action::Nix(NixVerb::Diff)),
    ("nix_delete_here", Action::Nix(NixVerb::DeleteHere)),
    ("nix_delete_generations", Action::Nix(NixVerb::DeleteGenerations)),
    ("nix_flake_update", Action::Nix(NixVerb::FlakeUpdate)),
    ("nix_flake_check", Action::Nix(NixVerb::FlakeCheck)),
    ("nix_channel_update", Action::Nix(NixVerb::ChannelUpdate)),
    ("nix_channel_rollback", Action::Nix(NixVerb::ChannelRollback)),
    ("nix_search_packages", Action::Nix(NixVerb::SearchPackages)),
    ("nix_option_value", Action::Nix(NixVerb::OptionValue)),
    ("nix_clean", Action::Nix(NixVerb::Clean)),
    ("log_order", Action::ToggleOrder),
];

impl Action {
    /// The config-file name for this action, if it has one.
    pub fn name(&self) -> Option<&'static str> {
        ACTIONS.iter().find(|(_, action)| action == self).map(|(name, _)| *name)
    }

    /// The action a config file means by `name`.
    pub fn from_name(name: &str) -> Option<Action> {
        ACTIONS.iter().find(|(known, _)| *known == name).map(|(_, action)| *action)
    }
}
