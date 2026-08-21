//! Thin wrappers over `SystemService`'s runtime-operation methods - one
//! function per action, mirroring `wagit_domain::branch`'s pattern so a
//! transient's callback is always `fn(&dyn SystemService, ...) -> Result<(),
//! MasysError>` with nothing extra to unwrap.
//!
//! `enable`/`disable`/`reset_failed` are on the port but have no wrapper
//! here, because they are not called the same way: the app consults
//! `PlatformService::unit_ownership` first and dispatches them from its
//! own confirmation, so a bare `fn(&dyn SystemService, &str)` would skip
//! the guard that is the whole reason they were deferred. `mask` is
//! still not on the port at all.

use crate::error::MasysError;
use crate::service::{IoNiceClass, Signal, SystemService};

pub fn start(service: &dyn SystemService, unit: &str) -> Result<(), MasysError> {
    service.start(unit)
}

pub fn stop(service: &dyn SystemService, unit: &str) -> Result<(), MasysError> {
    service.stop(unit)
}

pub fn restart(service: &dyn SystemService, unit: &str) -> Result<(), MasysError> {
    service.restart(unit)
}

pub fn reload(service: &dyn SystemService, unit: &str) -> Result<(), MasysError> {
    service.reload(unit)
}

pub fn kill(service: &dyn SystemService, pid: u32, signal: Signal) -> Result<(), MasysError> {
    service.kill(pid, signal)
}

pub fn renice(service: &dyn SystemService, pid: u32, value: i32) -> Result<(), MasysError> {
    service.renice(pid, value)
}

pub fn ionice(service: &dyn SystemService, pid: u32, class: IoNiceClass, level: i32) -> Result<(), MasysError> {
    service.ionice(pid, class, level)
}
