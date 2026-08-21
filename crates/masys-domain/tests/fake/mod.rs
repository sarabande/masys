#![allow(dead_code)]

//! Recording `SystemService`/`PlatformService` doubles - the domain's use
//! cases take `&dyn SystemService`, so wiring and sequencing can be
//! exercised with no D-Bus, no `systemctl`, no `/proc`, and no real
//! systemd. What `systemctl`/D-Bus/`/proc` actually do lives in
//! `masys-systemd/tests`, in a later plan.

use std::cell::RefCell;

use masys_domain::error::MasysError;
use masys_domain::journal::Entry;
use masys_domain::platform::{BootPressure, Ownership, Package, PendingReboot, PlatformId, UpdateStatus};
use masys_domain::sample::Snapshot;
use masys_domain::service::{IoNiceClass, PlatformService, Signal, SystemService};
use masys_domain::unit::Unit;

#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Start(String),
    Stop(String),
    Restart(String),
    Reload(String),
    Kill { pid: u32, signal: Signal },
    Renice { pid: u32, value: i32 },
    Ionice { pid: u32, class: IoNiceClass, level: i32 },
}

#[derive(Default)]
pub struct FakeSystemService {
    calls: RefCell<Vec<Call>>,
}

impl FakeSystemService {
    pub fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }

    /// The single call recorded, panicking if there wasn't exactly one -
    /// every unit_actions function is one systemd invocation, and
    /// asserting that directly keeps those tests to one line.
    pub fn only_call(&self) -> Call {
        let calls = self.calls();
        assert_eq!(calls.len(), 1, "expected exactly one call, got {calls:#?}");
        calls.into_iter().next().expect("checked above")
    }

    fn record(&self, call: Call) {
        self.calls.borrow_mut().push(call);
    }
}

impl SystemService for FakeSystemService {
    fn sample(&self) -> Result<Snapshot, MasysError> {
        unimplemented!("no unit_actions use case samples")
    }
    fn units(&self) -> Result<Vec<Unit>, MasysError> {
        unimplemented!("no unit_actions use case lists units")
    }
    fn unit_journal(&self, _unit: &str, _lines: usize) -> Result<Vec<Entry>, MasysError> {
        unimplemented!("no method under test reads a unit's journal")
    }

    /// Whatever the test configured, or nothing. A unit's detail is
    /// read on demand, so most tests never look at it.
    fn unit_detail(&self, unit: &str) -> Result<masys_domain::unit::UnitDetail, MasysError> {
        Ok(masys_domain::unit::UnitDetail { description: Some(format!("{unit} description")), ..Default::default() })
    }
    fn proc_detail(&self, pid: u32) -> Result<masys_domain::proc_detail::ProcDetail, MasysError> {
        Ok(masys_domain::proc_detail::ProcDetail { cmdline: Some(format!("/usr/bin/fake --pid {pid}")), ..Default::default() })
    }
    fn start(&self, unit: &str) -> Result<(), MasysError> {
        self.record(Call::Start(unit.to_string()));
        Ok(())
    }
    fn stop(&self, unit: &str) -> Result<(), MasysError> {
        self.record(Call::Stop(unit.to_string()));
        Ok(())
    }
    fn restart(&self, unit: &str) -> Result<(), MasysError> {
        self.record(Call::Restart(unit.to_string()));
        Ok(())
    }
    fn reload(&self, unit: &str) -> Result<(), MasysError> {
        self.record(Call::Reload(unit.to_string()));
        Ok(())
    }
    fn reset_failed(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no method under test resets a failed unit")
    }

    fn try_restart(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no domain use case try-restarts units")
    }

    fn enable(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no domain use case enables units")
    }

    fn mask(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no domain use case masks units")
    }

    fn unmask(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no domain use case unmasks units")
    }

    fn disable(&self, _unit: &str) -> Result<(), MasysError> {
        unimplemented!("no domain use case disables units")
    }

    fn edit(&self, _unit: &str) -> Result<(), MasysError> {
        // No `unit_actions` wrapper: editing is not a plain
        // `fn(&dyn SystemService, &str)` - it needs the terminal, so the
        // session dispatches it from its own suspend path.
        Ok(())
    }
    fn kill(&self, pid: u32, signal: Signal) -> Result<(), MasysError> {
        self.record(Call::Kill { pid, signal });
        Ok(())
    }
    fn renice(&self, pid: u32, value: i32) -> Result<(), MasysError> {
        self.record(Call::Renice { pid, value });
        Ok(())
    }
    fn ionice(&self, pid: u32, class: IoNiceClass, level: i32) -> Result<(), MasysError> {
        self.record(Call::Ionice { pid, class, level });
        Ok(())
    }
}

#[derive(Default)]
pub struct FakePlatformService;

impl PlatformService for FakePlatformService {
    fn id(&self) -> PlatformId {
        PlatformId::Unsupported
    }
    fn packages(&self) -> Result<Vec<Package>, MasysError> {
        Ok(vec![])
    }
    fn updates(&self) -> Result<UpdateStatus, MasysError> {
        Ok(UpdateStatus::default())
    }
    fn pending_reboot(&self) -> Result<Option<PendingReboot>, MasysError> {
        Ok(None)
    }
    fn unit_ownership(&self, _unit: &str) -> Result<Ownership, MasysError> {
        Ok(Ownership::Imperative)
    }
    fn boot_pressure(&self) -> Result<Option<BootPressure>, MasysError> {
        Ok(None)
    }
}
