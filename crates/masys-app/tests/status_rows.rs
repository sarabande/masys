mod fake;

use fake::{FakePlatformService, FakeSystemService};
use masys_app::status::{StatusBuffer, Tick};
use masys_domain::finding::{
    Finding, FindingKind, PressureResource, Severity, Thresholds, UnreadableSource,
};
use masys_domain::journal::{Entry, Origin, Priority};
use masys_domain::sample::SystemState;
use masys_view::{Node, Overview, SectionKind};
use masys_view::{OverviewPart, Reading};

fn overview() -> Overview {
    Overview {
        machine: None,
        system_state: SystemState::Running,
        unit_count: Some(47),
        load_1: None,
        load_5: None,
        load_15: None,
        uptime_secs: None,
        net_throughput: None,
        disk_throughput: None,
        cpu_percent: None,
        cpu_pressure: None,
        io_pressure: None,
        memory_pressure: None,
        mem_used_bytes: None,
        mem_total_bytes: None,
        zram_percent: None,
        swap_free_bytes: None,
        clock_synced: Reading {
            value: true,
            severity: Severity::Normal,
        },
        smart_ok: None,
        pending_reboot: None,
    }
}

#[test]
fn a_healthy_system_shows_only_the_system_section() {
    let rows = StatusBuffer {
        overview: Some(overview()),
        ..Default::default()
    }
    .rows();
    assert!(matches!(
        &rows[0],
        Node::SectionHeader {
            kind: SectionKind::System,
            ..
        }
    ));
    // A header and one row per line the overview has to draw. This
    // fixture reports no machine, no CPU figure, no throughput and no
    // PSI, so only the two unconditional parts appear.
    assert_eq!(
        rows[1..]
            .iter()
            .map(|row| match row {
                Node::OverviewLine { part, .. } => *part,
                other => panic!("not an overview line: {other:?}"),
            })
            .collect::<Vec<_>>(),
        vec![OverviewPart::Status, OverviewPart::Health],
        "{rows:#?}"
    );
}

/// Each line is its own row, so the cursor can rest on one of them rather
/// than on the whole block.
///
/// The section was a single `Node` holding every line until 2026-09-05,
/// which made it one seven-line list item: selectable, but highlighting
/// all of it at once, because a list item is the unit of selection.
#[test]
fn a_fully_answered_host_gets_a_row_for_every_overview_line() {
    let full = Overview {
        machine: Some(masys_domain::sample::Machine {
            distro: "NixOS 26.11".to_string(),
            kernel: "6.18.42".to_string(),
            arch: "x86_64".to_string(),
            cpu_model: "Core i7".to_string(),
            cpu_cores: 8,
        }),
        cpu_percent: Some(Reading {
            value: 12.0,
            severity: Severity::Normal,
        }),
        net_throughput: Some(masys_domain::rate::Throughput {
            in_bytes_per_sec: 1.0,
            out_bytes_per_sec: 1.0,
        }),
        cpu_pressure: Some(Reading {
            value: 3.0,
            severity: Severity::Normal,
        }),
        ..overview()
    };
    let rows = StatusBuffer {
        overview: Some(full),
        ..Default::default()
    }
    .rows();
    let parts: Vec<OverviewPart> = rows[1..]
        .iter()
        .filter_map(|row| match row {
            Node::OverviewLine { part, .. } => Some(*part),
            _ => None,
        })
        .collect();
    assert_eq!(parts, OverviewPart::ALL, "every part draws its own row");
    assert!(
        rows[1..].iter().all(|row| row.selectable()),
        "each line is a row the cursor can rest on"
    );
}

#[test]
fn a_failed_unit_gets_its_own_section() {
    let findings = vec![Finding::new(FindingKind::FailedUnit {
        unit: "restic-backup.service".to_string(),
        exit_code: Some(1),
        since_ms: 10_800_000,
        reason: None,
    })];
    let rows = StatusBuffer {
        findings,
        overview: Some(overview()),
        ..Default::default()
    }
    .rows();
    // System, its two overview lines, a spacer, then the finding section.
    assert_eq!(rows.len(), 6, "{rows:#?}");
    assert!(matches!(
        &rows[0],
        Node::SectionHeader {
            kind: SectionKind::System,
            ..
        }
    ));
    assert!(matches!(
        &rows[4],
        Node::SectionHeader {
            kind: SectionKind::FailedUnits,
            count: Some(1),
            ..
        }
    ));
    assert!(matches!(
        &rows[5],
        Node::Finding {
            finding: masys_domain::Finding {
                kind: FindingKind::FailedUnit { .. },
                ..
            },
            ..
        }
    ));
}

