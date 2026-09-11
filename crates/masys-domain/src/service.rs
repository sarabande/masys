use crate::error::MasysError;
use crate::journal::{Entry, Priority};
use crate::platform::{BootPressure, Ownership, Package, PendingReboot, PlatformId};
use crate::sample::Snapshot;
use crate::unit::Unit;

/// A signal deliverable to a process - the subset `kill` actually offers,
/// not every signal `libc` knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
    Hup,
    Int,
    Usr1,
    Usr2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoNiceClass {
    RealTime,
    BestEffort,
    Idle,
}

/// Local-host system facts and mutations: unit state, `/proc`, journal,
/// and the runtime operations masys performs. One implementation in v1
/// (`masys-systemd`, a later plan), reached through this trait so
/// masys-domain's use cases and tests never touch D-Bus, `systemctl`, or
/// `/proc` directly.
pub trait SystemService {
    /// Raw cumulative counters for every running process, plus the
    /// system-wide facts (PSI, filesystems, clock, OOM kills, degraded
    /// rollup) a triage pass and a rate derivation both need. No rates:
    /// see `crate::rate::derive`.
    fn sample(&self) -> Result<Snapshot, MasysError>;
    fn units(&self) -> Result<Vec<Unit>, MasysError>;
    /// The last `lines` journal entries belonging to one unit, oldest
    /// first.
    ///
    /// A real field query rather than a text search over what some other
    /// call already loaded. The difference is not cosmetic: journald
    /// attaches `_SYSTEMD_UNIT` from the sender's cgroup, so a process
    /// cannot lie about it, while a text match catches every *mention* of
    /// a unit's name by anything else and misses every line that does not
    /// happen to contain it.
    fn unit_journal(&self, unit: &str, lines: usize) -> Result<Vec<Entry>, MasysError>;
    /// Everything the host logged since `since_ms` at `min_priority` or
    /// worse, oldest first.
    ///
    /// The only read here that names no unit, and the Status buffer's
    /// two journal sections are its only caller. That is not a whole-
    /// system tail returning: the Log buffer dropped one of those
    /// deliberately, because "what happened on this machine" has no row
    /// you can put a cursor on. What comes back here is turned into
    /// findings - a handful of deduplicated rows with a count on each -
    /// and never listed line by line.
    ///
    /// **One read for both sections.** Kernel-origin and userspace lines
    /// are separated by `Entry::origin` above this port, rather than
    /// by asking twice. Two queries would be two windows that drift
    /// apart, and two sections disagreeing about the same instant is
    /// worse than either being slightly stale.
    ///
    /// `since_ms` is Unix milliseconds - the clock `Entry::timestamp_ms`
    /// is on, and the one `App::tick` already carries. Not
    /// `Snapshot::taken_at_ms`, which is monotonic and means nothing to
    /// journald.
    fn journal(&self, since_ms: u64, min_priority: Priority) -> Result<Vec<Entry>, MasysError>;
    /// The properties a unit shows when its row is opened.
    ///
    /// Separate from `units` because the cost is different in kind: this
    /// is several targeted reads for *one* unit, asked for when something
    /// is looking at it, where `units` is one read for all of them on
    /// every tick. Folding these into `units` would multiply the poll by
    /// the number of units on the host.
    /// New entries for `unit`, if any have arrived since the last call.
    ///
    /// The one read that is a *check* rather than a query: it is called
    /// four times a second while a log is open, so it must cost nothing
    /// when nothing has happened. `Ok(None)` means the journal has not
    /// moved.
    ///
    /// Defaults to "cannot follow", which is the honest answer for an
    /// adapter reading through a subprocess: a bounded query per wake
    /// would cost 100 ms and it cannot be told what changed. Such a host
    /// still sees its log refresh on the ordinary tick.
    fn follow_unit_journal(
        &self,
        _unit: &str,
        _lines: usize,
    ) -> Result<Option<Vec<Entry>>, MasysError> {
        Ok(None)
    }

