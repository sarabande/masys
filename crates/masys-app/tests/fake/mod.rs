#![allow(dead_code)]

//! Stub `SystemService`/`PlatformService` doubles - unlike
//! `masys-domain/tests/fake` (which *records* calls to verify wiring),
//! `App::tick` *reads* facts to make decisions, so these just hand back
//! whatever the test configured. Methods `tick()` never calls stay
//! `unimplemented!()`, same convention as the domain crate's fake.

use masys_domain::error::MasysError;
use masys_domain::journal::Entry;
use masys_domain::platform::{BootPressure, Ownership, Package, PendingReboot, PlatformId, UpdateStatus};
use masys_domain::sample::Snapshot;
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
        Ok(self.units.clone())
    }
    /// Whatever a test queued as "arrived since you last looked",
    /// drained once - the shape of a real follow, which answers `None`
    /// almost every time it is asked.
    fn follow_unit_journal(&self, _unit: &str, _lines: usize) -> Result<Option<Vec<Entry>>, MasysError> {
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
        Ok(self.journal.iter().filter(|e| e.unit.as_deref() == Some(unit)).cloned().collect())
    }

    /// Whatever the test configured, or nothing. A unit's detail is
    /// read on demand, so most tests never look at it.
    fn unit_detail(&self, unit: &str) -> Result<masys_domain::unit::UnitDetail, MasysError> {
        Ok(masys_domain::unit::UnitDetail { description: Some(format!("{unit} description")), ..Default::default() })
    }
    fn proc_detail(&self, pid: u32) -> Result<masys_domain::proc_detail::ProcDetail, MasysError> {
        self.detail_queries.borrow_mut().push(pid);
        Ok(self.proc_details.get(&pid).cloned().unwrap_or_default())
    }
    fn start(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions yet")
    }
    fn stop(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions yet")
    }
    fn restart(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions yet")
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
        self.calls.borrow_mut().push(format!("kill {pid} {signal:?}"));
        self.outcome()
    }
    fn renice(&self, pid: u32, value: i32) -> Result<(), MasysError> {
        self.calls.borrow_mut().push(format!("renice {pid} {value}"));
        self.outcome()
    }
    fn ionice(&self, _pid: u32, _class: IoNiceClass, _level: i32) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions yet")
    }
}

pub struct FakePlatformService {
    pub boot_pressure: Option<BootPressure>,
    pub pending_reboot: Option<PendingReboot>,
}

impl PlatformService for FakePlatformService {
    fn id(&self) -> PlatformId {
        PlatformId::Unsupported
    }
    fn packages(&self) -> Result<Vec<Package>, MasysError> {
        unimplemented!("no App method reads packages yet")
    }
    fn updates(&self) -> Result<UpdateStatus, MasysError> {
        unimplemented!("no App method reads updates yet")
    }
    fn pending_reboot(&self) -> Result<Option<PendingReboot>, MasysError> {
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
        Ok(Ownership::Imperative)
    }
    fn boot_pressure(&self) -> Result<Option<BootPressure>, MasysError> {
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
