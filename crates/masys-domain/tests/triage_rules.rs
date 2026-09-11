use masys_domain::finding::{Finding, FindingKind, PressureResource, Severity, Thresholds};
use masys_domain::journal::{Entry, Origin, Priority};
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
    Filesystem {
        mount_point: mount_point.to_string(),
        used_percent,
        free_bytes: 1_000_000,
        inode_used_percent: 10.0,
        read_only: false,
    }
}

/// One tick over a host that reported no boot pressure, logged nothing,
/// and has no SMART answer.
///
/// The three `evaluate` tests vary the sample, the units and the
/// thresholds and nothing else, so those three are the arguments and the
/// rest sit at the value that means "masys was told nothing". Named here
/// once rather than spelled at three call sites, which is the same
/// reason `unit` and `filesystem` above exist.
fn tick<'a>(
    snapshot: &'a Snapshot,
    units: &'a [Unit],
    thresholds: &'a Thresholds,
) -> triage::Tick<'a> {
    triage::Tick {
        snapshot,
        units,
        boot_pressure: None,
        entries: &[],
        smart: None,
        thermal_throttled_percent: None,
        thresholds,
        now_ms: 0,
    }
}

#[test]
fn failed_units_reports_only_failed() {
    let units = vec![
        unit("ok.service", ActiveState::Active),
        unit("broken.service", ActiveState::Failed),
    ];
    let findings = triage::failed_units(&units, 10_000);
    assert_eq!(findings.len(), 1);
    assert!(
        matches!(&findings[0].kind, FindingKind::FailedUnit { unit, .. } if unit == "broken.service")
    );
}

#[test]
fn flapping_units_needs_enough_restarts_in_the_window() {
    let thresholds = Thresholds {
        flapping_restart_count: 3,
        flapping_window_ms: 3_600_000,
        ..Thresholds::default()
    };
    let mut flaky = unit("otel-collector.service", ActiveState::Active);
    flaky.restart_timestamps_ms = vec![100, 200, 300];
    let calm = unit("nginx.service", ActiveState::Active);

    let findings = triage::flapping_units(&[flaky, calm], &thresholds, 3_600_100);
    assert_eq!(findings.len(), 1);
    assert!(
        matches!(&findings[0].kind, FindingKind::FlappingUnit { unit, restarts: 3, .. } if unit == "otel-collector.service")
    );
}

#[test]
fn flapping_units_ignores_restarts_outside_the_window() {
    let thresholds = Thresholds {
        flapping_restart_count: 3,
        flapping_window_ms: 1000,
        ..Thresholds::default()
    };
    let mut u = unit("otel-collector.service", ActiveState::Active);
    u.restart_timestamps_ms = vec![0, 1, 2];
    let findings = triage::flapping_units(&[u], &thresholds, 10_000);
    assert!(
        findings.is_empty(),
        "restarts at t=0..2 are outside a 1000ms window ending at t=10000"
    );
}

#[test]
fn pressure_flags_resources_over_the_some_threshold() {
    let thresholds = Thresholds {
        psi_some_avg60_percent: 20.0,
        psi_full_avg60_percent: 5.0,
        ..Thresholds::default()
    };
    let p = Pressure {
        cpu_some: PsiLine {
            avg60: 5.0,
            ..Default::default()
        },
        io_some: PsiLine {
            avg60: 22.7,
            ..Default::default()
        },
        io_full: PsiLine::default(),
        memory_some: PsiLine {
            avg60: 41.2,
            ..Default::default()
        },
        memory_full: PsiLine {
            avg60: 8.1,
            ..Default::default()
        },
    };
    let findings = triage::pressure(&p, &thresholds);
    assert_eq!(
        findings.len(),
        2,
        "cpu stays under threshold, io and memory don't: {findings:#?}"
    );
    assert!(findings.iter().any(|f| matches!(
        &f.kind,
        FindingKind::Pressure {
            resource: PressureResource::Io,
            ..
        }
    )));
    assert!(findings.iter().any(|f| matches!(
        &f.kind,
        FindingKind::Pressure {
            resource: PressureResource::Memory,
            ..
        }
    )));
}

