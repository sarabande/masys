use masys_domain::finding::{Finding, PressureResource, Thresholds};
use masys_domain::platform::BootPressure;
use masys_domain::sample::{Filesystem, OomKill, Pressure, PsiLine, Snapshot, SystemState};
use masys_domain::triage;
use masys_domain::unit::{ActiveState, Unit, UnitKind};

fn unit(name: &str, active_state: ActiveState) -> Unit {
    Unit {
        name: name.to_string(),
        kind: UnitKind::Service,
        active_state,
        sub_state: "running".to_string(),
        exit_code: None,
        enabled: true,
        restart_timestamps_ms: Vec::new(),
        since_ms: 0,
        cgroup: None,
        slice: None,
        triggers: Vec::new(),
        timer: None,
    }
}

fn filesystem(mount_point: &str, used_percent: f32) -> Filesystem {
    Filesystem { mount_point: mount_point.to_string(), used_percent, free_bytes: 1_000_000, inode_used_percent: 10.0, read_only: false }
}

#[test]
fn failed_units_reports_only_failed() {
    let units = vec![unit("ok.service", ActiveState::Active), unit("broken.service", ActiveState::Failed)];
    let findings = triage::failed_units(&units, 10_000);
    assert_eq!(findings.len(), 1);
    assert!(matches!(&findings[0], Finding::FailedUnit { unit, .. } if unit == "broken.service"));
}

#[test]
fn flapping_units_needs_enough_restarts_in_the_window() {
    let thresholds = Thresholds { flapping_restart_count: 3, flapping_window_ms: 3_600_000, ..Thresholds::default() };
    let mut flaky = unit("otel-collector.service", ActiveState::Active);
    flaky.restart_timestamps_ms = vec![100, 200, 300];
    let calm = unit("nginx.service", ActiveState::Active);

    let findings = triage::flapping_units(&[flaky, calm], &thresholds, 3_600_100);
    assert_eq!(findings.len(), 1);
    assert!(matches!(&findings[0], Finding::FlappingUnit { unit, restarts: 3, .. } if unit == "otel-collector.service"));
}

#[test]
fn flapping_units_ignores_restarts_outside_the_window() {
    let thresholds = Thresholds { flapping_restart_count: 3, flapping_window_ms: 1000, ..Thresholds::default() };
    let mut u = unit("otel-collector.service", ActiveState::Active);
    u.restart_timestamps_ms = vec![0, 1, 2];
    let findings = triage::flapping_units(&[u], &thresholds, 10_000);
    assert!(findings.is_empty(), "restarts at t=0..2 are outside a 1000ms window ending at t=10000");
}

#[test]
fn pressure_flags_resources_over_the_some_threshold() {
    let thresholds = Thresholds { psi_some_avg60_percent: 20.0, psi_full_avg60_percent: 5.0, ..Thresholds::default() };
    let p = Pressure {
        cpu_some: PsiLine { avg60: 5.0, ..Default::default() },
        io_some: PsiLine { avg60: 22.7, ..Default::default() },
        io_full: PsiLine::default(),
        memory_some: PsiLine { avg60: 41.2, ..Default::default() },
        memory_full: PsiLine { avg60: 8.1, ..Default::default() },
    };
    let findings = triage::pressure(&p, &thresholds);
    assert_eq!(findings.len(), 2, "cpu stays under threshold, io and memory don't: {findings:#?}");
    assert!(findings.iter().any(|f| matches!(f, Finding::Pressure { resource: PressureResource::Io, .. })));
    assert!(findings.iter().any(|f| matches!(f, Finding::Pressure { resource: PressureResource::Memory, .. })));
}

#[test]
fn pressure_below_every_threshold_reports_nothing() {
    let findings = triage::pressure(&Pressure::default(), &Thresholds::default());
    assert!(findings.is_empty());
}

#[test]
fn disk_capacity_flags_filesystems_over_threshold_and_attaches_boot_generations() {
    let thresholds = Thresholds { disk_used_percent: 85.0, ..Thresholds::default() };
    let filesystems = vec![filesystem("/nix", 91.0), filesystem("/boot", 88.0), filesystem("/home", 40.0)];
    let boot_pressure = BootPressure { generations: 9, reclaimable_bytes: 500_000_000 };

    let findings = triage::disk_capacity(&filesystems, Some(&boot_pressure), &thresholds);

    assert_eq!(findings.len(), 2, "/home is under threshold: {findings:#?}");
    let boot = findings
        .iter()
        .find(|f| matches!(f, Finding::DiskCapacity { mount_point, .. } if mount_point == "/boot"))
        .expect("a /boot finding");
    assert!(matches!(boot, Finding::DiskCapacity { generations: Some(9), .. }));
    let nix =
        findings.iter().find(|f| matches!(f, Finding::DiskCapacity { mount_point, .. } if mount_point == "/nix")).expect("a /nix finding");
    assert!(matches!(nix, Finding::DiskCapacity { generations: None, .. }), "generations only attaches to /boot");
}

#[test]
fn inode_exhaustion_is_independent_of_disk_percent() {
    let thresholds = Thresholds { inode_used_percent: 90.0, ..Thresholds::default() };
    let mut fs = filesystem("/var", 40.0);
    fs.inode_used_percent = 95.0;
    let findings = triage::inode_exhaustion(&[fs], &thresholds);
    assert_eq!(findings.len(), 1);
}