#[test]
fn disk_findings_of_every_kind_share_one_section() {
    let findings = vec![
        Finding::new(FindingKind::DiskCapacity {
            mount_point: "/nix".to_string(),
            used_percent: 91.0,
            free_bytes: 1,
            generations: None,
            reclaimable_bytes: None,
        }),
        Finding::new(FindingKind::InodeExhaustion {
            mount_point: "/var".to_string(),
            inode_used_percent: 95.0,
        }),
        Finding::new(FindingKind::ReadOnlyFilesystem {
            mount_point: "/".to_string(),
        }),
    ];
    let rows = StatusBuffer {
        findings,
        overview: Some(overview()),
        ..Default::default()
    }
    .rows();
    let header_count = rows
        .iter()
        .filter(|r| {
            matches!(
                r,
                Node::SectionHeader {
                    kind: SectionKind::Disk,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        header_count, 1,
        "all three disk-shaped findings share one section: {rows:#?}"
    );
    let finding_count = rows
        .iter()
        .filter(|r| matches!(r, Node::Finding { .. }))
        .count();
    assert_eq!(finding_count, 3);
}

#[test]
fn sections_appear_in_a_fixed_order() {
    let findings = vec![
        Finding::new(FindingKind::SystemDegraded { failed_units: 1 }),
        Finding::new(FindingKind::FailedUnit {
            unit: "a.service".to_string(),
            exit_code: None,
            since_ms: 0,
            reason: None,
        }),
    ];
    let rows = StatusBuffer {
        findings,
        overview: Some(overview()),
        ..Default::default()
    }
    .rows();
    let kinds: Vec<SectionKind> = rows
        .iter()
        .filter_map(|r| match r {
            Node::SectionHeader { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    // System leads: what the host *is* frames every finding under it.
    assert_eq!(
        kinds,
        vec![
            SectionKind::System,
            SectionKind::FailedUnits,
            SectionKind::Degraded
        ],
        "{kinds:?}"
    );
}

/// Before the first tick there is genuinely nothing to show - not a
/// System section reporting a machine nobody sampled.
///
/// The comment on this branch called it "the state only a test can
/// observe", and until now no test did: making `rows` draw a defaulted
/// overview instead failed nothing.
#[test]
fn nothing_is_drawn_before_the_first_sample() {
    assert!(StatusBuffer::default().rows().is_empty());
}

/// A host that churns through failing units should not accumulate every
/// answer it has ever given.
///
/// Driven through `refresh`, which is where the bound is applied, rather
/// than by reaching for the private step that applies it: sixty failed
/// units leave sixty cached answers, and sixty-five leave none.
#[test]
fn the_failure_reason_cache_is_bounded() {
    let refresh_with = |failed: usize| {
        let units: Vec<_> = (0..failed)
            .map(|n| failed_unit(&format!("unit{n}.service")))
            .collect();
        let system = FakeSystemService {
            snapshot: snapshot(),
            units: units.clone(),
            calls: Default::default(),
            fails_with: None,
            journal: Vec::new(),
            appended: Default::default(),
            journal_queries: Default::default(),
            queued: Default::default(),
            proc_details: Default::default(),
            detail_queries: Default::default(),
            units_fail_with: None,
            sample_fail_with: None,
            smart: None,
            smart_reads: Default::default(),
        };
        let platform = FakePlatformService::default();
        let mut status = StatusBuffer::default();
        status.refresh(
            &system,
            &platform,
            Tick {
                previous: None,
                snapshot: &snapshot(),
                units: &units,
                units_unreadable: None,
                net_throughput: None,
                disk_throughput: None,
                now_ms: 1_000_000,
            },
        );
        status.reasons.len()
    };
    assert_eq!(refresh_with(60), 60, "under the bound every answer is kept");
    assert_eq!(
        refresh_with(65),
        0,
        "past it the cache is dropped whole rather than trimmed"
    );
}

fn snapshot() -> masys_domain::sample::Snapshot {
    masys_domain::sample::Snapshot {
        taken_at_ms: 0,
        procs: Vec::new(),
        pressure: Some(masys_domain::sample::Pressure::default()),
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

fn failed_unit(name: &str) -> masys_domain::unit::Unit {
    masys_domain::unit::Unit {
        name: name.to_string(),
        kind: masys_domain::unit::UnitKind::Service,
        active_state: masys_domain::unit::ActiveState::Failed,
        sub_state: "failed".to_string(),
        exit_code: Some(1),
        enabled: true,
        restart_timestamps_ms: Vec::new(),
        since_ms: 1_000,
        cgroup: None,
        slice: None,
        timer: None,
        triggers: Vec::new(),
    }
}

/// A configured threshold changes what triage finds.
///
/// The seam has taken thresholds as a parameter since it was written and
/// nothing ever passed anything but the default, so nothing checked that
/// a different one reached the rules. A filesystem at 60% is quiet under
/// the design's 85 and a finding under a 50 somebody wrote down.
#[test]
fn a_lowered_threshold_finds_what_the_default_would_not() {
    let half_full = masys_domain::sample::Filesystem {
        mount_point: "/".to_string(),
        used_percent: 60.0,
        free_bytes: 1 << 30,
        inode_used_percent: 5.0,
        read_only: false,
    };
    let refresh_at = |disk_used_percent: f32| {
        let system = FakeSystemService {
            snapshot: masys_domain::sample::Snapshot {
                filesystems: vec![half_full.clone()],
                ..snapshot()
            },
            units: Vec::new(),
            calls: Default::default(),
            fails_with: None,
            journal: Vec::new(),
            appended: Default::default(),
            journal_queries: Default::default(),
            queued: Default::default(),
            proc_details: Default::default(),
            detail_queries: Default::default(),
            units_fail_with: None,
            sample_fail_with: None,
            smart: None,
            smart_reads: Default::default(),
        };
        let mut status = StatusBuffer {
            thresholds: Thresholds {
                disk_used_percent,
                ..Thresholds::default()
            },
            ..Default::default()
        };
        status.refresh(
            &system,
            &FakePlatformService::default(),
            Tick {
                previous: None,
                snapshot: &system.snapshot.clone(),
                units: &[],
                units_unreadable: None,
                net_throughput: None,
                disk_throughput: None,
                now_ms: 1_000_000,
            },
        );
        status
            .findings
            .iter()
            .filter(|finding| matches!(&finding.kind, FindingKind::DiskCapacity { .. }))
            .count()
    };

    assert_eq!(
        refresh_at(Thresholds::default().disk_used_percent),
        0,
        "85 is the design's number and 60% is quiet under it"
    );
    assert_eq!(
        refresh_at(50.0),
        1,
        "a threshold somebody wrote down has to reach the rule"
    );
}

/// The memory reading and the memory finding are one judgement, and this
/// is the test that says so: move the threshold, and both move.
///
/// Written against the session rather than against `triage` because this
/// is where the two could come apart - `refresh` fills the overview and
/// builds the findings in the same call, from the same snapshot, and
/// nothing but a shared rule keeps a segment plain while the finding
/// under it says the machine is stalling.
#[test]
fn moving_the_psi_threshold_moves_the_colour_and_the_finding_together() {
    let stalling = masys_domain::sample::Pressure {
        memory_some: masys_domain::sample::PsiLine {
            avg60: 30.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let judge = |psi_some_avg60_percent: f32| {
        let snapshot = masys_domain::sample::Snapshot {
            pressure: Some(stalling),
            memory: Some(masys_domain::sample::Memory {
                used_bytes: 29_400_000_000,
                total_bytes: 32_000_000_000,
                swap_free_bytes: 0,
                zram_percent: None,
            }),
            ..snapshot()
        };
        let mut status = StatusBuffer {
            thresholds: Thresholds {
                psi_some_avg60_percent,
                ..Thresholds::default()
            },
            ..Default::default()
        };
        status.refresh(
            &system_reporting(snapshot.clone()),
            &FakePlatformService::default(),
            Tick {
                previous: None,
                snapshot: &snapshot,
                units: &[],
                units_unreadable: None,
                net_throughput: None,
                disk_throughput: None,
                now_ms: 1_000_000,
            },
        );
        let severity = status
            .overview
            .as_ref()
            .and_then(|o| o.mem_used_bytes)
            .expect("a memory reading")
            .severity;
        let flagged = status.findings.iter().any(|f| {
            matches!(
                &f.kind,
                FindingKind::Pressure {
                    resource: PressureResource::Memory,
                    ..
                }
            )
        });
        (severity, flagged)
    };

    assert_eq!(
        judge(20.0),
        (Severity::Warning, true),
        "30% stalled crosses a threshold of 20"
    );
    assert_eq!(
        judge(50.0),
        (Severity::Normal, false),
        "and clears one of 50 - the segment and the finding both"
    );
}

/// A kernel with no PSI cannot say memory is fine. `Snapshot::pressure`
/// is an `Option` for exactly this reason, and the severity has to carry
/// the same admission through to the screen.
#[test]
fn memory_on_a_kernel_without_psi_is_unknown_rather_than_normal() {
    let snapshot = masys_domain::sample::Snapshot {
        pressure: None,
        memory: Some(masys_domain::sample::Memory {
            used_bytes: 29_400_000_000,
            total_bytes: 32_000_000_000,
            swap_free_bytes: 0,
            zram_percent: None,
        }),
        ..snapshot()
    };
    let mut status = StatusBuffer::default();
    status.refresh(
        &system_reporting(snapshot.clone()),
        &FakePlatformService::default(),
        Tick {
            previous: None,
            snapshot: &snapshot,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: 1_000_000,
        },
    );
    assert_eq!(
        status
            .overview
            .as_ref()
            .and_then(|o| o.mem_used_bytes)
            .expect("a memory reading")
            .severity,
        Severity::Unknown
    );
}

/// A fake that answers with one snapshot, for the tests that only care
/// what `refresh` makes of it.
fn system_reporting(snapshot: masys_domain::sample::Snapshot) -> FakeSystemService {
    FakeSystemService {
        snapshot,
        units: Vec::new(),
        calls: Default::default(),
        fails_with: None,
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
        units_fail_with: None,
        sample_fail_with: None,
        smart: None,
        smart_reads: Default::default(),
    }
}

/// One PSI reading colours one segment. A machine stalling on CPU says so
/// on the CPU segment and says nothing about memory - which is the whole
/// value of having three readings rather than one health light.
#[test]
fn each_segment_takes_its_severity_from_its_own_resource() {
    let snapshot = masys_domain::sample::Snapshot {
        pressure: Some(masys_domain::sample::Pressure {
            cpu_some: masys_domain::sample::PsiLine {
                avg60: 60.0,
                ..Default::default()
            },
            memory_some: masys_domain::sample::PsiLine {
                avg60: 1.0,
                ..Default::default()
            },
            ..Default::default()
        }),
        memory: Some(masys_domain::sample::Memory {
            used_bytes: 29_400_000_000,
            total_bytes: 32_000_000_000,
            swap_free_bytes: 0,
            zram_percent: None,
        }),
        ..snapshot()
    };
    let mut status = StatusBuffer::default();
    status.refresh(
        &system_reporting(snapshot.clone()),
        &FakePlatformService::default(),
        Tick {
            previous: None,
            snapshot: &snapshot,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: 1_000_000,
        },
    );
    let overview = status.overview.expect("an overview");

    assert_eq!(
        overview.cpu_pressure.expect("a cpu psi figure").severity,
        Severity::Warning,
        "60% of the last minute stalled on cpu"
    );
    assert_eq!(
        overview.mem_used_bytes.expect("a memory reading").severity,
        Severity::Normal,
        "cpu pressure is not memory's problem"
    );
    assert_eq!(
        overview
            .memory_pressure
            .expect("a memory psi figure")
            .severity,
        Severity::Normal
    );
}

/// Utilisation is a derivative, so the first tick has none. An operator
/// looking at a machine that has just started masys must not be told it
/// is idle.
#[test]
fn cpu_utilisation_needs_a_previous_sample() {
    let busy = |taken_at_ms, total_ticks, idle_ticks| masys_domain::sample::Snapshot {
        taken_at_ms,
        cpu_times: Some(masys_domain::sample::CpuTimes {
            total_ticks,
            idle_ticks,
        }),
        ..snapshot()
    };
    let first = busy(0, 1_000, 800);
    let second = busy(2_000, 1_400, 900);

    let reading = |previous: Option<&masys_domain::sample::Snapshot>,
                   current: &masys_domain::sample::Snapshot| {
        let mut status = StatusBuffer::default();
        status.refresh(
            &system_reporting(current.clone()),
            &FakePlatformService::default(),
            Tick {
                previous,
                snapshot: current,
                units: &[],
                units_unreadable: None,
                net_throughput: None,
                disk_throughput: None,
                now_ms: 1_000_000,
            },
        );
        status.overview.expect("an overview").cpu_percent
    };

    assert!(
        reading(None, &first).is_none(),
        "one sample is not a rate, and no figure is the honest answer"
    );
    let measured = reading(Some(&first), &second).expect("two samples");
    assert!(
        (measured.value - 75.0).abs() < 0.01,
        "expected 75%, got {}",
        measured.value
    );
}

/// A kernel with no PSI cannot judge the CPU either. The figure is real
/// and shown; the verdict on it is withheld.
#[test]
fn cpu_utilisation_without_psi_is_measured_but_unjudged() {
    let sample = |taken_at_ms, total_ticks, idle_ticks| masys_domain::sample::Snapshot {
        taken_at_ms,
        pressure: None,
        cpu_times: Some(masys_domain::sample::CpuTimes {
            total_ticks,
            idle_ticks,
        }),
        ..snapshot()
    };
    let first = sample(0, 1_000, 800);
    let second = sample(2_000, 1_400, 900);
    let mut status = StatusBuffer::default();
    status.refresh(
        &system_reporting(second.clone()),
        &FakePlatformService::default(),
        Tick {
            previous: Some(&first),
            snapshot: &second,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: 1_000_000,
        },
    );
    let overview = status.overview.expect("an overview");
    assert_eq!(
        overview.cpu_percent.expect("a cpu figure").severity,
        Severity::Unknown
    );
    assert!(
        overview.cpu_pressure.is_none(),
        "no PSI means no PSI figure - not a zero one"
    );
}

/// The three standing warnings, and the rule that they are warnings.
///
/// Each already decides its own finding or is a plain boolean fault, so
/// nothing new is being judged here - the header simply stops rendering
/// a failing disk in the same colour as the uptime beside it.
#[test]
fn the_standing_warnings_carry_a_severity() {
    let unsynchronised = masys_domain::sample::Snapshot {
        clock_synced: false,
        ..snapshot()
    };
    let mut status = StatusBuffer::default();
    status.refresh(
        &system_reporting(unsynchronised.clone()),
        &FakePlatformService {
            pending_reboot: Some(masys_domain::platform::PendingReboot {
                reason: "generation 412 not yet booted".to_string(),
            }),
            ..Default::default()
        },
        Tick {
            previous: None,
            snapshot: &unsynchronised,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: 1_000_000,
        },
    );
    let overview = status.overview.as_ref().expect("an overview");

    assert_eq!(
        overview.clock_synced.severity,
        Severity::Urgent,
        "an unsynchronised clock silently breaks TLS and journal ordering"
    );
    assert_eq!(
        overview
            .pending_reboot
            .as_ref()
            .expect("a pending reboot")
            .severity,
        Severity::Warning,
        "a standing condition rather than a fault"
    );
    // The agreement the ticket asks for: the segment says what the
    // finding says.
    assert!(
        status
            .findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::ClockUnsynchronized)),
        "{:#?}",
        status.findings
    );
}

/// And a healthy clock is not warned about, so the colour means
/// something when it appears.
#[test]
fn a_synchronised_clock_carries_no_warning_and_no_finding() {
    let mut status = StatusBuffer::default();
    let healthy = snapshot();
    status.refresh(
        &system_reporting(healthy.clone()),
        &FakePlatformService::default(),
        Tick {
            previous: None,
            snapshot: &healthy,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: 1_000_000,
        },
    );
    assert_eq!(
        status
            .overview
            .as_ref()
            .expect("an overview")
            .clock_synced
            .severity,
        Severity::Normal
    );
    assert!(
        !status
            .findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::ClockUnsynchronized))
    );
    assert!(
        status
            .overview
            .as_ref()
            .expect("an overview")
            .pending_reboot
            .is_none(),
        "no reboot pending, so nothing to warn about"
    );
}

fn logged(timestamp_ms: u64, unit: Option<&str>, message: &str, origin: Origin) -> Entry {
    Entry {
        timestamp_ms,
        unit: unit.map(str::to_string),
        priority: Priority::Error,
        message: message.to_string(),
        origin,
    }
}

/// Both sections, from one read, reaching the rows an operator sees.
#[test]
fn the_journal_sections_reach_the_status_buffer() {
    let now = 1_000_000_000;
    let snapshot = snapshot();
    let mut system = system_reporting(snapshot.clone());
    system.journal = vec![
        logged(
            now - 1_000,
            None,
            "EXT4-fs error (device sda1)",
            Origin::Kernel,
        ),
        logged(
            now - 2_000,
            Some("sshd.service"),
            "too many auth failures",
            Origin::Userspace,
        ),
    ];
    let mut status = StatusBuffer::default();
    status.refresh(
        &system,
        &FakePlatformService::default(),
        Tick {
            previous: None,
            snapshot: &snapshot,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: now,
        },
    );

    let titles: Vec<String> = status
        .rows()
        .iter()
        .filter_map(|row| match row {
            Node::SectionHeader { title, count, .. } => {
                Some(format!("{title}:{}", count.unwrap_or(0)))
            }
            _ => None,
        })
        .collect();
    assert!(titles.contains(&"Kernel:1".to_string()), "{titles:?}");
    assert!(
        titles.contains(&"Recent errors:1".to_string()),
        "{titles:?}"
    );
}

/// And a quiet host shows neither, which is what the buffer is for.
#[test]
fn a_host_with_a_quiet_journal_shows_neither_section() {
    let now = 1_000_000_000;
    let snapshot = snapshot();
    let mut status = StatusBuffer::default();
    status.refresh(
        &system_reporting(snapshot.clone()),
        &FakePlatformService::default(),
        Tick {
            previous: None,
            snapshot: &snapshot,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: now,
        },
    );
    let rows = status.rows();
    let titles: Vec<&str> = rows
        .iter()
        .filter_map(|row| match row {
            Node::SectionHeader { title, .. } => Some(title.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(titles, vec!["System"], "{titles:?}");
}

/// The cap bounds the rows and the heading still counts what there is.
/// `Recent errors (6)` above two rows says both that there are six and
/// that you are not seeing them all; `Recent errors (2)` would be a
/// count of the survivors presented as a count of the errors.
#[test]
fn a_capped_section_still_counts_what_it_did_not_show() {
    let now = 1_000_000_000;
    let wordings = [
        "cannot bind socket",
        "certificate expired",
        "disk quota exceeded",
        "upstream refused the connection",
        "template render failed",
        "no route to host",
    ];
    let snapshot = snapshot();
    let mut system = system_reporting(snapshot.clone());
    system.journal = wordings
        .iter()
        .enumerate()
        .map(|(i, m)| {
            logged(
                now - (i as u64 * 1_000),
                Some("a.service"),
                m,
                Origin::Userspace,
            )
        })
        .collect();
    let mut status = StatusBuffer {
        thresholds: Thresholds {
            journal_rows_per_section: 2,
            ..Thresholds::default()
        },
        ..Default::default()
    };
    status.refresh(
        &system,
        &FakePlatformService::default(),
        Tick {
            previous: None,
            snapshot: &snapshot,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: now,
        },
    );

    let rows = status.rows();
    let shown = rows
        .iter()
        .filter(|row| {
            matches!(
                row,
                Node::Finding {
                    finding: masys_domain::Finding {
                        kind: FindingKind::RecentError { .. },
                        ..
                    },
                    ..
                }
            )
        })
        .count();
    let counted = rows.iter().find_map(|row| match row {
        Node::SectionHeader {
            kind: SectionKind::RecentErrors,
            count,
            ..
        } => *count,
        _ => None,
    });
    assert_eq!(shown, 2, "the cap bounds what is drawn");
    assert_eq!(counted, Some(6), "and the heading says how many there are");
}

/// A journal that cannot be read is not a journal with nothing in it.
///
/// The failed read used to become an empty one, which produced no
/// findings and hid both sections - so an unreadable journal looked
/// exactly like a quiet host, on the buffer whose whole premise is that
/// quiet means healthy.
#[test]
fn a_failed_journal_read_keeps_what_was_last_read() {
    let now = 1_000_000_000;
    let snapshot = snapshot();
    let mut system = system_reporting(snapshot.clone());
    system.journal = vec![logged(
        now - 1_000,
        Some("sshd.service"),
        "auth failed",
        Origin::Userspace,
    )];
    let mut status = StatusBuffer::default();
    let tick = |now_ms| Tick {
        previous: None,
        snapshot: &snapshot,
        units: &[],
        units_unreadable: None,
        net_throughput: None,
        disk_throughput: None,
        now_ms,
    };
    status.refresh(&system, &FakePlatformService::default(), tick(now));
    assert!(
        status
            .findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::RecentError { .. })),
        "read once"
    );

    // The next tick's read fails outright.
    system.fails_with = Some("journald".to_string());
    status.refresh(&system, &FakePlatformService::default(), tick(now + 2_000));
    assert!(
        status
            .findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::RecentError { .. })),
        "and still reports what it last read, rather than that all is well"
    );
}

/// The buffer's premise is that no rows means nothing is wrong, so it
/// has to be able to say "I could not look".
#[test]
fn a_journal_that_cannot_be_read_is_reported_as_such() {
    let now = 1_000_000_000;
    let snapshot = snapshot();
    let mut system = system_reporting(snapshot.clone());
    system.fails_with = Some("journalctl: not found".to_string());
    let mut status = StatusBuffer::default();
    let tick = |now_ms| Tick {
        previous: None,
        snapshot: &snapshot,
        units: &[],
        units_unreadable: None,
        net_throughput: None,
        disk_throughput: None,
        now_ms,
    };
    status.refresh(&system, &FakePlatformService::default(), tick(now));

    let unreadable = status
        .findings
        .iter()
        .find_map(|f| match &f.kind {
            FindingKind::Unreadable {
                reason, since_ms, ..
            } => Some((reason.clone(), *since_ms)),
            _ => None,
        })
        .expect("a failed read says so");
    assert!(
        unreadable.0.contains("journalctl"),
        "what went wrong reaches the row: {}",
        unreadable.0
    );
    assert_eq!(unreadable.1, 0, "it has been failing since just now");
}

/// One failed read is noise; four minutes of them is a problem, and the
/// two must not look alike.
///
/// The clock advanced here is the *sample's*, not the wall clock: this
/// is the one age masys measures between two of its own ticks, so it
/// uses the monotonic reading and an NTP step cannot move it.
#[test]
fn a_journal_that_keeps_failing_says_how_long_it_has_been() {
    let now = 1_000_000_000;
    let at = |taken_at_ms| masys_domain::sample::Snapshot {
        taken_at_ms,
        ..snapshot()
    };
    let mut system = system_reporting(at(0));
    system.fails_with = Some("journalctl: not found".to_string());
    let mut status = StatusBuffer::default();

    let first = at(0);
    status.refresh(
        &system,
        &FakePlatformService::default(),
        Tick {
            previous: None,
            snapshot: &first,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: now,
        },
    );
    let later = at(240_000);
    status.refresh(
        &system,
        &FakePlatformService::default(),
        Tick {
            previous: None,
            snapshot: &later,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            // The wall clock stepping backwards mid-outage must not
            // change the answer, which is the whole reason this age
            // is not measured against it.
            now_ms: now - 3_600_000,
        },
    );
    assert!(
        status.findings.iter().any(
            |f| matches!(&f.kind, FindingKind::Unreadable { since_ms, .. } if *since_ms == 240_000)
        ),
        "{:#?}",
        status.findings
    );
}

/// The finding reaches a row, in a section of its own.
///
/// Asserted because deleting the section entry from the table left every
/// other test in this file passing: they all read `status.findings`,
/// which is the shape rather than the path to it. This is the test that
/// fails if the section stops being built.
#[test]
fn an_unreadable_journal_reaches_a_section_of_its_own() {
    let now = 1_000_000_000;
    let snapshot = snapshot();
    let mut system = system_reporting(snapshot.clone());
    system.fails_with = Some("journalctl: not found".to_string());
    let mut status = StatusBuffer::default();
    status.refresh(
        &system,
        &FakePlatformService::default(),
        Tick {
            previous: None,
            snapshot: &snapshot,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: now,
        },
    );

    let rows = status.rows();
    assert!(
        rows.iter().any(|row| matches!(
            row,
            Node::SectionHeader {
                kind: SectionKind::Unreadable,
                count: Some(1),
                ..
            }
        )),
        "{rows:#?}"
    );
    assert!(
        rows.iter().any(|row| matches!(
            row,
            Node::Finding {
                finding: masys_domain::Finding {
                    kind: FindingKind::Unreadable { .. },
                    ..
                },
                ..
            }
        )),
        "{rows:#?}"
    );
}

/// And a journal that comes back clears it - with the clock starting
/// over on the next failure rather than resuming where it left off,
/// which would report an outage that had already ended.
#[test]
fn a_journal_that_recovers_stops_being_reported() {
    let now = 1_000_000_000;
    let snapshot = snapshot();
    let mut system = system_reporting(snapshot.clone());
    let mut status = StatusBuffer::default();
    let tick = |now_ms| Tick {
        previous: None,
        snapshot: &snapshot,
        units: &[],
        units_unreadable: None,
        net_throughput: None,
        disk_throughput: None,
        now_ms,
    };

    system.fails_with = Some("journalctl: not found".to_string());
    status.refresh(&system, &FakePlatformService::default(), tick(now));
    system.fails_with = None;
    status.refresh(
        &system,
        &FakePlatformService::default(),
        tick(now + 240_000),
    );
    assert!(
        !status
            .findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::Unreadable { .. })),
        "a working journal is not reported"
    );

    system.fails_with = Some("journalctl: not found".to_string());
    status.refresh(
        &system,
        &FakePlatformService::default(),
        tick(now + 480_000),
    );
    assert!(
        status
            .findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::Unreadable { since_ms, .. } if *since_ms == 0)),
        "the clock starts over: {:#?}",
        status.findings
    );
}

/// A host whose journal reads fine shows no such section.
#[test]
fn a_readable_journal_produces_no_section_of_its_own() {
    let now = 1_000_000_000;
    let snapshot = snapshot();
    let mut status = StatusBuffer::default();
    status.refresh(
        &system_reporting(snapshot.clone()),
        &FakePlatformService::default(),
        Tick {
            previous: None,
            snapshot: &snapshot,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: now,
        },
    );
    let rows = status.rows();
    assert!(
        !rows.iter().any(|row| matches!(
            row,
            Node::SectionHeader {
                kind: SectionKind::Unreadable,
                ..
            }
        )),
        "{rows:#?}"
    );
}

/// A failed platform read costs that reading and nothing else. The pass
/// used to end here, leaving every finding and the whole overview
/// standing from the tick before while the header's clock kept moving.
#[test]
fn a_failed_platform_read_leaves_the_rest_of_the_pass_intact() {
    let now = 1_000_000_000;
    let snapshot = masys_domain::sample::Snapshot {
        clock_synced: false,
        ..snapshot()
    };
    let mut status = StatusBuffer::default();
    status.refresh(
        &system_reporting(snapshot.clone()),
        &FakePlatformService {
            fails_with: Some("nixos-rebuild: not found".to_string()),
            ..Default::default()
        },
        Tick {
            previous: None,
            snapshot: &snapshot,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: now,
        },
    );

    assert!(
        status
            .findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::ClockUnsynchronized)),
        "the rest of the pass still ran: {:#?}",
        status.findings
    );
    assert!(
        status.overview.is_some(),
        "and the overview was rebuilt rather than left standing"
    );
    assert!(
        status.findings.iter().any(|f| matches!(
            &f.kind,
            FindingKind::Unreadable {
                source: UnreadableSource::Platform,
                ..
            }
        )),
        "and the read that failed is reported: {:#?}",
        status.findings
    );
}