#[test]
fn pressure_below_every_threshold_reports_nothing() {
    let findings = triage::pressure(&Pressure::default(), &Thresholds::default());
    assert!(findings.is_empty());
}

#[test]
fn disk_capacity_flags_filesystems_over_threshold_and_attaches_boot_generations() {
    let thresholds = Thresholds {
        disk_used_percent: 85.0,
        ..Thresholds::default()
    };
    let filesystems = vec![
        filesystem("/nix", 91.0),
        filesystem("/boot", 88.0),
        filesystem("/home", 40.0),
    ];
    let boot_pressure = BootPressure {
        generations: Some(9),
        reclaimable_bytes: Some(500_000_000),
    };

    let findings = triage::disk_capacity(&filesystems, Some(&boot_pressure), &thresholds);

    assert_eq!(findings.len(), 2, "/home is under threshold: {findings:#?}");
    let boot = findings
        .iter()
        .find(|f| matches!(&f.kind, FindingKind::DiskCapacity { mount_point, .. } if mount_point == "/boot"))
        .expect("a /boot finding");
    assert!(matches!(
        &boot.kind,
        FindingKind::DiskCapacity {
            generations: Some(9),
            ..
        }
    ));
    let nix = findings
        .iter()
        .find(|f| matches!(&f.kind, FindingKind::DiskCapacity { mount_point, .. } if mount_point == "/nix"))
        .expect("a /nix finding");
    assert!(
        matches!(
            &nix.kind,
            FindingKind::DiskCapacity {
                generations: None,
                ..
            }
        ),
        "generations only attaches to /boot"
    );
}

#[test]
fn inode_exhaustion_is_independent_of_disk_percent() {
    let thresholds = Thresholds {
        inode_used_percent: 90.0,
        ..Thresholds::default()
    };
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
    assert_eq!(
        findings,
        vec![Finding::new(FindingKind::ReadOnlyFilesystem {
            mount_point: "/".to_string()
        })]
    );
}

#[test]
fn clock_unsynchronized_fires_only_when_unsynced() {
    assert!(triage::clock_unsynchronized(true).is_empty());
    assert_eq!(
        triage::clock_unsynchronized(false),
        vec![Finding::new(FindingKind::ClockUnsynchronized)]
    );
}

#[test]
fn oom_kills_are_reported_one_finding_each() {
    let kills = vec![OomKill {
        pid: 8944,
        comm: "firefox".to_string(),
        timestamp_ms: 5_000,
    }];
    let findings = triage::oom_kills(&kills);
    assert_eq!(
        findings,
        vec![Finding::new(FindingKind::OomKill {
            pid: 8944,
            comm: "firefox".to_string(),
            timestamp_ms: 5_000
        })]
    );
}

#[test]
fn system_degraded_counts_the_failed_units_causing_it() {
    let units = vec![
        unit("a.service", ActiveState::Failed),
        unit("b.service", ActiveState::Active),
        unit("c.service", ActiveState::Failed),
    ];
    let findings = triage::system_degraded(SystemState::Degraded, &units);
    assert_eq!(
        findings,
        vec![Finding::new(FindingKind::SystemDegraded {
            failed_units: 2
        })]
    );
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
        oom_kills: vec![OomKill {
            pid: 1,
            comm: "x".to_string(),
            timestamp_ms: 0,
        }],
        system_state: SystemState::Degraded,
        clock_ticks_per_sec: 100,
        machine: None,
        load: None,
        uptime_secs: None,
        memory: None,
        cpu_times: None,
        thermal_throttled_ms_by_core: None,
    };
    let thresholds = Thresholds::default();

    let findings = triage::evaluate(&tick(&snapshot, &units, &thresholds));

    assert!(
        findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::FailedUnit { .. }))
    );
    assert!(
        findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::DiskCapacity { .. }))
    );
    assert!(
        findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::ClockUnsynchronized))
    );
    assert!(
        findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::OomKill { .. }))
    );
    assert!(
        findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::SystemDegraded { .. }))
    );
}

