#![allow(dead_code)]

//! Stub `SystemService`/`PlatformService` doubles - unlike
//! `masys-domain/tests/fake` (which *records* calls to verify wiring),
//! `App::tick` *reads* facts to make decisions, so these just hand back
//! whatever the test configured. Methods `tick()` never calls stay
//! `unimplemented!()`, same convention as the domain crate's fake.

use masys_domain::declarative::{
    DeclarativeService, HomeMode, Inputs, NixOp, Profile, RebootState,
};
use masys_domain::error::MasysError;
use masys_domain::journal::{Entry, Priority};
use masys_domain::platform::{BootPressure, Ownership, PendingReboot, PlatformId};
use masys_domain::sample::{Snapshot, SystemState};
use masys_domain::service::{IoNiceClass, PlatformService, Signal, SystemService};
use masys_domain::unit::Unit;

pub struct FakeSystemService {
    pub snapshot: Snapshot,
    pub units: Vec<Unit>,
    /// Mutations this fake was asked to perform, so a test can assert
    /// that an action reached the port - and, just as importantly, that
    /// a declined confirmation did not.
    ///
    /// Shared with the test rather than read back through `App`: the
    /// session holds a `Box<dyn SystemService>` and cannot reach a
    /// fake-specific method, and giving the real trait a `calls()` for
    /// tests to use would put test machinery in production code.
    pub calls: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    /// What every mutation should return. `None` succeeds.
    pub fails_with: Option<String>,
    /// What `smart_health` should answer. `None` is the ordinary answer
    /// on a host masys cannot ask - no smartctl, no root, no SMART data.
    pub smart: Option<bool>,
    /// How many times `smart_health` was asked. The point of the field:
    /// the read spawns a subprocess per disk, so a test has to be able to
    /// prove it does not happen on every tick.
    pub smart_reads: std::rc::Rc<std::cell::Cell<u32>>,
    pub journal: Vec<Entry>,
    /// What a `follow_unit_journal` call should report as newly arrived,
    /// taken once. `None` is the ordinary answer - a real follow is asked
    /// four times a second and almost always has nothing to say.
    pub appended: std::rc::Rc<std::cell::RefCell<Option<Vec<Entry>>>>,
    /// Every `unit_journal` query this fake was asked for. Separate from
    /// `calls`, which records mutations: the question here is how often a
    /// *read* was repeated, which is what a cache exists to answer.
    /// Shared out through an `Rc` for the same reason `calls` is - the
    /// session owns the fake and a test cannot reach back into it.
    pub journal_queries: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    /// Successive snapshots for tests that need two samples - rate
    /// derivation cannot be exercised with a service that answers with the
    /// same counters forever. `RefCell` because `sample` takes `&self`.
    pub queued: std::cell::RefCell<Vec<Snapshot>>,
    /// What `proc_detail` answers, by pid. A pid with no entry gets the
    /// default - which is the same shape an unprivileged masys gets for
    /// a process it does not own, and therefore worth being the default.
    pub proc_details: std::collections::HashMap<u32, masys_domain::proc_detail::ProcDetail>,
    /// Every `proc_detail` call, so a test can assert the read happens on
    /// the keypress rather than on every tick.
    pub detail_queries: std::rc::Rc<std::cell::RefCell<Vec<u32>>>,
    /// Fails `sample` alone - the one read that still ends a tick, and
    /// so the one whose failure has no row to appear in.
    pub sample_fail_with: Option<String>,
    /// Fails `units` alone. Separate from `fails_with`, which fails
    /// mutations and the journal: a test that wants to know what a
    /// missing unit list does must not also lose the journal.
    pub units_fail_with: Option<String>,
}

impl FakeSystemService {
    /// Hands back each snapshot in turn, repeating the last one once the
    /// queue runs dry.
    pub fn returning(snapshots: Vec<Snapshot>) -> Self {
        let first = snapshots.first().expect("at least one snapshot").clone();
        Self {
            snapshot: first,
            units: Vec::new(),
            calls: Default::default(),
            fails_with: None,
            journal: Vec::new(),
            appended: Default::default(),
            journal_queries: Default::default(),
            queued: std::cell::RefCell::new(snapshots),
            proc_details: Default::default(),
            detail_queries: Default::default(),
            units_fail_with: None,
            sample_fail_with: None,
            smart: None,
            smart_reads: Default::default(),
        }
    }
}

