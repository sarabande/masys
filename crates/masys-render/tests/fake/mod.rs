#![allow(dead_code)]
// Identical to masys-app/tests/fake/mod.rs - each crate with integration
// tests against these ports carries its own copy, since Cargo test
// binaries can't share a tests/ helper across crates (see
// wagit-render/tests/support/mod.rs's identical note). Keep them in sync
// by hand.

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
    /// Successive snapshots for tests that need two samples - rate
    /// derivation cannot be exercised with a service that answers with the
    /// same counters forever. `RefCell` because `sample` takes `&self`.
    pub queued: std::cell::RefCell<Vec<Snapshot>>,
}

impl FakeSystemService {
    /// Hands back each snapshot in turn, repeating the last one once the
    /// queue runs dry.
    pub fn returning(snapshots: Vec<Snapshot>) -> Self {
        let first = snapshots.first().expect("at least one snapshot").clone();
        Self { snapshot: first, units: Vec::new(), queued: std::cell::RefCell::new(snapshots) }
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
        rss_bytes: (100 - pid as u64) * 1024 * 1024,
        io_read_bytes: 0,
        io_write_bytes: 0,
        state: masys_domain::sample::ProcState::Sleeping,
        nice: 0,
        oom_score: 0,
        threads: 1,
        started_at_ms: 0,
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
    fn unit_journal(&self, _unit: &str, _lines: usize) -> Result<Vec<Entry>, MasysError> {
        // `App::tick` now asks why each failed unit failed, so every fake
        // a tick runs against needs an answer. Empty means "nothing was
        // logged", which renders exactly as it did before reasons existed.
        Ok(Vec::new())
    }

    /// Whatever the test configured, or nothing. A unit's detail is
    /// read on demand, so most tests never look at it.
    fn unit_detail(&self, unit: &str) -> Result<masys_domain::unit::UnitDetail, MasysError> {
        Ok(masys_domain::unit::UnitDetail { description: Some(format!("{unit} description")), ..Default::default() })
    }
    fn proc_detail(&self, pid: u32) -> Result<masys_domain::proc_detail::ProcDetail, MasysError> {
        Ok(masys_domain::proc_detail::ProcDetail { cmdline: Some(format!("/usr/bin/fake --pid {pid}")), ..Default::default() })
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
    fn reload(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions yet")
    }
    fn reset_failed(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no method under test resets a failed unit")
    }

    fn try_restart(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions in this test")
    }

    fn enable(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions in this test")
    }

    fn mask(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions in this test")
    }

    fn unmask(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions in this test")
    }

    fn disable(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions in this test")
    }

    fn edit(&self, _unit: &str) -> Result<(), MasysError> {
        Ok(())
    }
    fn kill(&self, _pid: u32, _signal: Signal) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions yet")
    }
    fn renice(&self, _pid: u32, _value: i32) -> Result<(), MasysError> {
        unimplemented!("no App method dispatches unit actions yet")
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

/// A scanner that never finds anything: no render test opens a
/// filesystem row.
#[derive(Default)]
pub struct NoScanner;

impl masys_domain::scan::DirScanner for NoScanner {
    fn start(&self, _root: &std::path::Path) {}
    fn poll(&self) -> masys_domain::scan::ScanProgress {
        masys_domain::scan::ScanProgress::default()
    }
    fn cancel(&self) {}
}