    /// Whether every disk masys can ask reports its SMART self-assessment
    /// as passing.
    ///
    /// **The one read here with no `Result`, and deliberately.** Every way
    /// this can fail is a way of not knowing: `smartctl` is not installed,
    /// the caller is not root, the device is virtual, the disk has no
    /// SMART data. None of those is a fault an operator can act on and
    /// none of them says anything about the disks, so all of them answer
    /// `None` and the overview draws no segment. An `Err` here would put
    /// a row on the Status buffer saying masys could not run a program,
    /// on the majority of hosts, forever.
    ///
    /// `Some(true)` is a claim about *every* disk, so it requires every
    /// disk to have answered. A host where two disks pass and a third
    /// cannot be read answers `None`: "all healthy" would be a reading
    /// nobody took of the one that matters.
    ///
    /// Defaults to `None`, which is the honest answer for an adapter that
    /// has no way to ask.
    fn smart_health(&self) -> Option<bool> {
        None
    }

    fn unit_detail(&self, unit: &str) -> Result<crate::unit::UnitDetail, MasysError>;
    /// The properties a process shows when its row is opened.
    ///
    /// Separate from `sample` for the same reason `unit_detail` is
    /// separate from `units`, and more so: this is a handful of targeted
    /// reads for *one* pid - several of them per-descriptor - where
    /// `sample` is one sweep for every process on the host, every tick.
    ///
    /// Most of what it reads is gated by the kernel on same-uid or
    /// `CAP_SYS_PTRACE`, so an unprivileged masys gets a mostly-empty
    /// `ProcDetail` for other users' processes. That is reported as
    /// absent fields rather than as an error: the row is still worth
    /// opening for what `Proc` alone carries.
    fn proc_detail(&self, pid: u32) -> Result<crate::proc_detail::ProcDetail, MasysError>;
    fn start(&self, unit: &str) -> Result<(), MasysError>;
    fn stop(&self, unit: &str) -> Result<(), MasysError>;
    fn restart(&self, unit: &str) -> Result<(), MasysError>;
    fn reload(&self, unit: &str) -> Result<(), MasysError>;
    /// Restart a unit *only if it is already running*.
    ///
    /// The difference from `restart` is what happens to a stopped unit:
    /// `restart` starts it, `try-restart` leaves it stopped. That makes
    /// this the safe form for "pick up the new config" across a set of
    /// units, where `restart` would silently start whatever somebody had
    /// deliberately shut down.
    fn try_restart(&self, unit: &str) -> Result<(), MasysError>;
    /// Clear a unit's failed state.
    ///
    /// systemd keeps a unit `failed` until something resets it, which is
    /// what makes `is-system-running` report `degraded` hours after a
    /// transient fault. Nothing is started, stopped or changed on disk -
    /// only the marker is dropped, along with the restart-rate limiting
    /// that came with it.
    fn reset_failed(&self, unit: &str) -> Result<(), MasysError>;
    /// Enable or disable a unit at boot.
    ///
    /// Deferred until the persistence-ownership guard had a shape: on a
    /// declaratively managed host these either fail outright or are
    /// undone by the next rebuild, and an "enabled" that silently
    /// reverts is worse than no button at all.
    /// `PlatformService::unit_ownership` is what the caller must consult
    /// first.
    fn enable(&self, unit: &str) -> Result<(), MasysError>;
    fn disable(&self, unit: &str) -> Result<(), MasysError>;
    /// Mask or unmask a unit - a symlink to `/dev/null` that makes the
    /// unit unstartable by anything, including its own dependencies.
    ///
    /// Stronger than `disable`, which only removes it from the boot
    /// path: a masked unit refuses a manual `start` too. The symlink
    /// lands under `/etc/systemd/system`, the same place enable and
    /// disable write, so callers consult
    /// `PlatformService::unit_ownership` first for the same reason.
    fn mask(&self, unit: &str) -> Result<(), MasysError>;
    fn unmask(&self, unit: &str) -> Result<(), MasysError>;
    /// `systemctl edit` - opens the unit's drop-in override in `$EDITOR`.
    ///
    /// The one method here that takes over the terminal. Every other
    /// action returns when it is done; this one hands stdin, stdout and
    /// stderr to a program that expects to own the screen, so a caller
    /// drawing a frame must give the terminal up first and take it back
    /// after. `masys_app::Flow::Suspend` is how the session says so.
    ///
    /// Changes what happens at boot, so callers consult
    /// `PlatformService::unit_ownership` first, exactly as they do for
    /// `enable` and `disable`.
    fn edit(&self, unit: &str) -> Result<(), MasysError>;
    fn kill(&self, pid: u32, signal: Signal) -> Result<(), MasysError>;
    /// `setpriority` - `value` is a raw nice value, -20..=19.
    fn renice(&self, pid: u32, value: i32) -> Result<(), MasysError>;
    /// `ioprio_set` - `level` is 0..=7 within `class` (ignored for `Idle`,
    /// which has no levels).
    fn ionice(&self, pid: u32, class: IoNiceClass, level: i32) -> Result<(), MasysError>;
}

