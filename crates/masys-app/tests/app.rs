mod fake;

use fake::{FakePlatformService, FakeSystemService};
use masys_app::{App, Key};
use masys_domain::platform::PendingReboot;
use masys_domain::sample::{LoadAverage, Memory, Pressure, Snapshot, SystemState};
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
    };
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());

    app.tick(10_000, "2026-05-20 09:14".to_string()).expect("tick");
    let view = app.view();

    assert_eq!(view.rows.len(), 2, "{:#?}", view.rows);
    assert!(matches!(view.header, Header::Status { hostname: "devbox", timestamp: "2026-05-20 09:14" }));
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
    };
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());

    app.tick(10_800_000, "2026-05-20 09:14".to_string()).expect("tick");
    let view = app.view();

    assert!(view.rows.iter().any(|r| matches!(r, Node::SectionHeader { kind: SectionKind::FailedUnits, .. })));
    assert!(view.rows.iter().any(|r| matches!(r, Node::SectionHeader { kind: SectionKind::Degraded, .. })));
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
    };
    let platform = FakePlatformService {
        boot_pressure: None,
        pending_reboot: Some(PendingReboot { reason: "generation 412 not yet booted".to_string() }),
    };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());

    app.tick(0, "t".to_string()).expect("tick");
    let view = app.view();

    let overview = view
        .rows
        .iter()
        .find_map(|r| match r {
            Node::Overview(o) => Some(o),
            _ => None,
        })
        .expect("an Overview row");
    assert_eq!(overview.pending_reboot.as_ref().map(|p| p.reason.as_str()), Some("generation 412 not yet booted"));
}

#[test]
fn measured_system_facts_reach_the_overview() {
    let mut snapshot = healthy_snapshot();
    snapshot.load = Some(LoadAverage { one: 1.82, five: 1.44, fifteen: 1.20 });
    snapshot.uptime_secs = Some(187_200);
    snapshot.memory =
        Some(Memory { used_bytes: 31_580_000_000, total_bytes: 34_360_000_000, swap_free_bytes: 0, zram_percent: Some(100.0) });

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
    };
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(10_000, "2026-05-20 09:14".to_string()).expect("tick");
    let view = app.view();

    let overview = view
        .rows
        .iter()
        .find_map(|r| match r {
            Node::Overview(o) => Some(o),
            _ => None,
        })
        .expect("an Overview row");
    assert_eq!((overview.load_1, overview.load_5, overview.load_15), (Some(1.82), Some(1.44), Some(1.20)));
    assert_eq!(overview.uptime_secs, Some(187_200));
    assert_eq!(overview.mem_used_bytes, Some(31_580_000_000));
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
    };
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(10_000, "2026-05-20 09:14".to_string()).expect("tick");
    let view = app.view();

    let overview = view
        .rows
        .iter()
        .find_map(|r| match r {
            Node::Overview(o) => Some(o),
            _ => None,
        })
        .expect("an Overview row");
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
    first.procs = vec![owned(fake::proc(1, "chrome", 0)), owned(fake::proc(2, "rust-analyzer", 0))];
    let mut second = first.clone();
    second.taken_at_ms = 1_000;
    // 50 ticks of CPU in one second at 100 Hz is 50%.
    second.procs = vec![owned(fake::proc(1, "chrome", 50)), owned(fake::proc(2, "rust-analyzer", 0))];

    let system = FakeSystemService::returning(vec![first, second]);
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());

    app.tick(1_000, "t".to_string()).expect("first tick");
    app.handle_key(Key::char('2')); // the procs view
    let first_tick = proc_rates(app.view().rows);
    assert_eq!(first_tick.len(), 2, "both processes have a row: {:#?}", app.view().rows);
    assert!(first_tick.iter().all(|r| r.is_none()), "but no rate exists yet after one sample");

    app.tick(2_000, "t".to_string()).expect("second tick");
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
    assert!((chrome.cpu_percent - 50.0).abs() < 0.01, "50 ticks in 1s at 100Hz is 50%, got {}", chrome.cpu_percent);
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
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(1_000, "t".to_string()).expect("first tick");
    app.tick(2_000, "t".to_string()).expect("second tick");

    let view = app.view();
    assert!(view.rows.iter().all(|r| !matches!(r, Node::Proc { .. })), "{:#?}", view.rows);
    // A healthy host: the System section and its body, and nothing else.
    assert_eq!(view.rows.len(), 2, "{:#?}", view.rows);
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