/// One process with everything but pid, comm, and cpu_ticks held constant -
/// rss is derived from the pid so the memory ranking is deterministic.
pub fn proc(pid: u32, comm: &str, cpu_ticks: u64) -> masys_domain::sample::Proc {
    masys_domain::sample::Proc {
        pid,
        comm: comm.to_string(),
        cgroup: None,
        cpu_ticks,
        // Saturating: a test using a realistic pid used to panic here
        // rather than fail, which reads as a bug in the code under test.
        rss_bytes: 100u64.saturating_sub(pid as u64) * 1024 * 1024,
        io_read_bytes: 0,
        io_write_bytes: 0,
        state: masys_domain::sample::ProcState::Sleeping,
        nice: 0,
        oom_score: 0,
        threads: 1,
        started_at_ms: 0,
    }
}

impl FakeSystemService {
    fn outcome(&self) -> Result<(), MasysError> {
        match &self.fails_with {
            Some(message) => Err(MasysError::Command(message.clone())),
            None => Ok(()),
        }
    }
}

impl SystemService for FakeSystemService {
    fn sample(&self) -> Result<Snapshot, MasysError> {
        if let Some(why) = &self.sample_fail_with {
            return Err(MasysError::System(why.clone()));
        }
        let mut queued = self.queued.borrow_mut();
        if queued.is_empty() {
            return Ok(self.snapshot.clone());
        }
        let next = queued.remove(0);
        if queued.is_empty() {
            queued.push(next.clone());
        }
        Ok(next)
    }
    fn units(&self) -> Result<Vec<Unit>, MasysError> {
        match &self.units_fail_with {
            Some(why) => Err(MasysError::Command(why.clone())),
            None => Ok(self.units.clone()),
        }
    }
    /// Whatever a test queued as "arrived since you last looked",
    /// drained once - the shape of a real follow, which answers `None`
    /// almost every time it is asked.
    fn follow_unit_journal(
        &self,
        _unit: &str,
        _lines: usize,
    ) -> Result<Option<Vec<Entry>>, MasysError> {
        Ok(self.appended.borrow_mut().take())
    }

    fn unit_journal(&self, unit: &str, _lines: usize) -> Result<Vec<Entry>, MasysError> {
        self.journal_queries.borrow_mut().push(unit.to_string());
        // `fails_with` fails reads as well as mutations: journalctl
        // exiting non-zero is a real case, and the session has to do
        // something sensible with it.
        self.outcome()?;
        // Field-matched, as the real adapter is: a text search would be
        // the very bug this method exists to fix.
        Ok(self
            .journal
            .iter()
            .filter(|e| e.unit.as_deref() == Some(unit))
            .cloned()
            .collect())
    }

    /// The host-wide read, answered from the same fixture `unit_journal`
    /// serves - one journal, filtered two ways, so a test cannot set up
    /// a host whose sections disagree with its unit logs.
    fn journal(&self, since_ms: u64, min_priority: Priority) -> Result<Vec<Entry>, MasysError> {
        self.outcome()?;
        Ok(self
            .journal
            .iter()
            .filter(|e| e.timestamp_ms >= since_ms)
            .filter(|e| e.priority.at_or_worse_than(min_priority))
            .cloned()
            .collect())
    }

    /// Whatever the test configured, counting the asks so a test can
    /// prove the read is not taken every tick.
    fn smart_health(&self) -> Option<bool> {
        self.smart_reads.set(self.smart_reads.get() + 1);
        self.smart
    }