#[test]
fn pressure_orders_resources_memory_io_cpu() {
    let thresholds = Thresholds {
        psi_some_avg60_percent: 1.0,
        psi_full_avg60_percent: 1.0,
        ..Thresholds::default()
    };
    let p = Pressure {
        cpu_some: PsiLine {
            avg60: 12.0,
            ..Default::default()
        },
        io_some: PsiLine {
            avg60: 22.7,
            ..Default::default()
        },
        io_full: PsiLine::default(),
        memory_some: PsiLine {
            avg60: 41.2,
            ..Default::default()
        },
        memory_full: PsiLine {
            avg60: 8.1,
            ..Default::default()
        },
    };
    let resources: Vec<PressureResource> = triage::pressure(&p, &thresholds)
        .iter()
        .map(|f| match &f.kind {
            FindingKind::Pressure { resource, .. } => *resource,
            other => panic!("pressure emitted a non-Pressure finding: {other:?}"),
        })
        .collect();
    // The mockup lists memory above io; cpu last, being the least
    // predictive of the machine becoming unusable.
    assert_eq!(
        resources,
        vec![
            PressureResource::Memory,
            PressureResource::Io,
            PressureResource::Cpu
        ]
    );
}

/// PSI needs kernel 4.20+ with `CONFIG_PSI=y`, and some kernels still
/// want `psi=1` on the command line. Defaulting the missing case to
/// zeroes made such a host look perfectly idle - the most dangerous wrong
/// answer a triage pass can give, because it is silence that reads as
/// health.
#[test]
fn a_kernel_without_psi_reports_no_pressure_findings_rather_than_zero_ones() {
    let thresholds = Thresholds {
        psi_some_avg60_percent: 20.0,
        ..Thresholds::default()
    };

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
        cpu_times: None,
        thermal_throttled_ms_by_core: None,
    };
    let without = triage::evaluate(&tick(&snapshot, &[], &thresholds));
    assert!(
        !without
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::Pressure { .. })),
        "no PSI must produce no pressure findings: {without:#?}"
    );

    // And a host that *does* report PSI still triages on it, so the
    // absence path has not simply disabled the rule.
    snapshot.pressure = Some(Pressure {
        memory_some: PsiLine {
            avg60: 41.2,
            ..Default::default()
        },
        ..Default::default()
    });
    let with = triage::evaluate(&tick(&snapshot, &[], &thresholds));
    assert!(
        with.iter()
            .any(|f| matches!(&f.kind, FindingKind::Pressure { .. })),
        "{with:#?}"
    );
}

/// A host whose `/boot` is not generation-based attaches no count, rather
/// than attaching zero.
///
/// The distinction the second platform adapter forced: `masys_render::view`
/// appends `. {n} generations` to this finding whenever the value is
/// present, so `Some(0)` would tell a Debian operator their `/boot` holds
/// "0 generations" - a count of something that host does not have. Both of
/// the two `None`s reaching here - no boot pressure read, and no such
/// concept - correctly mean "say nothing".
#[test]
fn a_boot_without_generations_attaches_no_count_rather_than_zero() {
    let thresholds = Thresholds {
        disk_used_percent: 85.0,
        ..Thresholds::default()
    };
    let filesystems = vec![filesystem("/boot", 91.0)];
    let debian = BootPressure {
        generations: None,
        reclaimable_bytes: Some(175_000_000),
    };

    let findings = triage::disk_capacity(&filesystems, Some(&debian), &thresholds);

    assert!(
        matches!(
            findings.as_slice(),
            [f] if matches!(
                &f.kind,
                FindingKind::DiskCapacity {
                    generations: None,
                    ..
                }
            )
        ),
        "{findings:#?}"
    );
}