#[test]
fn read_only_filesystems_are_reported() {
    let mut fs = filesystem("/", 50.0);
    fs.read_only = true;
    let findings = triage::read_only_filesystems(&[fs]);
    assert_eq!(findings, vec![Finding::ReadOnlyFilesystem { mount_point: "/".to_string() }]);
}

#[test]
fn clock_unsynchronized_fires_only_when_unsynced() {
    assert!(triage::clock_unsynchronized(true).is_empty());
    assert_eq!(triage::clock_unsynchronized(false), vec![Finding::ClockUnsynchronized]);
}

#[test]
fn oom_kills_are_reported_one_finding_each() {
    let kills = vec![OomKill { pid: 8944, comm: "firefox".to_string(), timestamp_ms: 5_000 }];
    let findings = triage::oom_kills(&kills);
    assert_eq!(findings, vec![Finding::OomKill { pid: 8944, comm: "firefox".to_string(), timestamp_ms: 5_000 }]);
}

#[test]
fn system_degraded_counts_the_failed_units_causing_it() {
    let units =
        vec![unit("a.service", ActiveState::Failed), unit("b.service", ActiveState::Active), unit("c.service", ActiveState::Failed)];
    let findings = triage::system_degraded(SystemState::Degraded, &units);
    assert_eq!(findings, vec![Finding::SystemDegraded { failed_units: 2 }]);
}

#[test]
fn system_degraded_is_silent_when_running() {
    assert!(triage::system_degraded(SystemState::Running, &[]).is_empty());
}

#[test]
fn evaluate_aggregates_every_check() {
    let units = vec![unit("broken.service", ActiveState::Failed)];
    let snapshot = Snapshot {
        taken_at_ms: 0,
        procs: Vec::new(),
        pressure: Some(Pressure::default()),
        filesystems: vec![filesystem("/boot", 95.0)],
        disks: Vec::new(),
        clock_synced: false,
        utc_offset_secs: -21_600,
        interfaces: Vec::new(),
        oom_kills: vec![OomKill { pid: 1, comm: "x".to_string(), timestamp_ms: 0 }],
        system_state: SystemState::Degraded,
        clock_ticks_per_sec: 100,
        machine: None,
        load: None,
        uptime_secs: None,
        memory: None,
    };
    let thresholds = Thresholds::default();

    let findings = triage::evaluate(&snapshot, &units, None, &thresholds, 0);

    assert!(findings.iter().any(|f| matches!(f, Finding::FailedUnit { .. })));
    assert!(findings.iter().any(|f| matches!(f, Finding::DiskCapacity { .. })));
    assert!(findings.iter().any(|f| matches!(f, Finding::ClockUnsynchronized)));
    assert!(findings.iter().any(|f| matches!(f, Finding::OomKill { .. })));
    assert!(findings.iter().any(|f| matches!(f, Finding::SystemDegraded { .. })));
}

#[test]
fn pressure_orders_resources_memory_io_cpu() {
    let thresholds = Thresholds { psi_some_avg60_percent: 1.0, psi_full_avg60_percent: 1.0, ..Thresholds::default() };
    let p = Pressure {
        cpu_some: PsiLine { avg60: 12.0, ..Default::default() },
        io_some: PsiLine { avg60: 22.7, ..Default::default() },
        io_full: PsiLine::default(),
        memory_some: PsiLine { avg60: 41.2, ..Default::default() },
        memory_full: PsiLine { avg60: 8.1, ..Default::default() },
    };
    let resources: Vec<PressureResource> = triage::pressure(&p, &thresholds)
        .iter()
        .map(|f| match f {
            Finding::Pressure { resource, .. } => *resource,
            other => panic!("pressure emitted a non-Pressure finding: {other:?}"),
        })
        .collect();
    // The mockup lists memory above io; cpu last, being the least
    // predictive of the machine becoming unusable.
    assert_eq!(resources, vec![PressureResource::Memory, PressureResource::Io, PressureResource::Cpu]);
}

/// PSI needs kernel 4.20+ with `CONFIG_PSI=y`, and some kernels still
/// want `psi=1` on the command line. Defaulting the missing case to
/// zeroes made such a host look perfectly idle - the most dangerous wrong
/// answer a triage pass can give, because it is silence that reads as
/// health.
#[test]
fn a_kernel_without_psi_reports_no_pressure_findings_rather_than_zero_ones() {
    let thresholds = Thresholds { psi_some_avg60_percent: 20.0, ..Thresholds::default() };

    let mut snapshot = Snapshot {
        taken_at_ms: 0,
        procs: Vec::new(),
        pressure: None,
        filesystems: Vec::new(),
        disks: Vec::new(),
        clock_synced: true,
        utc_offset_secs: -21_600,
        interfaces: Vec::new(),
        oom_kills: Vec::new(),
        system_state: SystemState::Running,
        clock_ticks_per_sec: 100,
        machine: None,
        load: None,
        uptime_secs: None,
        memory: None,
    };
    let without = triage::evaluate(&snapshot, &[], None, &thresholds, 0);
    assert!(!without.iter().any(|f| matches!(f, Finding::Pressure { .. })), "no PSI must produce no pressure findings: {without:#?}");

    // And a host that *does* report PSI still triages on it, so the
    // absence path has not simply disabled the rule.
    snapshot.pressure = Some(Pressure { memory_some: PsiLine { avg60: 41.2, ..Default::default() }, ..Default::default() });
    let with = triage::evaluate(&snapshot, &[], None, &thresholds, 0);
    assert!(with.iter().any(|f| matches!(f, Finding::Pressure { .. })), "{with:#?}");
}