/// A pending reboot masys could not re-read keeps its last known value.
/// Absent renders as *no reboot pending*, so dropping it would answer a
/// question nobody asked it again.
#[test]
fn a_pending_reboot_survives_a_failed_platform_read() {
    let now = 1_000_000_000;
    let snapshot = snapshot();
    let mut status = StatusBuffer::default();
    let tick = |now_ms| Tick {
        previous: None,
        snapshot: &snapshot,
        units: &[],
        units_unreadable: None,
        net_throughput: None,
        disk_throughput: None,
        now_ms,
    };
    let pending = FakePlatformService {
        pending_reboot: Some(masys_domain::platform::PendingReboot {
            reason: "generation 412 not yet booted".to_string(),
        }),
        ..Default::default()
    };
    status.refresh(&system_reporting(snapshot.clone()), &pending, tick(now));
    assert!(
        status
            .overview
            .as_ref()
            .expect("an overview")
            .pending_reboot
            .is_some(),
        "read once"
    );

    status.refresh(
        &system_reporting(snapshot.clone()),
        &FakePlatformService {
            fails_with: Some("nixos-rebuild: not found".to_string()),
            ..Default::default()
        },
        tick(now + 2_000),
    );
    assert!(
        status
            .overview
            .as_ref()
            .expect("an overview")
            .pending_reboot
            .is_some(),
        "and still reported, rather than reading as a host with nothing pending"
    );
}