/// What `apt autoremove` or a garbage collection would free, carried to
/// the finding.
///
/// The actionable half of boot pressure and the only half a Debian host
/// has: with `generations` correctly `None` there, a finding that carried
/// nothing else would say `/boot is 91% full` and stop - which is the
/// percentage the operator could already see. `reclaimable_bytes` was a
/// field on the port with no reader anywhere until the second adapter gave
/// it a producer.
#[test]
fn what_could_be_reclaimed_reaches_the_finding() {
    let thresholds = Thresholds {
        disk_used_percent: 85.0,
        ..Thresholds::default()
    };
    let findings = triage::disk_capacity(
        &[filesystem("/boot", 91.0)],
        Some(&BootPressure {
            generations: None,
            reclaimable_bytes: Some(175_000_000),
        }),
        &thresholds,
    );
    assert!(
        matches!(
            findings.as_slice(),
            [f] if matches!(
                &f.kind,
                FindingKind::DiskCapacity {
                    reclaimable_bytes: Some(175_000_000),
                    ..
                }
            )
        ),
        "{findings:#?}"
    );
}

/// And nothing to reclaim says nothing, rather than `0 B`.
#[test]
fn a_filesystem_that_is_not_boot_carries_no_reclaimable_figure() {
    let thresholds = Thresholds {
        disk_used_percent: 85.0,
        ..Thresholds::default()
    };
    let findings = triage::disk_capacity(
        &[filesystem("/home", 91.0)],
        Some(&BootPressure {
            generations: Some(9),
            reclaimable_bytes: Some(500_000_000),
        }),
        &thresholds,
    );
    assert!(
        matches!(
            findings.as_slice(),
            [f] if matches!(
                &f.kind,
                FindingKind::DiskCapacity {
                    reclaimable_bytes: None,
                    ..
                }
            )
        ),
        "boot pressure is about /boot: {findings:#?}"
    );
}

/// The rule behind both the finding and the colour, in one place.
///
/// A finding says memory is hurting; the overview's colour says the same
/// thing in one glyph's worth of screen. If they were two rules they
/// would disagree the first time a threshold moved, so there is one -
/// and `triage::pressure` is written in terms of it.
#[test]
fn pressure_severity_rises_with_the_threshold_that_is_crossed() {
    let thresholds = Thresholds {
        psi_some_avg60_percent: 20.0,
        psi_full_avg60_percent: 5.0,
        ..Thresholds::default()
    };
    assert_eq!(
        triage::pressure_severity(19.9, Some(4.9), &thresholds),
        Severity::Normal,
        "under both thresholds"
    );
    assert_eq!(
        triage::pressure_severity(20.0, Some(4.9), &thresholds),
        Severity::Warning,
        "some tasks stalling"
    );
    assert_eq!(
        triage::pressure_severity(90.0, Some(5.0), &thresholds),
        Severity::Urgent,
        "every task stalling is worse than some of them"
    );
}

/// CPU has no `full` line - the kernel does not report one, because a
/// runnable task is never *fully* stalled on CPU. Absent must not read as
/// a zero that clears the urgent threshold by luck.
#[test]
fn pressure_severity_without_a_full_line_can_still_warn() {
    let thresholds = Thresholds {
        psi_some_avg60_percent: 20.0,
        psi_full_avg60_percent: 5.0,
        ..Thresholds::default()
    };
    assert_eq!(
        triage::pressure_severity(50.0, None, &thresholds),
        Severity::Warning
    );
    assert_eq!(
        triage::pressure_severity(1.0, None, &thresholds),
        Severity::Normal
    );
}

/// The agreement itself, asserted rather than assumed: a resource has a
/// finding exactly when its severity is not `Normal`. This is what makes
/// moving a threshold in config move both.
#[test]
fn a_pressure_finding_exists_exactly_when_severity_is_not_normal() {
    let thresholds = Thresholds {
        psi_some_avg60_percent: 20.0,
        psi_full_avg60_percent: 5.0,
        ..Thresholds::default()
    };
    for memory_some in [0.0, 19.9, 20.0, 99.0] {
        let p = Pressure {
            memory_some: PsiLine {
                avg60: memory_some,
                ..Default::default()
            },
            ..Default::default()
        };
        let severity =
            triage::pressure_severity(p.memory_some.avg60, Some(p.memory_full.avg60), &thresholds);
        let has_finding = triage::pressure(&p, &thresholds).iter().any(|f| {
            matches!(
                &f.kind,
                FindingKind::Pressure {
                    resource: PressureResource::Memory,
                    ..
                }
            )
        });
        assert_eq!(
            has_finding,
            severity != Severity::Normal,
            "memory_some {memory_some}: finding and severity must agree"
        );
    }
}