/// Distro-specific facts `SystemService` cannot answer on its own: what
/// this host is, whether it wants rebooting, what its generations cost,
/// and whether a runtime change to a unit persists.
///
/// Four methods, and every one of them has a caller. It carried `packages`
/// and `updates` until 2026-08-29, on the argument that "a port is the
/// whole contract or it is a suggestion" - but the only adapter that
/// answers anything refused both with `Err("not implemented")`, no caller
/// asked, and no row drew the result. A contract neither side performs is
/// not a contract, so they were removed rather than kept as a promise
/// (#16). The Packages buffer can bring its own port when it exists.
///
/// The port models *not being able to answer*, so that a host with no
/// adapter of its own is a normal working state rather than an error. That
/// property is carried by the return types below rather than by any
/// implementation: `PlatformId::Unsupported` is a real answer,
/// `unit_ownership` resolves to `Ownership` in words Debian can speak, and
/// `pending_reboot` and `boot_pressure` return `Option` because "nothing
/// is known to be pending" and "I could not find out" must not be the same
/// value. A method returning `generation: u32` would fail that test on
/// sight, which is what keeps NixOS's shape out of this trait.
///
/// Stated of the types deliberately. An adapter that answers nothing
/// cannot fail to satisfy a badly shaped port - it would return `0` as
/// happily as anything else - so the check has to live where it can
/// actually be made.
pub trait PlatformService {
    fn id(&self) -> PlatformId;
    fn pending_reboot(&self) -> Result<Option<PendingReboot>, MasysError>;
    fn unit_ownership(&self, unit: &str) -> Result<Ownership, MasysError>;
    fn boot_pressure(&self) -> Result<Option<BootPressure>, MasysError>;
}

/// What this host has installed.
///
/// **Its own port rather than a method on [`PlatformService`]**, and the
/// difference is which question a host that cannot answer is asked.
/// `PlatformService` is answered by every host - the fallback included -
/// so a `packages()` there would have to return *something*, and the only
/// somethings available are an empty list, which claims this host has no
/// packages, and an `Err`, which is what the NixOS adapter returned for a
/// year while nothing called it.
///
/// An `Option<Box<dyn PackageService>>` asks it once, at selection: either
/// this host has an adapter that can list packages or it has none, and
/// where it has none the Packages buffer is simply absent. That is
/// `DeclarativeService`'s shape and it is here for
/// `DeclarativeService`'s reason - a buffer of dashes pretending to be
/// readings is worse than a buffer that is not there.
pub trait PackageService {
    /// Every package this host has installed, ascending by name.
    ///
    /// Sorted by the adapter rather than the buffer, because the order is
    /// a property of the answer and two adapters sorting differently would
    /// be a difference the operator sees and cannot explain.
    fn packages(&self) -> Result<Vec<Package>, MasysError>;
}