    /// Whatever the test configured, or nothing. A unit's detail is
    /// read on demand, so most tests never look at it.
    fn unit_detail(&self, unit: &str) -> Result<masys_domain::unit::UnitDetail, MasysError> {
        Ok(masys_domain::unit::UnitDetail {
            description: Some(format!("{unit} description")),
            ..Default::default()
        })
    }
    /// Fails for a pid the sample does not carry, the way the real
    /// adapter does: a dead process has no `/proc/<pid>` to read.
    ///
    /// Modelled off the snapshot rather than off `proc_details`, because
    /// "gone" and "unreadable" are different answers and the session
    /// tells them apart by asking the sample. A pid that *is* in the
    /// sample but has no entry here reads as the second - which is the
    /// ordinary state of another user's process, and what the default
    /// stands for.
    fn proc_detail(&self, pid: u32) -> Result<masys_domain::proc_detail::ProcDetail, MasysError> {
        self.detail_queries.borrow_mut().push(pid);
        if !self.snapshot.procs.iter().any(|proc| proc.pid == pid) {
            return Err(MasysError::Command(format!("no such process: {pid}")));
        }
        Ok(self.proc_details.get(&pid).cloned().unwrap_or_default())
    }
    fn start(&self, unit: &str) -> Result<(), MasysError> {
        self.calls.borrow_mut().push(format!("start {unit}"));
        self.outcome()
    }
    fn stop(&self, unit: &str) -> Result<(), MasysError> {
        self.calls.borrow_mut().push(format!("stop {unit}"));
        self.outcome()
    }
    fn restart(&self, unit: &str) -> Result<(), MasysError> {
        self.calls.borrow_mut().push(format!("restart {unit}"));
        self.outcome()
    }
    fn reload(&self, unit: &str) -> Result<(), MasysError> {
        self.calls.borrow_mut().push(format!("reload {unit}"));
        self.outcome()
    }
    fn try_restart(&self, unit: &str) -> Result<(), MasysError> {
        self.calls.borrow_mut().push(format!("try-restart {unit}"));
        self.outcome()
    }
    fn reset_failed(&self, unit: &str) -> Result<(), MasysError> {
        self.calls.borrow_mut().push(format!("reset-failed {unit}"));
        self.outcome()
    }

    fn enable(&self, unit: &str) -> Result<(), MasysError> {
        self.calls.borrow_mut().push(format!("enable {unit}"));
        self.outcome()
    }

    fn disable(&self, unit: &str) -> Result<(), MasysError> {
        self.calls.borrow_mut().push(format!("disable {unit}"));
        self.outcome()
    }

    fn mask(&self, unit: &str) -> Result<(), MasysError> {
        self.calls.borrow_mut().push(format!("mask {unit}"));
        self.outcome()
    }

    fn unmask(&self, unit: &str) -> Result<(), MasysError> {
        self.calls.borrow_mut().push(format!("unmask {unit}"));
        self.outcome()
    }

    fn edit(&self, unit: &str) -> Result<(), MasysError> {
        self.calls.borrow_mut().push(format!("edit {unit}"));
        self.outcome()
    }
    fn kill(&self, pid: u32, signal: Signal) -> Result<(), MasysError> {
        self.calls
            .borrow_mut()
            .push(format!("kill {pid} {signal:?}"));
        self.outcome()
    }
    fn renice(&self, pid: u32, value: i32) -> Result<(), MasysError> {
        self.calls
            .borrow_mut()
            .push(format!("renice {pid} {value}"));
        self.outcome()
    }
    fn ionice(&self, _pid: u32, _class: IoNiceClass, _level: i32) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions yet")
    }
}

pub struct FakePlatformService {
    pub boot_pressure: Option<BootPressure>,
    pub pending_reboot: Option<PendingReboot>,
    /// What `unit_ownership` answers. `None` makes the read **fail**,
    /// which is a third case and not a third answer: a platform that
    /// could not find out is not one that said "nothing owns this unit".
    ///
    /// The fake used to hardcode `Ok(Imperative)`, so no test could reach
    /// either of the other two - which is how the footer came to flatten
    /// a failed read into "it persists" with a green suite.
    pub ownership: Option<Ownership>,
    /// Fails the pending-reboot read. A third case again, and not a
    /// third answer: a platform that could not be asked has not said
    /// there is no reboot pending.
    pub fails_with: Option<String>,
    /// Fails the boot-pressure read *alone*.
    ///
    /// Separate because the two degrade differently on purpose, and one
    /// switch could not reach that: boot pressure produces no row, since
    /// its absence already means "say nothing about generations", while
    /// an unaskable pending reboot does. A fake that failed both
    /// together made the asymmetry untestable.
    pub boot_fails_with: Option<String>,
}