/// The other half of that agreement: a finding does not merely *exist*
/// when the severity is not `Normal`, it *carries* that severity.
///
/// It did not, and the gap reached the screen. `triage::pressure`
/// computed `Urgent` for a full-stall over threshold, kept the finding
/// and discarded the verdict; the renderer then drew every pressure row
/// from a hardcoded yellow. The overview's segment directly above it,
/// built from the same two figures through `Reading`, drew the same
/// reading light-red - so the row that exists to give the detail was
/// quieter than the summary it details.
///
/// Literal figures rather than only the sweep below, so this cannot pass
/// by recomputing the rule the way the rule computes it: 6.0 is over the
/// 5.0 full threshold, which the documented rule calls `Urgent`, and 40.0
/// over 20.0 with a full line under its own threshold is `Warning`.
#[test]
fn a_pressure_finding_carries_the_severity_its_figures_earn() {
    let thresholds = Thresholds {
        psi_some_avg60_percent: 20.0,
        psi_full_avg60_percent: 5.0,
        ..Thresholds::default()
    };
    let stalling = |some: f32, full: f32| Pressure {
        memory_some: PsiLine {
            avg60: some,
            ..Default::default()
        },
        memory_full: PsiLine {
            avg60: full,
            ..Default::default()
        },
        ..Default::default()
    };

    let urgent = triage::pressure(&stalling(40.0, 6.0), &thresholds);
    assert_eq!(
        urgent.first().map(|f| f.severity()),
        Some(Severity::Urgent),
        "a full-stall over threshold is urgent: {urgent:#?}"
    );

    let warning = triage::pressure(&stalling(40.0, 1.0), &thresholds);
    assert_eq!(
        warning.first().map(|f| f.severity()),
        Some(Severity::Warning),
        "some-stall alone is a warning: {warning:#?}"
    );
}

/// A figure that is not a number is not a reading, and a comparison
/// against it is false in both directions - so an unusable PSI value
/// would clear every threshold and be reported as `Normal`. It says
/// `Unknown` instead, and produces no finding: masys has nothing to
/// report and nothing to reassure anybody about.
#[test]
fn a_psi_figure_that_is_not_a_number_is_unknown_rather_than_fine() {
    let thresholds = Thresholds::default();
    assert_eq!(
        triage::pressure_severity(f32::NAN, Some(0.0), &thresholds),
        Severity::Unknown
    );
    assert_eq!(
        triage::pressure_severity(0.0, Some(f32::NAN), &thresholds),
        Severity::Unknown
    );

    let unusable = Pressure {
        memory_some: PsiLine {
            avg60: f32::NAN,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(
        triage::pressure(&unusable, &thresholds).is_empty(),
        "an unknown reading is not a finding"
    );
}

/// The mapping from a resource to its PSI lines lives in one place, and
/// this is what that buys: the figure a finding carries is the figure
/// its severity was computed from.
#[test]
fn a_findings_figures_are_the_ones_its_severity_was_judged_on() {
    let thresholds = Thresholds {
        psi_some_avg60_percent: 20.0,
        psi_full_avg60_percent: 5.0,
        ..Thresholds::default()
    };
    let p = Pressure {
        memory_some: PsiLine {
            avg60: 41.0,
            ..Default::default()
        },
        memory_full: PsiLine {
            avg60: 8.0,
            ..Default::default()
        },
        io_some: PsiLine {
            avg60: 30.0,
            ..Default::default()
        },
        cpu_some: PsiLine {
            avg60: 25.0,
            ..Default::default()
        },
        ..Default::default()
    };
    for finding in triage::pressure(&p, &thresholds) {
        let FindingKind::Pressure {
            resource,
            some_avg60,
            full_avg60,
            ..
        } = finding.kind
        else {
            panic!("pressure produces pressure findings");
        };
        assert_eq!(
            (some_avg60, full_avg60),
            triage::psi_lines(&p, resource),
            "{resource:?}"
        );
    }
    assert_eq!(
        triage::psi_lines(&p, masys_domain::finding::PressureResource::Cpu).1,
        None,
        "the kernel reports no full line for cpu"
    );
}

/// One journal line.
///
/// Addresses in these fixtures are RFC 5737 documentation addresses,
/// which route nowhere by definition. This tree is published, and
/// `repo-guard` refuses RFC1918 ranges on the way out: an address that
/// looks like somebody's LAN is a fact about a machine, and this
/// repository states none.
fn logged(timestamp_ms: u64, unit: Option<&str>, message: &str, origin: Origin) -> Entry {
    Entry {
        timestamp_ms,
        unit: unit.map(str::to_string),
        priority: Priority::Error,
        message: message.to_string(),
        origin,
    }
}

const NOW: u64 = 1_000_000_000;

/// Who sent the line decides which section it lands in - the only thing
/// that separates the two, and the reason one query serves both.
#[test]
fn kernel_lines_and_userspace_lines_go_to_different_sections() {
    let entries = vec![
        logged(
            NOW - 1_000,
            None,
            "EXT4-fs error (device sda1)",
            Origin::Kernel,
        ),
        logged(
            NOW - 2_000,
            Some("sshd.service"),
            "too many auth failures",
            Origin::Userspace,
        ),
    ];
    let findings = triage::journal_findings(&entries, &Thresholds::default(), NOW);
    assert!(
        findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::KernelError { message, .. } if message.contains("EXT4"))),
        "{findings:#?}"
    );
    assert!(
        findings.iter().any(|f| matches!(
            &f.kind,
            FindingKind::RecentError { unit: Some(u), .. } if u == "sshd.service"
        )),
        "{findings:#?}"
    );
}

