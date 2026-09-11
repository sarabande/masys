mod fake;

use fake::{FakePlatformService, FakeSystemService};
use masys_app::{App, Key};
use masys_domain::finding::{FindingKind, Thresholds};
use masys_domain::platform::PendingReboot;
use masys_domain::sample::{Filesystem, LoadAverage, Memory, Pressure, Snapshot, SystemState};
use masys_domain::unit::{ActiveState, Unit, UnitKind};
use masys_view::{Header, Node, SectionKind};

fn healthy_snapshot() -> Snapshot {
    Snapshot {
        taken_at_ms: 0,
        procs: Vec::new(),
        pressure: Some(Pressure::default()),
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
    }
}

#[test]
fn a_healthy_host_produces_a_nearly_empty_view() {
    let system = FakeSystemService {
        snapshot: healthy_snapshot(),
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
    let platform = FakePlatformService::default();
    let mut app = App::new(
        Box::new(system),
        Box::new(platform),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );

    app.tick(10_000, "2026-05-20 09:14".to_string());
    let view = app.view();

    // The System section and nothing else - a header plus one row per
    // line the overview has to draw. Asserted by shape rather than by a
    // count, because how many lines a host fills depends on what it
    // answered, and this test is about the sections that are *absent*.
    assert!(
        view.rows.iter().all(|row| matches!(
            row,
            Node::SectionHeader {
                kind: SectionKind::System,
                ..
            } | Node::OverviewLine { .. }
        )),
        "{:#?}",
        view.rows
    );
    assert!(matches!(
        view.header,
        Header::Status {
            hostname: "devbox",
            timestamp: "2026-05-20 09:14"
        }
    ));
    // A non-empty buffer always has a cursor - the first selectable row.
    assert_eq!(view.selected, Some(0));
    assert!(view.modal.is_none());
}

#[test]
fn a_failed_unit_surfaces_as_a_finding_row() {
    let mut snapshot = healthy_snapshot();
    snapshot.system_state = SystemState::Degraded;
    let units = vec![Unit {
        name: "restic-backup.service".to_string(),
        kind: UnitKind::Service,
        active_state: ActiveState::Failed,
        sub_state: "failed".to_string(),
        exit_code: Some(1),
        enabled: true,
        restart_timestamps_ms: Vec::new(),
        since_ms: 0,
        cgroup: None,
        slice: None,
        triggers: Vec::new(),
        timer: None,
    }];
    let system = FakeSystemService {
        snapshot,
        units,
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
    let mut app = App::new(
        Box::new(system),
        Box::new(platform),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );

    app.tick(10_800_000, "2026-05-20 09:14".to_string());
    let view = app.view();

    assert!(view.rows.iter().any(|r| matches!(
        r,
        Node::SectionHeader {
            kind: SectionKind::FailedUnits,
            ..
        }
    )));
    assert!(view.rows.iter().any(|r| matches!(
        r,
        Node::SectionHeader {
            kind: SectionKind::Degraded,
            ..
        }
    )));
}

#[test]
fn pending_reboot_flows_into_the_overview() {
    let system = FakeSystemService {
        snapshot: healthy_snapshot(),
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
    let platform = FakePlatformService {
        pending_reboot: Some(PendingReboot {
            reason: "generation 412 not yet booted".to_string(),
        }),
        ..Default::default()
    };
    let mut app = App::new(
        Box::new(system),
        Box::new(platform),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );

    app.tick(0, "t".to_string());
    let view = app.view();

    let overview = view
        .rows
        .iter()
        .find_map(|r| match r {
            Node::OverviewLine { overview, .. } => Some(overview),
            _ => None,
        })
        .expect("an overview line");
    assert_eq!(
        overview
            .pending_reboot
            .as_ref()
            .map(|p| p.value.reason.as_str()),
        Some("generation 412 not yet booted")
    );
}

#[test]
fn measured_system_facts_reach_the_overview() {
    let mut snapshot = healthy_snapshot();
    snapshot.load = Some(LoadAverage {
        one: 1.82,
        five: 1.44,
        fifteen: 1.20,
    });
    snapshot.uptime_secs = Some(187_200);
    snapshot.memory = Some(Memory {
        used_bytes: 31_580_000_000,
        total_bytes: 34_360_000_000,
        swap_free_bytes: 0,
        zram_percent: Some(100.0),
    });

    let system = FakeSystemService {
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
    };
    let platform = FakePlatformService::default();
    let mut app = App::new(
        Box::new(system),
        Box::new(platform),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(10_000, "2026-05-20 09:14".to_string());
    let view = app.view();

    let overview = view
        .rows
        .iter()
        .find_map(|r| match r {
            Node::OverviewLine { overview, .. } => Some(overview),
            _ => None,
        })
        .expect("an overview line");
    assert_eq!(
        (overview.load_1, overview.load_5, overview.load_15),
        (Some(1.82), Some(1.44), Some(1.20))
    );
    assert_eq!(overview.uptime_secs, Some(187_200));
    assert_eq!(
        overview.mem_used_bytes.map(|r| r.value),
        Some(31_580_000_000)
    );
    assert_eq!(overview.mem_total_bytes, Some(34_360_000_000));
    assert_eq!(overview.swap_free_bytes, Some(0));
    assert_eq!(overview.zram_percent, Some(100.0));
}

#[test]
fn unmeasured_system_facts_stay_none_rather_than_reading_as_zero() {
    let system = FakeSystemService {
        snapshot: healthy_snapshot(),
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
    let platform = FakePlatformService::default();
    let mut app = App::new(
        Box::new(system),
        Box::new(platform),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(10_000, "2026-05-20 09:14".to_string());
    let view = app.view();

    let overview = view
        .rows
        .iter()
        .find_map(|r| match r {
            Node::OverviewLine { overview, .. } => Some(overview),
            _ => None,
        })
        .expect("an overview line");
    // An adapter that cannot measure these must leave them unset - the
    // renderer omits the whole line rather than printing "load 0.00".
    assert_eq!(overview.load_1, None);
    assert_eq!(overview.uptime_secs, None);
    assert_eq!(overview.mem_total_bytes, None);
    assert_eq!(overview.zram_percent, None);
    assert_eq!(overview.swap_free_bytes, None);
}

/// CPU% is a derivative: it exists only between two samples. A process
/// row therefore has no rate at all on the first tick, and the renderer
/// draws `-` rather than `0.0%` - "not yet measured" and "genuinely
/// idle" have to stay distinguishable.
#[test]
fn a_process_rate_appears_only_after_a_second_tick() {
    let mut first = healthy_snapshot();
    first.taken_at_ms = 0;
    // In a real cgroup rather than the kernel bucket, which the session
    // opens collapsed - rows inside it would not be built at all, and the
    // assertions below would pass over an empty list.
    first.procs = vec![
        owned(fake::proc(1, "chrome", 0)),
        owned(fake::proc(2, "rust-analyzer", 0)),
    ];
    let mut second = first.clone();
    second.taken_at_ms = 1_000;
    // 50 ticks of CPU in one second at 100 Hz is 50%.
    second.procs = vec![
        owned(fake::proc(1, "chrome", 50)),
        owned(fake::proc(2, "rust-analyzer", 0)),
    ];

    let system = FakeSystemService::returning(vec![first, second]);
    let platform = FakePlatformService::default();
    let mut app = App::new(
        Box::new(system),
        Box::new(platform),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );

    app.tick(1_000, "t".to_string());
    app.handle_key(Key::char('2')); // the procs buffer
    let first_tick = proc_rates(app.view().rows);
    assert_eq!(
        first_tick.len(),
        2,
        "both processes have a row: {:#?}",
        app.view().rows
    );
    assert!(
        first_tick.iter().all(|r| r.is_none()),
        "but no rate exists yet after one sample"
    );

    app.tick(2_000, "t".to_string());
    let view = app.view();
    let chrome = view
        .rows
        .iter()
        .find_map(|r| match r {
            Node::Proc { proc, rate, .. } if proc.comm == "chrome" => Some(*rate),
            _ => None,
        })
        .flatten()
        .expect("a measured rate");
    assert!(
        (chrome.cpu_percent - 50.0).abs() < 0.01,
        "50 ticks in 1s at 100Hz is 50%, got {}",
        chrome.cpu_percent
    );
}

/// The status buffer is triage. It answers "what is wrong", and a ranking
/// of the busiest processes is not that - it showed on a perfectly
/// healthy host, which is the one thing every other section here refuses
/// to do. The Procs buffer is where "what is this machine busy with"
/// belongs, and it is one key away.
#[test]
fn the_status_buffer_ranks_no_processes() {
    let mut first = healthy_snapshot();
    first.procs = vec![owned(fake::proc(1, "chrome", 0))];
    let mut second = first.clone();
    second.taken_at_ms = 1_000;
    second.procs = vec![owned(fake::proc(1, "chrome", 50))];

    let system = FakeSystemService::returning(vec![first, second]);
    let platform = FakePlatformService::default();
    let mut app = App::new(
        Box::new(system),
        Box::new(platform),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(1_000, "t".to_string());
    app.tick(2_000, "t".to_string());

    let view = app.view();
    assert!(
        view.rows.iter().all(|r| !matches!(r, Node::Proc { .. })),
        "{:#?}",
        view.rows
    );
    // A healthy host: the System section and its body, and nothing else.
    // The System section and nothing else - a header plus one row per
    // line the overview has to draw. Asserted by shape rather than by a
    // count, because how many lines a host fills depends on what it
    // answered, and this test is about the sections that are *absent*.
    assert!(
        view.rows.iter().all(|row| matches!(
            row,
            Node::SectionHeader {
                kind: SectionKind::System,
                ..
            } | Node::OverviewLine { .. }
        )),
        "{:#?}",
        view.rows
    );
}

/// A process in a real cgroup, so the Procs buffer files it under a group
/// that is expanded by default.
fn owned(mut p: masys_domain::sample::Proc) -> masys_domain::sample::Proc {
    p.cgroup = Some("/user.slice".to_string());
    p
}

fn proc_rates(rows: &[Node]) -> Vec<Option<masys_domain::rate::ProcRate>> {
    rows.iter()
        .filter_map(|r| match r {
            Node::Proc { rate, .. } => Some(*rate),
            _ => None,
        })
        .collect()
}

/// A threshold set on the session reaches the rules that evaluate it.
///
/// The composition root reads `[thresholds]` and hands the result here,
/// and nothing between the two is obvious: `Thresholds` lives on the
/// Status buffer, three layers from the key that sets it. Making
/// `set_thresholds` do nothing failed no test before this one - the
/// parsing was covered, the rules were covered, and the wire between
/// them was not.
///
/// A filesystem at 60% is quiet under the design's 85 and a finding under
/// a 50 somebody wrote down.
#[test]
fn thresholds_set_on_the_session_reach_triage() {
    let half_full = Filesystem {
        mount_point: "/".to_string(),
        used_percent: 60.0,
        free_bytes: 1 << 30,
        inode_used_percent: 5.0,
        read_only: false,
    };
    let findings_with = |thresholds: Option<Thresholds>| {
        let system = FakeSystemService {
            snapshot: Snapshot {
                filesystems: vec![half_full.clone()],
                ..healthy_snapshot()
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
        let platform = FakePlatformService::default();
        let mut app = App::new(
            Box::new(system),
            Box::new(platform),
            Box::new(fake::NoScanner),
            "devbox".to_string(),
        );
        if let Some(thresholds) = thresholds {
            app.set_thresholds(thresholds);
        }
        app.tick(10_000, "t".to_string());
        app.view()
            .rows
            .iter()
            .filter(|row| {
                matches!(
                    row,
                    Node::Finding {
                        finding: masys_domain::Finding {
                            kind: FindingKind::DiskCapacity { .. },
                            ..
                        },
                        ..
                    }
                )
            })
            .count()
    };

    assert_eq!(
        findings_with(None),
        0,
        "85 is the design's number and 60% is quiet under it"
    );
    assert_eq!(
        findings_with(Some(Thresholds {
            disk_used_percent: 50.0,
            ..Thresholds::default()
        })),
        1,
        "a threshold the operator wrote down has to reach the rule"
    );
}

/// A sample that fails ends the tick — it is the one read with nothing
/// to degrade to — and says so where the operator is looking.
///
/// Untested when it landed: the fake's `sample` answered `Ok`
/// unconditionally, so the whole branch could be deleted for a bare
/// `return` with the suite still green. That is the shape the style doc
/// records as a fail-safe nothing holds.
#[test]
fn a_failed_sample_ends_the_tick_and_reports_it() {
    let mut system = FakeSystemService::returning(vec![healthy_snapshot()]);
    system.sample_fail_with = Some("/proc: permission denied".to_string());
    let mut app = App::new(
        Box::new(system),
        Box::new(FakePlatformService::default()),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(1_000, "t".to_string());

    let view = app.view();
    assert!(
        matches!(view.status, masys_view::StatusLine::Error(message) if message.contains("/proc")),
        "the failure is where the operator is looking"
    );
    assert!(
        view.rows.is_empty(),
        "and the pass did not happen: {:#?}",
        view.rows
    );
}

/// And it stops being said once the sample comes back, rather than
/// asserting a failure the latest reading contradicts.
#[test]
fn a_recovered_sample_clears_what_the_failure_said() {
    let mut system = FakeSystemService::returning(vec![healthy_snapshot()]);
    system.sample_fail_with = Some("/proc: permission denied".to_string());
    let mut app = App::new(
        Box::new(system),
        Box::new(FakePlatformService::default()),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(1_000, "t".to_string());
    assert!(matches!(
        app.view().status,
        masys_view::StatusLine::Error(_)
    ));

    let mut working = FakeSystemService::returning(vec![healthy_snapshot()]);
    working.sample_fail_with = None;
    app.replace_system(Box::new(working));
    app.tick(3_000, "t".to_string());
    assert!(
        matches!(app.view().status, masys_view::StatusLine::Hints),
        "a recovered sample says nothing"
    );
}

/// The session keeps the unit list it last read, so a failed read costs
/// the reading and not the list.
///
/// At this seam rather than the buffer's: `StatusBuffer` is handed
/// whatever units the session decides to pass, so a test there cannot
/// tell whether the session kept them. Blanking `self.units` on failure
/// passed every test in the suite until this one.
#[test]
fn a_failed_unit_read_leaves_the_session_holding_the_last_list() {
    let mut system = FakeSystemService::returning(vec![healthy_snapshot()]);
    system.units = vec![Unit {
        name: "restic-backup.service".to_string(),
        kind: UnitKind::Service,
        active_state: ActiveState::Failed,
        sub_state: "failed".to_string(),
        exit_code: Some(1),
        enabled: true,
        restart_timestamps_ms: Vec::new(),
        since_ms: 0,
        cgroup: None,
        slice: None,
        triggers: Vec::new(),
        timer: None,
    }];
    let mut app = App::new(
        Box::new(system),
        Box::new(FakePlatformService::default()),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(1_000, "t".to_string());

    let mut refusing = FakeSystemService::returning(vec![healthy_snapshot()]);
    refusing.units = Vec::new();
    refusing.units_fail_with = Some("systemctl: connection refused".to_string());
    app.replace_system(Box::new(refusing));
    app.tick(3_000, "t".to_string());

    let overview = app
        .view()
        .rows
        .iter()
        .find_map(|row| match row {
            Node::OverviewLine { overview, .. } => Some(overview.clone()),
            _ => None,
        })
        .expect("an overview line");
    assert_eq!(
        overview.unit_count,
        Some(1),
        "the list the session last read is still counted"
    );
}

/// **The overview's throughput is the IO buffer's throughput, not a
/// second sum over the same samples.**
///
/// #25 asked that "the figures agree with what the IO buffer shows for
/// the same tick", and nothing checked it: every `Overview` carrying
/// throughput in the suite is hand-built by a render test, and a
/// hand-built pair agrees with itself by construction.
///
/// What this pins is that the two share a derivation. `App::tick`
/// refreshes the IO buffer first and hands its totals to the Status
/// buffer, which is what lets one of them skip loopback without the
/// other having to know: the interfaces the total sums are exactly the
/// interfaces the buffer shows. Two independent sums would agree today
/// and diverge the first time either learned a new exclusion.
#[test]
fn the_overview_totals_are_what_the_io_buffer_shows() {
    let iface = |name: &str, rx: u64, tx: u64, loopback: bool| masys_domain::sample::Interface {
        name: name.to_string(),
        rx_bytes: rx,
        tx_bytes: tx,
        rx_packets: 0,
        tx_packets: 0,
        rx_errs: 0,
        tx_errs: 0,
        rx_drop: 0,
        tx_drop: 0,
        up: true,
        loopback,
    };
    let disk = |name: &str, read: u64, write: u64| masys_domain::sample::Disk {
        name: name.to_string(),
        reads: 0,
        writes: 0,
        read_sectors: read,
        write_sectors: write,
        io_ms: 0,
    };
    let at = |ms: u64, n: u64| {
        let mut snapshot = healthy_snapshot();
        snapshot.taken_at_ms = ms;
        snapshot.interfaces = vec![
            iface("eth0", 4_000_000 * n, 1_000_000 * n, false),
            iface("wlan0", 500_000 * n, 250_000 * n, false),
            // Carries more than both put together, and belongs in
            // neither figure: it is not a network path.
            iface("lo", 90_000_000 * n, 90_000_000 * n, true),
        ];
        snapshot.disks = vec![
            disk("sda", 20_000 * n, 8_000 * n),
            disk("sdb", 4_000 * n, 900 * n),
        ];
        snapshot
    };

    let mut app = App::new(
        Box::new(FakeSystemService::returning(vec![at(0, 0), at(1_000, 1)])),
        Box::new(FakePlatformService::default()),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    // Twice, because a throughput is a difference and one sample has none.
    app.tick(1_000, "t".to_string());
    app.tick(2_000, "t".to_string());

    let overview = app
        .view()
        .rows
        .iter()
        .find_map(|r| match r {
            Node::OverviewLine { overview, .. } => Some(overview.clone()),
            _ => None,
        })
        .expect("an overview line");
    let net = overview.net_throughput.expect("a network figure");
    let disks = overview.disk_throughput.expect("a disk figure");

    app.handle_key(Key::char('4'));
    let shown = app.view();
    let (mut net_in, mut net_out, mut disk_in, mut disk_out) = (0.0, 0.0, 0.0, 0.0);
    let mut names = Vec::new();
    for row in shown.rows.iter() {
        match row {
            Node::Interface { interface, rate } => {
                names.push(interface.name.clone());
                if let Some(rate) = rate {
                    net_in += rate.rx_bytes_per_sec;
                    net_out += rate.tx_bytes_per_sec;
                }
            }
            Node::Disk {
                rate: Some(rate), ..
            } => {
                disk_in += rate.read_bytes_per_sec;
                disk_out += rate.write_bytes_per_sec;
            }
            _ => {}
        }
    }

    assert!(
        !names.iter().any(|n| n == "lo"),
        "loopback is not shown, so it must not be in the total either: {names:?}"
    );
    // Summed in a different order than `Throughput::of_nets` folds them -
    // the rows are sorted by throughput - so this is float-equal rather
    // than bit-equal.
    let close = |a: f64, b: f64| (a - b).abs() < 1.0;
    assert!(
        close(net.in_bytes_per_sec, net_in) && close(net.out_bytes_per_sec, net_out),
        "the header's network figure is the rows it shows: {net:?} vs {net_in}/{net_out}"
    );
    assert!(
        close(disks.in_bytes_per_sec, disk_in) && close(disks.out_bytes_per_sec, disk_out),
        "and the same for disks: {disks:?} vs {disk_in}/{disk_out}"
    );
    assert!(
        net_in > 0.0 && disk_in > 0.0,
        "and both moved something, or this agrees about nothing"
    );
}

/// A host whose port gives `smart` as its SMART answer, ticked once.
fn host_reporting_smart(smart: Option<bool>) -> App {
    let mut system = FakeSystemService::returning(vec![healthy_snapshot()]);
    system.smart = smart;
    let mut app = App::new(
        Box::new(system),
        Box::new(FakePlatformService::default()),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(10_000, "t".to_string());
    app
}

fn smart_segment(app: &App) -> Option<masys_view::Reading<bool>> {
    app.view().rows.iter().find_map(|r| match r {
        Node::OverviewLine { overview, .. } => overview.smart_ok,
        _ => None,
    })
}

fn has_smart_finding(app: &App) -> bool {
    app.view().rows.iter().any(|r| {
        matches!(
            r,
            Node::Finding {
                finding: masys_domain::Finding {
                    kind: FindingKind::SmartFailing,
                    ..
                },
                ..
            }
        )
    })
}

#[test]
fn a_failing_smart_status_reaches_the_overview_and_raises_a_finding() {
    let app = host_reporting_smart(Some(false));
    let segment = smart_segment(&app).expect("a smart segment on a host that answered");
    assert!(!segment.value, "the disk says it expects to fail");
    assert_eq!(
        segment.severity,
        masys_domain::finding::Severity::Urgent,
        "which is the most actionable thing this tool can say"
    );
    assert!(
        has_smart_finding(&app),
        "and the finding is present, which is what the segment's severity agrees with"
    );
}

#[test]
fn a_passing_smart_status_reads_as_normal_and_raises_nothing() {
    let app = host_reporting_smart(Some(true));
    let segment = smart_segment(&app).expect("a smart segment on a host that answered");
    assert!(segment.value);
    assert_eq!(segment.severity, masys_domain::finding::Severity::Normal);
    assert!(
        !has_smart_finding(&app),
        "nothing is wrong, so nothing is reported"
    );
}

/// **The case that is most hosts, and the one worth getting right.**
///
/// No smartctl, no root, a virtual disk, a disk with no SMART data: all
/// of them answer `None`, and `None` draws no segment at all. A healthy
/// disk and a disk nobody asked must not render alike - the second is a
/// claim about a reading masys never took, and it is the one an operator
/// would act on.
#[test]
fn a_host_masys_cannot_ask_about_smart_draws_no_segment() {
    let app = host_reporting_smart(None);
    // The overview first. Without this the assertion below passes on a
    // buffer that drew no overview at all, which is a different bug
    // wearing this test's name.
    let overview = app
        .view()
        .rows
        .iter()
        .find_map(|r| match r {
            Node::OverviewLine { overview, .. } => Some(overview.clone()),
            _ => None,
        })
        .expect("an overview, so that an absent segment means an absent segment");
    assert!(
        overview.smart_ok.is_none(),
        "unknown is not healthy, and it is not failing either"
    );
    assert!(!has_smart_finding(&app));
}

/// **The SMART read does not happen on every tick, and the tick is two
/// seconds.**
///
/// Every other reading the Status buffer takes is a file in `/proc` or
/// `/sys`. This one is a `smartctl` process per disk, so asking it at the
/// ordinary cadence would spawn subprocesses forever to watch a number
/// that changes on the timescale of a disk dying.
///
/// Measured on the sample's clock, which is why the snapshots below carry
/// advancing `taken_at_ms` rather than the tick just being called with a
/// later wall clock.
#[test]
fn the_smart_read_is_not_taken_every_tick() {
    let at = |taken_at_ms| Snapshot {
        taken_at_ms,
        ..healthy_snapshot()
    };
    let mut system =
        FakeSystemService::returning(vec![at(0), at(2_000), at(4_000), at(400_000), at(402_000)]);
    system.smart = Some(true);
    let reads = system.smart_reads.clone();
    let mut app = App::new(
        Box::new(system),
        Box::new(FakePlatformService::default()),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );

    app.tick(0, "t".to_string());
    assert_eq!(reads.get(), 1, "asked once, on the first tick");
    app.tick(2_000, "t".to_string());
    app.tick(4_000, "t".to_string());
    assert_eq!(
        reads.get(),
        1,
        "and not again four seconds later, nor on any tick inside the window"
    );
    assert!(
        smart_segment(&app).is_some(),
        "the held answer is still shown - not asking is not the same as forgetting"
    );

    // Past the window, on the sample's clock.
    app.tick(400_000, "t".to_string());
    assert_eq!(reads.get(), 2, "asked again once the answer has aged out");
    app.tick(402_000, "t".to_string());
    assert_eq!(reads.get(), 2, "and the window starts over");
}

/// **A buffer that shrinks for a tick and grows back puts the cursor
/// where it was.**
///
/// #48, and the half of it #40 could not reach. `rebuild` has to clamp -
/// `View::selected` hands the renderer an index and an out-of-range one
/// panics - but the clamp was *stored*, so a value computed to keep one
/// frame drawable replaced where the operator had actually gone. A list
/// that briefly comes back shorter, and the cursor is near the top
/// forever after.
///
/// The distinction the fix encodes: what is remembered is where they
/// went, and what is drawn is what this frame can legally show.
#[test]
fn a_buffer_that_shrinks_for_a_tick_puts_the_cursor_back() {
    let proc = |n: u32| masys_domain::sample::Proc {
        pid: 100 + n,
        comm: format!("svc-{n:02}"),
        cgroup: Some(format!("/system.slice/svc-{n:02}.service")),
        cpu_ticks: 0,
        rss_bytes: 1_000,
        io_read_bytes: 0,
        io_write_bytes: 0,
        state: masys_domain::sample::ProcState::Running,
        nice: 0,
        oom_score: 0,
        threads: 1,
        started_at_ms: 0,
    };
    let host = |taken_at_ms, count: u32| {
        let mut snapshot = healthy_snapshot();
        snapshot.taken_at_ms = taken_at_ms;
        snapshot.procs = (0..count).map(proc).collect();
        snapshot
    };

    let mut app = App::new(
        // Twelve processes, then one, then twelve again.
        Box::new(FakeSystemService::returning(vec![
            host(0, 12),
            host(1_000, 1),
            host(2_000, 12),
        ])),
        Box::new(FakePlatformService::default()),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(1_000, "t".to_string());
    app.handle_key(Key::char('2'));

    // Down to a row only the long list has.
    for _ in 0..9 {
        app.handle_key(Key::new(masys_app::key::KeyCode::Down));
    }
    let parked = app.view().selected.expect("a cursor");
    let long_rows = app.view().rows.len();
    assert!(
        parked >= 8,
        "parked deep enough to be clamped away: {parked}"
    );

    app.tick(2_000, "t".to_string());
    let shrunk = app.view().selected.expect("a cursor on the short buffer");
    assert!(
        shrunk < parked,
        "the short buffer draws a legal row, which is the clamp doing its job"
    );

    app.tick(3_000, "t".to_string());
    // The buffer refilled, judged before the position is. Without this
    // the assertion below is satisfied by a buffer that never came back -
    // and since what is restored is an index, "the same row" would follow
    // from "the same index" and could never fail on its own.
    assert_eq!(
        app.view().rows.len(),
        long_rows,
        "the long buffer is back, so there is a row at {parked} to be on"
    );
    assert_eq!(
        app.view().selected,
        Some(parked),
        "and the cursor is on the row the operator was on"
    );
}

/// **A move made from a clamped position starts from where the cursor is
/// drawn, not from the row it is remembering.**
///
/// #48's third criterion, and the one the round-trip test above cannot
/// reach: there, the operator never moves while the buffer is short. The
/// two positions are equal on every ordinary move, so `step` reading the
/// remembered index instead of the drawn one is invisible until a buffer
/// has shrunk under the cursor - and then it is a keypress that jumps
/// somewhere nobody can see, or lands past the end of the rows.
#[test]
fn a_move_from_a_clamped_cursor_steps_from_the_row_on_screen() {
    let proc = |n: u32| masys_domain::sample::Proc {
        pid: 100 + n,
        comm: format!("svc-{n:02}"),
        cgroup: Some(format!("/system.slice/svc-{n:02}.service")),
        cpu_ticks: 0,
        rss_bytes: 1_000,
        io_read_bytes: 0,
        io_write_bytes: 0,
        state: masys_domain::sample::ProcState::Running,
        nice: 0,
        oom_score: 0,
        threads: 1,
        started_at_ms: 0,
    };
    let host = |taken_at_ms, count: u32| {
        let mut snapshot = healthy_snapshot();
        snapshot.taken_at_ms = taken_at_ms;
        snapshot.procs = (0..count).map(proc).collect();
        snapshot
    };

    let mut app = App::new(
        Box::new(FakeSystemService::returning(vec![
            host(0, 12),
            host(1_000, 1),
        ])),
        Box::new(FakePlatformService::default()),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(1_000, "t".to_string());
    app.handle_key(Key::char('2'));
    for _ in 0..9 {
        app.handle_key(Key::new(masys_app::key::KeyCode::Down));
    }
    let parked = app.view().selected.expect("a cursor");
    assert!(parked >= 8, "deep enough to be clamped away: {parked}");

    // The buffer shrinks under the cursor. What is drawn is the last
    // legal row; what is remembered is still `parked`.
    app.tick(2_000, "t".to_string());
    let drawn = app.view().selected.expect("a cursor on the short buffer");
    let short_rows = app.view().rows.len();
    assert!(
        drawn < parked && drawn + 1 == short_rows,
        "clamped to the last legal row: drawn {drawn} of {short_rows}"
    );

    // One step up. From the drawn row, so it lands inside the short
    // buffer - stepping from `parked` would aim at row 8 of a buffer that
    // has two.
    app.handle_key(Key::new(masys_app::key::KeyCode::Up));
    let after = app.view().selected.expect("a cursor after moving");
    assert!(
        after < drawn,
        "moved up from the drawn row {drawn}, not from the remembered {parked}: {after}"
    );
    assert!(
        after < short_rows,
        "and stayed inside a buffer of {short_rows} rows: {after}"
    );
}