impl Default for FakePlatformService {
    /// The ordinary host: nothing pending, and units that stay how you
    /// leave them.
    fn default() -> Self {
        FakePlatformService {
            boot_fails_with: None,
            fails_with: None,
            boot_pressure: None,
            pending_reboot: None,
            ownership: Some(Ownership::Imperative),
        }
    }
}

impl PlatformService for FakePlatformService {
    fn id(&self) -> PlatformId {
        PlatformId::Unsupported
    }
    fn pending_reboot(&self) -> Result<Option<PendingReboot>, MasysError> {
        if let Some(why) = &self.fails_with {
            return Err(MasysError::Platform(why.clone()));
        }
        Ok(self.pending_reboot.clone())
    }
    /// `Imperative`, the answer for a host with no platform adapter.
    ///
    /// Not `unimplemented!()`: `App::dim_unavailable` consults this the
    /// moment the cursor lands on a unit row, so a panic here fires on any
    /// test that merely *navigates* to a unit - which reads as a crash
    /// rather than as a failing assertion. A test needing a different
    /// answer supplies its own double.
    fn unit_ownership(&self, _unit: &str) -> Result<Ownership, MasysError> {
        self.ownership
            .clone()
            .ok_or_else(|| MasysError::Platform("could not read who owns this unit".to_string()))
    }
    fn boot_pressure(&self) -> Result<Option<BootPressure>, MasysError> {
        if let Some(why) = &self.boot_fails_with {
            return Err(MasysError::Platform(why.clone()));
        }
        Ok(self.boot_pressure.clone())
    }
}

/// A scanner that never finds anything, for the tests that are not about
/// scanning but must construct an `App`.
#[derive(Default)]
pub struct NoScanner;

impl masys_domain::scan::DirScanner for NoScanner {
    fn start(&self, _root: &std::path::Path) {}
    fn poll(&self) -> masys_domain::scan::ScanProgress {
        masys_domain::scan::ScanProgress::default()
    }
    fn cancel(&self) {}
}

/// A machine with nothing measurable on it, for the tests whose subject
/// is not the sample.
///
/// Written out rather than reached for through a `Default` on `Snapshot`
/// itself. Several of those fields are `Option` precisely so a reading
/// that was never taken cannot be confused with a real one, and deriving
/// `Default` in the domain crate would put `Snapshot::default()` in front
/// of every adapter - where a machine with no processes, no units and no
/// filesystems is the shape of a failed read rather than of a fixture.
/// Here the shape is asked for by name, which is the difference.
pub fn bare_snapshot() -> Snapshot {
    Snapshot {
        taken_at_ms: 0,
        procs: Vec::new(),
        pressure: None,
        filesystems: Vec::new(),
        disks: Vec::new(),
        clock_synced: true,
        utc_offset_secs: 0,
        interfaces: Vec::new(),
        oom_kills: Vec::new(),
        system_state: SystemState::Running,
        clock_ticks_per_sec: 100,
        machine: None,
        load: None,
        uptime_secs: None,
        memory: None,
        cpu_times: None,
        thermal_throttled_ms_by_core: None,
    }
}