/// A retry storm is one finding with a count, not forty rows. The buffer
/// is empty on a healthy machine, and a wall of near-identical lines is
/// how an operator learns to stop reading it.
#[test]
fn repeats_of_one_line_collapse_into_a_single_row_with_a_count() {
    let entries: Vec<Entry> = (0..12)
        .map(|i| {
            logged(
                NOW - (i * 1_000),
                Some("sshd.service"),
                &format!(
                    "Failed password for root from 198.51.100.{i} port {}",
                    52_000 + i
                ),
                Origin::Userspace,
            )
        })
        .collect();
    let findings = triage::journal_findings(&entries, &Thresholds::default(), NOW);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert!(
        matches!(
            &findings[0].kind,
            FindingKind::RecentError { count: 12, .. }
        ),
        "{findings:#?}"
    );
}

/// And the thing that must not happen: two different failures from one
/// sender showing as one, with whichever message won speaking for both.
#[test]
fn two_different_failures_from_one_sender_stay_two_rows() {
    let entries = vec![
        logged(
            NOW - 1_000,
            Some("sshd.service"),
            "Failed password for root from 198.51.100.5 port 52134",
            Origin::Userspace,
        ),
        logged(
            NOW - 2_000,
            Some("sshd.service"),
            "Failed password for root from 198.51.100.9 port 41022",
            Origin::Userspace,
        ),
        logged(
            NOW - 3_000,
            Some("sshd.service"),
            "Invalid user admin from 198.51.100.7",
            Origin::Userspace,
        ),
    ];
    let findings = triage::journal_findings(&entries, &Thresholds::default(), NOW);
    assert_eq!(findings.len(), 2, "{findings:#?}");
    let counts: Vec<u32> = findings
        .iter()
        .filter_map(|f| match &f.kind {
            FindingKind::RecentError { count, .. } => Some(*count),
            _ => None,
        })
        .collect();
    assert!(counts.contains(&2) && counts.contains(&1), "{counts:?}");
}

