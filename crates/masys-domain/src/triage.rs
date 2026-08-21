use crate::finding::{Finding, PressureResource, Thresholds};
use crate::platform::BootPressure;
use crate::sample::{Filesystem, OomKill, Pressure, Snapshot, SystemState};
use crate::unit::{ActiveState, Unit};

pub fn failed_units(units: &[Unit], now_ms: u64) -> Vec<Finding> {
    units
        .iter()
        .filter(|u| u.active_state == ActiveState::Failed)
        .map(|u| Finding::FailedUnit {
            unit: u.name.clone(),
            exit_code: u.exit_code,
            since_ms: now_ms.saturating_sub(u.since_ms),
            // Filled by the session, which has journal access.
            reason: None,
        })
        .collect()
}

pub fn flapping_units(units: &[Unit], thresholds: &Thresholds, now_ms: u64) -> Vec<Finding> {
    let window_start = now_ms.saturating_sub(thresholds.flapping_window_ms);
    units
        .iter()
        .filter_map(|u| {
            let restarts = u.restart_timestamps_ms.iter().filter(|&&t| t >= window_start && t <= now_ms).count() as u32;
            (restarts >= thresholds.flapping_restart_count).then_some(Finding::FlappingUnit {
                unit: u.name.clone(),
                restarts,
                window_ms: thresholds.flapping_window_ms,
            })
        })
        .collect()
}

pub fn pressure(p: &Pressure, thresholds: &Thresholds) -> Vec<Finding> {
    let mut findings = Vec::new();
    if p.memory_some.avg60 >= thresholds.psi_some_avg60_percent || p.memory_full.avg60 >= thresholds.psi_full_avg60_percent {
        findings.push(Finding::Pressure {
            resource: PressureResource::Memory,
            some_avg60: p.memory_some.avg60,
            full_avg60: Some(p.memory_full.avg60),
        });
    }
    if p.io_some.avg60 >= thresholds.psi_some_avg60_percent || p.io_full.avg60 >= thresholds.psi_full_avg60_percent {
        findings.push(Finding::Pressure { resource: PressureResource::Io, some_avg60: p.io_some.avg60, full_avg60: Some(p.io_full.avg60) });
    }
    if p.cpu_some.avg60 >= thresholds.psi_some_avg60_percent {
        findings.push(Finding::Pressure { resource: PressureResource::Cpu, some_avg60: p.cpu_some.avg60, full_avg60: None });
    }
    findings
}

pub fn disk_capacity(filesystems: &[Filesystem], boot_pressure: Option<&BootPressure>, thresholds: &Thresholds) -> Vec<Finding> {
    filesystems
        .iter()
        .filter(|fs| fs.used_percent >= thresholds.disk_used_percent)
        .map(|fs| Finding::DiskCapacity {
            mount_point: fs.mount_point.clone(),
            used_percent: fs.used_percent,
            free_bytes: fs.free_bytes,
            generations: if fs.mount_point == "/boot" { boot_pressure.map(|b| b.generations) } else { None },
        })
        .collect()
}

pub fn inode_exhaustion(filesystems: &[Filesystem], thresholds: &Thresholds) -> Vec<Finding> {
    filesystems
        .iter()
        .filter(|fs| fs.inode_used_percent >= thresholds.inode_used_percent)
        .map(|fs| Finding::InodeExhaustion { mount_point: fs.mount_point.clone(), inode_used_percent: fs.inode_used_percent })
        .collect()
}

pub fn read_only_filesystems(filesystems: &[Filesystem]) -> Vec<Finding> {
    filesystems.iter().filter(|fs| fs.read_only).map(|fs| Finding::ReadOnlyFilesystem { mount_point: fs.mount_point.clone() }).collect()
}

pub fn clock_unsynchronized(clock_synced: bool) -> Vec<Finding> {
    if clock_synced { Vec::new() } else { vec![Finding::ClockUnsynchronized] }
}

pub fn oom_kills(kills: &[OomKill]) -> Vec<Finding> {
    kills.iter().map(|k| Finding::OomKill { pid: k.pid, comm: k.comm.clone(), timestamp_ms: k.timestamp_ms }).collect()
}

pub fn system_degraded(state: SystemState, units: &[Unit]) -> Vec<Finding> {
    if state != SystemState::Degraded {
        return Vec::new();
    }
    let failed_units = units.iter().filter(|u| u.active_state == ActiveState::Failed).count() as u32;
    vec![Finding::SystemDegraded { failed_units }]
}

/// Every v1 check, concatenated - what the status buffer calls once per
/// tick to build its whole finding list.
pub fn evaluate(
    snapshot: &Snapshot,
    units: &[Unit],
    boot_pressure: Option<&BootPressure>,
    thresholds: &Thresholds,
    now_ms: u64,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(failed_units(units, now_ms));
    findings.extend(flapping_units(units, thresholds, now_ms));
    // No PSI means no pressure findings - not findings that say zero.
    if let Some(psi) = &snapshot.pressure {
        findings.extend(pressure(psi, thresholds));
    }
    findings.extend(disk_capacity(&snapshot.filesystems, boot_pressure, thresholds));
    findings.extend(inode_exhaustion(&snapshot.filesystems, thresholds));
    findings.extend(read_only_filesystems(&snapshot.filesystems));
    findings.extend(clock_unsynchronized(snapshot.clock_synced));
    findings.extend(oom_kills(&snapshot.oom_kills));
    findings.extend(system_degraded(snapshot.system_state, units));
    findings
}