/// An unreadable unit list is not a host with no units. `unit_count`
/// would render the failure as the number zero, with nothing failed
/// beneath it.
#[test]
fn an_unreadable_unit_list_is_reported_rather_than_counted_as_zero() {
    let unreadable =
        masys_domain::error::MasysError::Command("systemctl: connection refused".to_string());
    let now = 1_000_000_000;
    let snapshot = snapshot();
    let mut status = StatusBuffer::default();
    status.refresh(
        &system_reporting(snapshot.clone()),
        &FakePlatformService::default(),
        Tick {
            previous: None,
            snapshot: &snapshot,
            units: &[],
            units_unreadable: Some(&unreadable),
            net_throughput: None,
            disk_throughput: None,
            now_ms: now,
        },
    );
    assert!(
        status.findings.iter().any(|f| matches!(
            &f.kind,
            FindingKind::Unreadable {
                source: UnreadableSource::Units,
                ..
            }
        )),
        "{:#?}",
        status.findings
    );
}

/// The caveat comes before the claims it qualifies. If the unit list
/// could not be read, `Failed units` being absent means nothing.
#[test]
fn the_unreadable_section_is_the_first_thing_on_the_buffer() {
    let unreadable =
        masys_domain::error::MasysError::Command("systemctl: connection refused".to_string());
    let now = 1_000_000_000;
    let snapshot = masys_domain::sample::Snapshot {
        clock_synced: false,
        ..snapshot()
    };
    let mut status = StatusBuffer::default();
    status.refresh(
        &system_reporting(snapshot.clone()),
        &FakePlatformService::default(),
        Tick {
            previous: None,
            snapshot: &snapshot,
            units: &[],
            units_unreadable: Some(&unreadable),
            net_throughput: None,
            disk_throughput: None,
            now_ms: now,
        },
    );
    let rows = status.rows();
    let kinds: Vec<SectionKind> = rows
        .iter()
        .filter_map(|row| match row {
            Node::SectionHeader { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    // First of the finding sections. `System` still leads the buffer:
    // it is not a finding but the block that says which host this is,
    // and it has been the "checks actually ran" marker since before any
    // of these sections existed.
    assert_eq!(
        kinds.first(),
        Some(&SectionKind::System),
        "the host still names itself first: {kinds:?}"
    );
    assert_eq!(
        kinds.get(1),
        Some(&SectionKind::Unreadable),
        "and the caveat comes before every claim it qualifies: {kinds:?}"
    );
}

/// A unit list nobody has managed to read is not a host with no units.
///
/// On any later tick the last good list stands, stale but counted, with
/// a row saying so. This is the first-tick case, where there is nothing
/// to stand in - and `0 units` there would be a number that looks like a
/// reading and came from a read that did not work.
#[test]
fn a_unit_count_nobody_has_taken_is_absent_rather_than_zero() {
    let now = 1_000_000_000;
    let snapshot = snapshot();
    let mut status = StatusBuffer::default();
    let unreadable = masys_domain::error::MasysError::Command("systemctl: refused".to_string());
    let refresh = |status: &mut StatusBuffer, units: &[masys_domain::unit::Unit], failed| {
        status.refresh(
            &system_reporting(snapshot.clone()),
            &FakePlatformService::default(),
            Tick {
                previous: None,
                snapshot: &snapshot,
                units,
                units_unreadable: failed,
                net_throughput: None,
                disk_throughput: None,
                now_ms: now,
            },
        );
    };

    refresh(&mut status, &[], Some(&unreadable));
    assert_eq!(
        status.overview.as_ref().expect("an overview").unit_count,
        None,
        "nothing has counted them"
    );

    // A host that genuinely has none still answers zero: an empty list
    // and a failed read are different things.
    refresh(&mut status, &[], None);
    assert_eq!(
        status.overview.as_ref().expect("an overview").unit_count,
        Some(0)
    );
}

/// The last good unit list survives a failed read, which is the whole
/// reason the count is not zero.
///
/// Asserted because blanking the list on failure passed every test:
/// they all checked that the row appeared, which is the shape of the
/// criterion rather than the whole of it.
#[test]
fn a_failed_unit_read_keeps_the_list_it_last_had() {
    let now = 1_000_000_000;
    let snapshot = snapshot();
    let unreadable = masys_domain::error::MasysError::Command("systemctl: refused".to_string());
    let units = vec![failed_unit("restic-backup.service")];
    let mut status = StatusBuffer::default();
    let mut refresh = |units: &[masys_domain::unit::Unit], failed| {
        status.refresh(
            &system_reporting(snapshot.clone()),
            &FakePlatformService::default(),
            Tick {
                previous: None,
                snapshot: &snapshot,
                units,
                units_unreadable: failed,
                net_throughput: None,
                disk_throughput: None,
                now_ms: now,
            },
        );
    };

    refresh(&units, None);
    refresh(&units, Some(&unreadable));
    assert_eq!(
        status.overview.as_ref().expect("an overview").unit_count,
        Some(1),
        "the list the session still holds is counted, not blanked"
    );
    assert!(
        status
            .findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::FailedUnit { .. })),
        "and what it says is still reported: {:#?}",
        status.findings
    );
}

/// Boot pressure and the pending reboot degrade differently on purpose,
/// and this is the case a fake that failed both together could not
/// reach.
///
/// A boot-pressure read that fails costs `/boot` its generation counts
/// and nothing else, because `None` there already means "say nothing
/// about generations" - true when the read failed. Only a reading whose
/// absence would be a claim gets a row.
#[test]
fn a_failed_boot_pressure_read_costs_only_what_it_enriched() {
    let now = 1_000_000_000;
    let snapshot = snapshot();
    let mut status = StatusBuffer::default();
    status.refresh(
        &system_reporting(snapshot.clone()),
        &FakePlatformService {
            boot_fails_with: Some("nixos-rebuild: not found".to_string()),
            ..Default::default()
        },
        Tick {
            previous: None,
            snapshot: &snapshot,
            units: &[],
            units_unreadable: None,
            net_throughput: None,
            disk_throughput: None,
            now_ms: now,
        },
    );
    assert!(
        !status
            .findings
            .iter()
            .any(|f| matches!(&f.kind, FindingKind::Unreadable { .. })),
        "an absence that is already honest is not reported: {:#?}",
        status.findings
    );
    assert!(status.overview.is_some(), "and the pass still ran");
}

/// That a finding reaches a *row*, which is a step further than the
/// compiler goes.
///
/// `section_of` being exhaustive means every variant is assigned a
/// section, so "claimed by nothing" is no longer representable - that
/// half of the question stopped needing a test on 2026-09-05. What is
/// left is the half a type cannot state: that the section a finding
/// lands in actually renders it. A section could be assigned and still
/// draw nothing.
#[test]
fn a_thermal_throttling_finding_reaches_a_row() {
    let rows = StatusBuffer {
        findings: vec![Finding::new(FindingKind::ThermalThrottling {
            percent: 42.0,
        })],
        overview: Some(overview()),
        ..Default::default()
    }
    .rows();
    assert!(
        rows.iter().any(|r| matches!(r, Node::Finding { .. })),
        "the finding was produced and no section drew it: {rows:#?}"
    );
}

/// And it shares the Pressure section rather than opening one of its own:
/// "why is this machine slow" is one question, and PSI and a held-back
/// clock are two answers to it.
#[test]
fn thermal_throttling_shares_the_pressure_section() {
    let rows = StatusBuffer {
        findings: vec![
            Finding::new(FindingKind::Pressure {
                resource: PressureResource::Cpu,
                some_avg60: 30.0,
                full_avg60: None,
                severity: Severity::Warning,
            }),
            Finding::new(FindingKind::ThermalThrottling { percent: 42.0 }),
        ],
        overview: Some(overview()),
        ..Default::default()
    }
    .rows();
    let headers = rows
        .iter()
        .filter(|r| {
            matches!(
                r,
                Node::SectionHeader {
                    kind: SectionKind::Pressure,
                    ..
                }
            )
        })
        .count();
    assert_eq!(headers, 1, "one Pressure section, not two: {rows:#?}");
}