/// The masking rule itself, which is the part that can be subtly wrong.
#[test]
fn a_messages_shape_keeps_what_identifies_it_and_drops_what_varies() {
    let shape = triage::message_shape;
    assert_eq!(
        shape("Failed password for root from 198.51.100.5 port 52134"),
        shape("Failed password for root from 198.51.100.9 port 41022"),
        "addresses and ports vary per occurrence"
    );
    assert_ne!(
        shape("Failed password for root"),
        shape("Invalid user admin"),
        "different failures are different shapes"
    );
    // A device name is part of what the line is about, so only the digit
    // goes - `sda1` and `sdb1` stay apart.
    assert_ne!(
        shape("EXT4-fs error (device sda1)"),
        shape("EXT4-fs error (device sdb1)")
    );
    // A hex identifier varies per occurrence and is masked whole, rather
    // than losing only its digits.
    assert_eq!(
        shape("GPU hang ecode 9:1:85dffffb"),
        shape("GPU hang ecode 9:1:0a1b2c3d")
    );
    // A hex-looking word with no digits in it is a word.
    assert_eq!(shape("cafebabe deadbeef"), "cafebabe deadbeef");
}

/// Lines older than the window are not this tick's news.
#[test]
fn a_line_outside_the_window_is_not_reported() {
    let thresholds = Thresholds::default();
    let stale = NOW - thresholds.journal_window_ms - 1;
    let entries = vec![logged(
        stale,
        Some("cron.service"),
        "job failed",
        Origin::Userspace,
    )];
    assert!(
        triage::journal_findings(&entries, &thresholds, NOW).is_empty(),
        "an hour-old error is not what is happening now"
    );
}

/// A quiet host reports nothing at all, which is the buffer's premise.
#[test]
fn a_host_with_nothing_logged_produces_no_findings() {
    assert!(triage::journal_findings(&[], &Thresholds::default(), NOW).is_empty());
}

/// Newest first: the row at the top is the thing that just happened.
#[test]
fn findings_are_ordered_by_how_recently_they_were_last_seen() {
    let entries = vec![
        logged(
            NOW - 60_000,
            Some("old.service"),
            "an older failure",
            Origin::Userspace,
        ),
        logged(
            NOW - 1_000,
            Some("new.service"),
            "a newer failure",
            Origin::Userspace,
        ),
    ];
    let findings = triage::journal_findings(&entries, &Thresholds::default(), NOW);
    assert!(
        matches!(&findings[0].kind, FindingKind::RecentError { unit: Some(u), .. } if u == "new.service"),
        "{findings:#?}"
    );
}

/// An age, not a timestamp - the renderer has no clock, and every other
/// finding already states its time this way.
#[test]
fn a_finding_carries_how_long_ago_it_was_last_seen() {
    let entries = vec![logged(
        NOW - 90_000,
        Some("a.service"),
        "failed",
        Origin::Userspace,
    )];
    let findings = triage::journal_findings(&entries, &Thresholds::default(), NOW);
    assert!(
        matches!(
            &findings[0].kind,
            FindingKind::RecentError { age_ms: 90_000, .. }
        ),
        "{findings:#?}"
    );
}

/// A uuid per occurrence is how services actually log a retry storm, and
/// collapsing those is the whole job. This is what the masking threshold
/// is set for: eight characters left two lines bearing different uuids as
/// two rows, because only two of a uuid's five groups are that long.
#[test]
fn two_lines_differing_only_by_a_uuid_are_one_finding() {
    let entries = vec![
        logged(
            NOW - 1_000,
            Some("api.service"),
            "request 550e8400-e29b-41d4-a716-446655440000 failed",
            Origin::Userspace,
        ),
        logged(
            NOW - 2_000,
            Some("api.service"),
            "request f81d4fae-7dec-11d0-a765-00a0c91e6bf6 failed",
            Origin::Userspace,
        ),
    ];
    let findings = triage::journal_findings(&entries, &Thresholds::default(), NOW);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert!(
        matches!(&findings[0].kind, FindingKind::RecentError { count: 2, .. }),
        "{findings:#?}"
    );
}

/// And the line either side of that threshold still tells devices apart,
/// which is what stops it merging two failing disks into one row.
#[test]
fn a_shorter_threshold_still_keeps_two_devices_apart() {
    assert_ne!(
        triage::message_shape("EXT4-fs error (device sda1)"),
        triage::message_shape("EXT4-fs error (device sdb1)")
    );
}