/// A `DeclarativeService` that hands back whatever the test configured -
/// the same convention as `FakeSystemService`. Nothing here touches a
/// filesystem, so the Nix view's tests run on any host.
#[derive(Default)]
pub struct FakeDeclarative {
    /// Whether this host has a way to run one command as root.
    ///
    /// `false` by default: the three profile-writing operations were a
    /// dead end before elevation, and a test that says nothing about it
    /// should still describe that host.
    pub can_elevate: bool,
    /// `false` by default, `can_elevate`'s reason: a test that says
    /// nothing about it should still describe an unprivileged host.
    pub is_root: bool,
    pub profiles: Vec<Profile>,
    pub reboot: Option<RebootState>,
    pub inputs: Option<Inputs>,
    pub home_mode: Option<HomeMode>,
    pub running_nixpkgs_rev: Option<String>,
    pub gc_retention: Option<String>,
    pub gc_roots: u32,
    /// Every operation this fake was asked to perform, in order.
    ///
    /// Shared out through an `Rc` for the reason `FakeSystemService::calls`
    /// is: the session holds a `Box<dyn DeclarativeService>` and a test
    /// cannot reach back into the fake it handed over. The question these
    /// answer is the one `Pending` exists for - that an operation reached
    /// the port only after it was confirmed, and never before.
    pub ops: std::rc::Rc<std::cell::RefCell<Vec<NixOp>>>,
    /// While set, every read fails.
    ///
    /// Shared and interior-mutable for the same reason `FakeSystemService`
    /// shares its `calls`: the session owns the fake behind a
    /// `Box<dyn DeclarativeService>` and a test cannot reach back into it.
    /// A read that fails on the *second* tick is the case the session's
    /// keep-last-good rule exists for, and there is no way to stage it
    /// without a switch the test still holds.
    pub failing: std::rc::Rc<std::cell::Cell<bool>>,
    /// This host's flake reference, or `None` for a channels host.
    ///
    /// Defaulted to `None` rather than to a stand-in path: the rows that
    /// consult it are the flake-only ones, and a fake that invented a
    /// reference would leave every test asserting they are live without
    /// any test having said the host has a flake.
    pub flake_ref: Option<String>,
    /// Where `<nixos-config>` resolves, for the one row that needs it.
    /// `None` by default, for `flake_ref`'s reason.
    pub nixos_config: Option<String>,
}

impl FakeDeclarative {
    fn outcome(&self) -> Result<(), MasysError> {
        match self.failing.get() {
            true => Err(MasysError::Platform("declarative read refused".to_string())),
            false => Ok(()),
        }
    }
}

impl DeclarativeService for FakeDeclarative {
    /// Off by default, so every test written before elevation existed
    /// still describes a host where the profile-writing rows are refused.
    fn can_elevate(&self) -> bool {
        self.can_elevate
    }

    /// Off by default, `can_elevate`'s reason: a test that says nothing
    /// about it should still describe an unprivileged host.
    fn is_root(&self) -> bool {
        self.is_root
    }

    fn profiles(&self) -> Result<Vec<Profile>, MasysError> {
        self.outcome()?;
        Ok(self.profiles.clone())
    }
    fn reboot(&self) -> Result<Option<RebootState>, MasysError> {
        self.outcome()?;
        Ok(self.reboot.clone())
    }
    fn flake_ref(&self) -> Result<Option<String>, MasysError> {
        self.outcome()?;
        Ok(self.flake_ref.clone())
    }
    fn nixos_config(&self) -> Result<Option<String>, MasysError> {
        self.outcome()?;
        Ok(self.nixos_config.clone())
    }
    /// `Absent` where the test set nothing, which is the port's own answer
    /// for a host without home-manager rather than a stand-in for one.
    fn home_mode(&self) -> Result<HomeMode, MasysError> {
        self.outcome()?;
        Ok(self.home_mode.unwrap_or(HomeMode::Absent))
    }
    fn inputs(&self) -> Result<Option<Inputs>, MasysError> {
        self.outcome()?;
        Ok(self.inputs.clone())
    }
    fn running_nixpkgs_rev(&self) -> Result<Option<String>, MasysError> {
        self.outcome()?;
        Ok(self.running_nixpkgs_rev.clone())
    }
    fn gc_retention(&self) -> Result<Option<String>, MasysError> {
        self.outcome()?;
        Ok(self.gc_retention.clone())
    }
    fn gc_roots(&self) -> Result<u32, MasysError> {
        self.outcome()?;
        Ok(self.gc_roots)
    }
    /// Records the operation rather than performing it.
    ///
    /// The commands these name activate system configurations and collect
    /// the store, so a fake that ran anything would make `cargo test`
    /// dangerous on the machine running it - the same reason
    /// `masys-platform-nixos`'s own suite asserts on `argv` and never
    /// spawns.
    ///
    /// Fails while `failing` is set, like every read here: a rebuild that
    /// exits non-zero is the ordinary case, and the session has to hold
    /// the failure rather than swallow it.
    fn run(&self, op: &NixOp) -> Result<(), MasysError> {
        self.ops.borrow_mut().push(op.clone());
        self.outcome()
    }
}