/// Every distinct shape is returned. How many fit on a screen is the row
/// builder's question, and a rule that dropped some would leave the
/// section heading counting survivors.
#[test]
fn the_rule_returns_every_distinct_shape_it_found() {
    let wordings = [
        "cannot bind socket",
        "certificate expired",
        "disk quota exceeded",
        "upstream refused the connection",
        "template render failed",
        "no route to host",
    ];
    let entries: Vec<Entry> = wordings
        .iter()
        .enumerate()
        .map(|(i, m)| {
            logged(
                NOW - (i as u64 * 1_000),
                Some("a.service"),
                m,
                Origin::Userspace,
            )
        })
        .collect();
    assert_eq!(
        triage::journal_findings(&entries, &Thresholds::default(), NOW).len(),
        wordings.len()
    );
}

/// **A record nobody could attribute is reported, and not as the
/// kernel's.**
///
/// `Origin::Unknown` is journald recording a transport this reader could
/// not make sense of. Three things could happen to such a record and only
/// one of them is honest: it could be dropped, which loses an error the
/// host really logged; it could join the Kernel section, which is a
/// positive claim about where it came from; or it can land in `Recent
/// errors`, which is named for *when* rather than for *where* and is the
/// complement of the kernel's claim rather than a rival claim.
///
/// The third. This pins it, because the choice is invisible in the
/// output - an unattributed row looks exactly like a userspace one - and
/// so it is the kind of decision that drifts.
#[test]
fn a_record_with_no_attributable_origin_is_reported_but_not_as_the_kernels() {
    let entries = vec![
        logged(
            NOW - 1_000,
            None,
            "EXT4-fs error (device sda1)",
            Origin::Kernel,
        ),
        logged(
            NOW - 2_000,
            None,
            "watchdog did not respond",
            Origin::Unknown,
        ),
    ];
    let findings = triage::journal_findings(&entries, &Thresholds::default(), NOW);

    assert!(
        findings.iter().any(|f| matches!(
            &f.kind,
            FindingKind::RecentError { message, .. } if message.contains("watchdog")
        )),
        "the record reaches the section named for when errors happened, \
         rather than being dropped or claimed by the kernel: {findings:#?}"
    );
    assert!(
        !findings.iter().any(|f| matches!(
            &f.kind,
            FindingKind::KernelError { message, .. } if message.contains("watchdog")
        )),
        "and not as a kernel record, which nobody established: {findings:#?}"
    );
    assert_eq!(
        findings
            .iter()
            .filter(|f| matches!(&f.kind, FindingKind::KernelError { .. }))
            .count(),
        1,
        "the kernel section holds only what journald attributed to the kernel"
    );
}

/// Throttling past the threshold is a finding; below it is the ordinary
/// burst behaviour of a machine that boosts, and reporting it would put a
/// row on the buffer that is always there.
#[test]
fn thermal_throttling_reports_only_past_the_threshold() {
    let thresholds = Thresholds::default();
    assert_eq!(
        triage::thermal_throttling(Some(40.0), &thresholds),
        vec![Finding::new(FindingKind::ThermalThrottling {
            percent: 40.0
        })]
    );
    assert_eq!(
        triage::thermal_throttling(Some(1.5), &thresholds),
        Vec::new(),
        "a brief boost-clock burst is not a finding"
    );
}

/// The rule every reading in this tree keeps: not measured is not zero. A
/// kernel that does not account for throttling must produce no finding,
/// not a finding saying nothing is wrong.
#[test]
fn thermal_throttling_unmeasured_is_not_a_finding() {
    assert_eq!(
        triage::thermal_throttling(None, &Thresholds::default()),
        Vec::new()
    );
}

/// A NaN reading has no order against a threshold - every comparison is
/// false - so it must be refused explicitly rather than silently passing
/// for "below". `pressure_severity` guards the same way.
#[test]
fn thermal_throttling_refuses_a_reading_that_is_not_a_number() {
    assert_eq!(
        triage::thermal_throttling(Some(f32::NAN), &Thresholds::default()),
        Vec::new()
    );
}
