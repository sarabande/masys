//! Finding the row a jump names.
//!
//! The lookup `go_back` has always needed and the finding drill-down
//! needs too. It was a two-arm match written inline inside `go_back`,
//! covering units and timers; a second copy for filesystems and processes
//! would be the shape that once had the footer and the registry
//! disagreeing about which buffers a host had.

use masys_app::jump::{RowTarget, row_named};
use masys_domain::rate::ProcRate;
use masys_domain::sample::{Filesystem, Proc, ProcState};
use masys_domain::unit::{ActiveState, Unit, UnitKind};
use masys_view::Node;

fn unit_row(name: &str) -> Node {
    Node::Unit {
        unit: Unit {
            name: name.to_string(),
            kind: UnitKind::Service,
            active_state: ActiveState::Active,
            sub_state: "running".to_string(),
            exit_code: None,
            enabled: true,
            restart_timestamps_ms: Vec::new(),
            since_ms: 0,
            cgroup: None,
            slice: None,
            timer: None,
            triggers: Vec::new(),
        },
        age_ms: None,
        expanded: false,
        depth: 0,
        children: false,
    }
}

fn timer_row(name: &str) -> Node {
    Node::Timer {
        name: name.to_string(),
        activates: "backup.service".to_string(),
        next_in_ms: None,
        last_ago_ms: None,
        children: false,
        activated_failed: false,
    }
}

fn filesystem_row(mount_point: &str) -> Node {
    Node::Filesystem {
        filesystem: Filesystem {
            mount_point: mount_point.to_string(),
            used_percent: 88.0,
            free_bytes: 2_100_000_000,
            inode_used_percent: 3.0,
            read_only: false,
        },
        expanded: false,
    }
}

fn proc_row(pid: u32, comm: &str) -> Node {
    Node::Proc {
        proc: Proc {
            pid,
            comm: comm.to_string(),
            cgroup: None,
            cpu_ticks: 100,
            rss_bytes: 1_000_000,
            io_read_bytes: 0,
            io_write_bytes: 0,
            state: ProcState::Running,
            nice: 0,
            oom_score: 0,
            threads: 1,
            started_at_ms: 0,
        },
        rate: None::<ProcRate>,
        cpu_seconds: 1,
        age_ms: None,
        depth: 0,
        expanded: false,
    }
}

/// A unit, by name.
#[test]
fn a_unit_row_is_found_by_its_name() {
    let rows = vec![
        unit_row("sshd.service"),
        unit_row("nginx.service"),
        unit_row("cron.service"),
    ];
    assert_eq!(
        row_named(&rows, &RowTarget::Unit("nginx.service".to_string())),
        Some(1)
    );
}

/// **A timer is a unit.** `go_back` has always matched both, because `l`
/// on a timer row has to come back to that timer - and a target that
/// knew only about `Node::Unit` would silently fail to return there.
#[test]
fn a_timer_row_is_found_by_the_same_target() {
    let rows = vec![unit_row("sshd.service"), timer_row("backup.timer")];
    assert_eq!(
        row_named(&rows, &RowTarget::Unit("backup.timer".to_string())),
        Some(1)
    );
}

/// A filesystem, by mount point.
#[test]
fn a_filesystem_row_is_found_by_its_mount_point() {
    let rows = vec![filesystem_row("/"), filesystem_row("/boot")];
    assert_eq!(
        row_named(&rows, &RowTarget::Filesystem("/boot".to_string())),
        Some(1)
    );
}

/// A process, by pid rather than by name.
///
/// Two processes can share a `comm` - every worker of a pooled service
/// does - so a name would find whichever sorted first, which is not the
/// row anybody meant.
#[test]
fn a_process_row_is_found_by_its_pid() {
    let rows = vec![proc_row(101, "postgres"), proc_row(102, "postgres")];
    assert_eq!(row_named(&rows, &RowTarget::Process(102)), Some(1));
}

/// **Absent is a real answer.**
///
/// The rows are rebuilt between the jump being remembered and the jump
/// being taken, so the thing named may have gone: a unit stopped, a
/// filesystem unmounted, a process exited. Returning a default index
/// would land the cursor on an unrelated row and call it the one that was
/// asked for.
#[test]
fn a_name_no_row_carries_is_not_found() {
    let rows = vec![unit_row("sshd.service"), filesystem_row("/")];
    assert_eq!(
        row_named(&rows, &RowTarget::Unit("nginx.service".to_string())),
        None
    );
    assert_eq!(
        row_named(&rows, &RowTarget::Filesystem("/boot".to_string())),
        None
    );
    assert_eq!(row_named(&rows, &RowTarget::Process(999)), None);
    assert_eq!(
        row_named(&[], &RowTarget::Unit("sshd.service".to_string())),
        None,
        "and an empty buffer finds nothing rather than panicking"
    );
}

/// A target only matches its own kind of row.
///
/// A mount point and a unit name cannot collide today, but the lookup
/// answering "is there any row whose text is this" rather than "is there
/// a filesystem row for this mount" is how a jump ends up somewhere
/// plausible and wrong.
#[test]
fn a_target_does_not_match_a_row_of_another_kind() {
    let rows = vec![unit_row("/boot"), filesystem_row("/boot")];
    assert_eq!(
        row_named(&rows, &RowTarget::Filesystem("/boot".to_string())),
        Some(1),
        "the filesystem row, not the unit that happens to share its text"
    );
    assert_eq!(
        row_named(&rows, &RowTarget::Unit("/boot".to_string())),
        Some(0)
    );
}

// ---------------------------------------------------------------------
// Where a finding jumps.
// ---------------------------------------------------------------------

use masys_app::buffer::Buffer;
use masys_app::keymap::Sort;
use masys_domain::finding::{FindingKind, PressureResource};

/// **The two targets that answer "no row", and what they answer it
/// against.**
///
/// Neither is a defect. `TopOfProcs` is an ordering and a position, which
/// only the session can resolve; a `UnitLog` row does not exist until the
/// Log buffer has been opened on that unit, and by then the destination
/// is the buffer rather than a row in it.
///
/// This pins that each answers `None` even against a buffer holding a row
/// of every kind, including one whose text matches. It does not pin the
/// *size* of that set: a third variant deliberately routed to `false`
/// would pass this untouched. The compiler covers the case of one being
/// forgotten - the match on `RowTarget` carries no wildcard - and nothing
/// covers the case of one being added on purpose, which is a decision and
/// is meant to be made by hand.
#[test]
fn the_targets_that_name_no_row_find_nothing_in_any_buffer() {
    let rows = vec![
        unit_row("sshd.service"),
        timer_row("backup.timer"),
        filesystem_row("/"),
        proc_row(101, "postgres"),
    ];
    for target in [
        RowTarget::TopOfProcs(Sort::Cpu),
        // Named after a unit that *is* in the rows, so a lookup matching
        // on text rather than on kind would find it.
        RowTarget::UnitLog("sshd.service".to_string()),
    ] {
        assert_eq!(
            row_named(&rows, &target),
            None,
            "{target:?} names no row, in a buffer holding one of every kind"
        );
    }
}

// The per-finding jump destinations were asserted here, one test per
// kind, until `Presentation` made them one answer among six. They are
// now `a_finding_describes_itself_completely` and
// `the_findings_that_name_no_row_do_not_jump` in masys-view, which check
// the same values against the same expectations and additionally fail
// when a *new* kind is added without one. What stays below is what only
// a session can answer: that pressing `.` on a finding actually moves
// the cursor there, and that `esc` comes back.

mod fake;

use fake::{FakePlatformService, FakeSystemService, NoScanner, bare_snapshot};
use masys_app::key::KeyCode;
use masys_app::{App, Key};
use masys_domain::sample::SystemState;

/// `FailedUnit` finding and the systemd buffer carries its row.
fn host_with_a_failed_unit() -> App {
    let mut snapshot = bare_snapshot();
    snapshot.system_state = SystemState::Degraded;
    let mut system = FakeSystemService::returning(vec![snapshot]);
    system.units = vec![
        Unit {
            active_state: ActiveState::Failed,
            sub_state: "failed".to_string(),
            exit_code: Some(1),
            ..plain_unit("nginx.service")
        },
        plain_unit("sshd.service"),
    ];
    let mut app = App::new(
        Box::new(system),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "devbox".to_string(),
    );
    app.tick(10_800_000, "t".to_string());
    app
}

/// A host whose sshd really flaps, and whose units are otherwise fine.
///
/// Three restarts inside the flapping window - the tick below is at
/// 10_800_000, the default window is an hour, so anything at or after
/// 7_200_000 counts. `plain_unit` leaves `restart_timestamps_ms` empty,
/// which raises no flapping finding at all; a test built on it can only
/// pass by landing on some other kind of unit finding, so this host has
/// no failed unit for it to fall through to.
fn host_with_a_flapping_unit() -> App {
    let mut system = FakeSystemService::returning(vec![bare_snapshot()]);
    system.units = vec![
        Unit {
            restart_timestamps_ms: vec![8_000_000, 9_000_000, 10_000_000],
            ..plain_unit("sshd.service")
        },
        plain_unit("cron.service"),
    ];
    let mut app = App::new(
        Box::new(system),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "devbox".to_string(),
    );
    app.tick(10_800_000, "t".to_string());
    app
}

fn plain_unit(name: &str) -> Unit {
    Unit {
        name: name.to_string(),
        kind: UnitKind::Service,
        active_state: ActiveState::Active,
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

/// The name of the row the cursor is on: a group by its name, a process
/// by its comm.
///
/// What "landed on the right row" has to mean in Procs, which is an
/// outline rather than a flat table - processes fold under their cgroup,
/// so the row a sort puts first is the *group* holding the heaviest one.
fn landing_name(app: &App) -> String {
    let view = app.view();
    match view.rows.get(view.selected.expect("a cursor")) {
        Some(Node::ProcGroup { name, .. }) => name.clone(),
        Some(Node::Proc { proc, .. }) => proc.comm.clone(),
        other => panic!("expected a process or its group, got {other:#?}"),
    }
}

/// Puts the cursor on the first finding of the given kind, and returns
/// whether it found one.
fn cursor_on_finding(
    app: &mut App,
    matches: impl Fn(&masys_domain::finding::Finding) -> bool,
) -> bool {
    let target = app
        .view()
        .rows
        .iter()
        .position(|row| matches!(row, Node::Finding { finding: f, .. } if matches(f)));
    let Some(target) = target else { return false };
    while app.view().selected != Some(target) {
        let before = app.view().selected;
        app.handle_key(Key::new(KeyCode::Down));
        if app.view().selected == before {
            return false;
        }
    }
    true
}

/// **`.` on a failed-unit finding lands on that unit, and `esc` comes
/// back.**
///
/// The whole point of the key: the Status buffer names what is wrong, and
/// this is what turns that name into somewhere to be.
#[test]
fn the_jump_lands_on_the_unit_the_finding_names_and_esc_returns() {
    let mut app = host_with_a_failed_unit();
    assert!(
        cursor_on_finding(&mut app, |f| matches!(
            &f.kind,
            masys_domain::finding::FindingKind::FailedUnit { unit, .. } if unit == "nginx.service"
        )),
        "the host has a failed unit, so Status has a finding for it"
    );

    app.handle_key(Key::char('.'));
    assert_eq!(app.buffer(), Buffer::Systemd, "the jump opens systemd");
    let view = app.view();
    assert!(
        matches!(
            view.rows.get(view.selected.expect("a cursor")),
            Some(Node::Unit { unit, .. }) if unit.name == "nginx.service"
        ),
        "and the cursor is on the unit the finding named: {:#?}",
        view.rows.get(view.selected.unwrap())
    );

    app.handle_key(Key::new(KeyCode::Esc));
    assert_eq!(app.buffer(), Buffer::Status, "esc comes back to Status");
    assert!(
        matches!(
            app.view().rows.get(app.view().selected.expect("a cursor")),
            Some(Node::Finding { .. })
        ),
        "cursor still on the finding it left from"
    );
}

/// A finding that names no row does nothing, and says so in the footer.
///
/// `SystemDegraded` is the case that is always present next to a failed
/// unit, which makes it the honest one to assert: two findings, adjacent,
/// one live and one marked.
#[test]
fn a_finding_that_names_no_row_does_not_move_and_is_dimmed() {
    let mut app = host_with_a_failed_unit();
    assert!(
        cursor_on_finding(&mut app, |f| matches!(
            &f.kind,
            masys_domain::finding::FindingKind::SystemDegraded { .. }
        )),
        "a degraded host has the rollup finding"
    );

    let before = app.view().selected;
    app.handle_key(Key::char('.'));
    assert_eq!(app.buffer(), Buffer::Status, "no jump happened");
    assert_eq!(app.view().selected, before, "and the cursor did not move");

    let jump = app
        .view()
        .actions
        .iter()
        .find(|binding| binding.chord == ".")
        .expect("the footer offers the jump in Status")
        .clone();
    assert!(jump.dimmed, "marked rather than silently doing nothing");
}

/// And on a finding that does name a row, the same footer entry is live.
#[test]
fn the_footer_marks_the_jump_live_on_a_finding_that_has_somewhere_to_go() {
    let mut app = host_with_a_failed_unit();
    assert!(cursor_on_finding(&mut app, |f| matches!(
        &f.kind,
        masys_domain::finding::FindingKind::FailedUnit { .. }
    )));

    let jump = app
        .view()
        .actions
        .iter()
        .find(|binding| binding.chord == ".")
        .expect("the footer offers the jump in Status")
        .clone();
    assert!(!jump.dimmed, "this one has somewhere to go");
}

/// A session whose `/boot` is nearly full, so Status carries a
/// `DiskCapacity` finding and the IO buffer carries that mount's row.
fn host_with_a_full_filesystem() -> App {
    let mut snapshot = bare_snapshot();
    snapshot.filesystems = vec![
        Filesystem {
            mount_point: "/".to_string(),
            used_percent: 40.0,
            free_bytes: 200_000_000_000,
            inode_used_percent: 3.0,
            read_only: false,
        },
        Filesystem {
            mount_point: "/boot".to_string(),
            used_percent: 95.0,
            free_bytes: 40_000_000,
            inode_used_percent: 4.0,
            read_only: false,
        },
    ];
    let mut app = App::new(
        Box::new(FakeSystemService::returning(vec![snapshot])),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "devbox".to_string(),
    );
    app.tick(10_800_000, "t".to_string());
    app
}

/// **`.` on a full-filesystem finding lands on that mount in the IO
/// buffer**, and lands on the right one where there are several.
#[test]
fn the_jump_lands_on_the_filesystem_the_finding_names() {
    let mut app = host_with_a_full_filesystem();
    assert!(
        cursor_on_finding(&mut app, |f| matches!(
            &f.kind,
            FindingKind::DiskCapacity { mount_point, .. } if mount_point == "/boot"
        )),
        "a 95%-full /boot is a finding"
    );

    app.handle_key(Key::char('.'));
    assert_eq!(app.buffer(), Buffer::Io, "the jump opens the IO buffer");
    let view = app.view();
    assert!(
        matches!(
            view.rows.get(view.selected.expect("a cursor")),
            Some(Node::Filesystem { filesystem, .. }) if filesystem.mount_point == "/boot"
        ),
        "on /boot, not on the / row above it: {:#?}",
        view.rows.get(view.selected.unwrap())
    );

    app.handle_key(Key::new(KeyCode::Esc));
    assert_eq!(app.buffer(), Buffer::Status, "esc comes back");
}

/// A mount that is no longer in the sample moves no cursor.
///
/// The finding is built from the tick that saw the filesystem; the rows
/// are rebuilt from the tick that runs when the key is pressed. A mount
/// unmounted between the two has no row, and landing the cursor on
/// whatever is at that index instead would present an unrelated
/// filesystem as the one that was full.
#[test]
fn a_mount_that_has_gone_moves_no_cursor() {
    let rows = vec![filesystem_row("/"), filesystem_row("/home")];
    assert_eq!(
        row_named(&rows, &RowTarget::Filesystem("/boot".to_string())),
        None,
        "not index 0, and not the last row either"
    );
}

/// A host under memory pressure, with the heaviest process deliberately
/// not the one that would be top under any other order.
fn host_under_memory_pressure() -> App {
    let mut snapshot = bare_snapshot();
    snapshot.pressure = Some(masys_domain::sample::Pressure {
        memory_some: masys_domain::sample::PsiLine {
            avg60: 40.0,
            ..Default::default()
        },
        ..Default::default()
    });
    // A cgroup each, or both collapse into the single folded `kernel`
    // group and no `Node::Proc` row exists to land on - which is how the
    // first version of this test passed while proving nothing.
    snapshot.procs = vec![
        // Top by cpu, and first by name.
        masys_domain::sample::Proc {
            cpu_ticks: 9_000,
            rss_bytes: 10_000_000,
            cgroup: Some("/system.slice/busy.service".to_string()),
            ..plain_proc(101, "aaa-busy")
        },
        // Top by memory, and top by nothing else.
        masys_domain::sample::Proc {
            cpu_ticks: 10,
            rss_bytes: 8_000_000_000,
            cgroup: Some("/system.slice/hungry.service".to_string()),
            ..plain_proc(202, "zzz-hungry")
        },
    ];
    let mut app = App::new(
        Box::new(FakeSystemService::returning(vec![snapshot])),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "devbox".to_string(),
    );
    app.tick(10_800_000, "t".to_string());
    app
}

fn plain_proc(pid: u32, comm: &str) -> masys_domain::sample::Proc {
    masys_domain::sample::Proc {
        pid,
        comm: comm.to_string(),
        cgroup: None,
        cpu_ticks: 0,
        rss_bytes: 0,
        io_read_bytes: 0,
        io_write_bytes: 0,
        state: ProcState::Running,
        nice: 0,
        oom_score: 0,
        threads: 1,
        started_at_ms: 0,
    }
}

/// **A memory pressure jump lands on the hungriest process, not the
/// busiest one.**
///
/// The assertion that proves the sort actually took effect: the fixture's
/// top process by CPU is deliberately different from its top by memory,
/// so a jump that opened Procs in its default CPU order would land on the
/// wrong row and pass a weaker test.
#[test]
fn a_memory_pressure_jump_lands_on_the_hungriest_process() {
    let mut app = host_under_memory_pressure();
    assert!(
        cursor_on_finding(&mut app, |f| matches!(
            &f.kind,
            FindingKind::Pressure {
                resource: PressureResource::Memory,
                ..
            }
        )),
        "the host is under memory pressure"
    );

    app.handle_key(Key::char('.'));
    assert_eq!(app.buffer(), Buffer::Procs, "the jump opens Procs");
    // Which group is the whole assertion - the first version of this test
    // accepted any `ProcGroup` at all, and so passed with the sort mapped
    // to `Name`.
    let landed = landing_name(&app);
    assert!(
        landed.contains("hungry"),
        "on the memory-heaviest process or its group, not the busiest: {landed}"
    );

    app.handle_key(Key::new(KeyCode::Esc));
    assert_eq!(app.buffer(), Buffer::Status, "esc comes back");
}

/// A host under IO pressure, whose noisiest process is deliberately
/// neither the busiest by CPU nor the hungriest by memory.
///
/// Four samples, because a rate is a difference: `rate_of` answers `None`
/// until a pid has been sampled twice, and a fixture whose counters stand
/// still derives a rate of zero for every row - which would leave the CPU
/// order decided by the name tie-break rather than by CPU, and a test
/// that lands correctly for a reason it does not state. The third sample
/// is empty and the fourth fills again, for the cursor round-trip below.
fn host_under_io_pressure() -> App {
    let procs = |ticks: u64, written: u64| {
        vec![
            // Top by CPU: the only one whose ticks move between samples.
            masys_domain::sample::Proc {
                cpu_ticks: ticks,
                cgroup: Some("/system.slice/busy.service".to_string()),
                ..plain_proc(101, "aaa-busy")
            },
            // Top by memory, and by nothing else.
            masys_domain::sample::Proc {
                rss_bytes: 8_000_000_000,
                cgroup: Some("/system.slice/hungry.service".to_string()),
                ..plain_proc(202, "zzz-hungry")
            },
            // Top by IO, and last of the three by *group* name - the
            // Procs buffer folds by cgroup, so `busy` < `hungry` <
            // `noisy` is the order a name sort or a tie-break produces,
            // and landing here means the IO sort really took effect.
            masys_domain::sample::Proc {
                io_write_bytes: written,
                cgroup: Some("/system.slice/noisy.service".to_string()),
                ..plain_proc(303, "mmm-noisy")
            },
        ]
    };
    let mut first = bare_snapshot();
    first.pressure = Some(masys_domain::sample::Pressure {
        io_some: masys_domain::sample::PsiLine {
            avg60: 40.0,
            ..Default::default()
        },
        ..Default::default()
    });
    first.procs = procs(0, 0);
    let mut second = first.clone();
    second.taken_at_ms = 1_000;
    second.procs = procs(9_000, 50_000_000);
    let mut empty = first.clone();
    empty.taken_at_ms = 2_000;
    empty.procs = Vec::new();
    let mut refilled = first.clone();
    refilled.taken_at_ms = 3_000;
    refilled.procs = procs(18_000, 100_000_000);

    let mut app = App::new(
        Box::new(FakeSystemService::returning(vec![
            first, second, empty, refilled,
        ])),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "devbox".to_string(),
    );
    app.tick(10_800_000, "t".to_string());
    app.tick(10_801_000, "t".to_string());
    app
}

/// **An IO pressure jump lands on the process hitting the disk.**
///
/// #23's break-verification, which asked to "map IO pressure to another
/// column and confirm an end-to-end test fails" - there was no end-to-end
/// test to fail. CPU and memory each had one; IO had only the pure
/// mapping, which cannot see a key binding, a sort or a cursor.
#[test]
fn an_io_pressure_jump_lands_on_the_noisiest_process() {
    let mut app = host_under_io_pressure();
    assert!(
        cursor_on_finding(&mut app, |f| matches!(
            &f.kind,
            FindingKind::Pressure {
                resource: PressureResource::Io,
                ..
            }
        )),
        "the host is under io pressure"
    );

    app.handle_key(Key::char('.'));
    assert_eq!(app.buffer(), Buffer::Procs, "the jump opens Procs");
    let landed = landing_name(&app);
    assert!(
        landed.contains("noisy"),
        "on the process writing to disk, not the busiest or the hungriest: {landed}"
    );

    app.handle_key(Key::new(KeyCode::Esc));
    assert_eq!(app.buffer(), Buffer::Status, "esc comes back");
}

/// A flapping unit jumps end to end, not only in the mapping.
///
/// #20 asks for `FailedUnit` *and* `FlappingUnit`; only the first had a
/// path through `App`, and a mapping test cannot see a key binding, a
/// buffer switch or a cursor.
#[test]
fn the_jump_lands_on_a_flapping_unit_too() {
    let mut app = host_with_a_flapping_unit();
    assert!(
        cursor_on_finding(&mut app, |f| matches!(
            &f.kind,
            FindingKind::FlappingUnit { unit, .. } if unit == "sshd.service"
        )),
        "the fixture's sshd restarts three times inside the window, so \
         Status has a flapping finding naming it"
    );

    app.handle_key(Key::char('.'));
    assert_eq!(app.buffer(), Buffer::Systemd, "the jump opens systemd");
    let view = app.view();
    assert!(
        matches!(
            view.rows.get(view.selected.expect("a cursor")),
            Some(Node::Unit { unit, .. }) if unit.name == "sshd.service"
        ),
        "and the cursor is on the unit that flapped, not merely in the \
         buffer: {:#?}",
        view.rows.get(view.selected.unwrap())
    );
}

/// The other two filesystem findings reach the IO buffer through `App`.
///
/// #21 asks for all three; only `DiskCapacity` had an end-to-end path,
/// and the other two differ from it in which threshold produced them,
/// not in where they go - but that is an argument, not a test.
#[test]
fn a_read_only_filesystem_finding_reaches_the_io_buffer() {
    let mut snapshot = bare_snapshot();
    snapshot.filesystems = vec![
        Filesystem {
            mount_point: "/".to_string(),
            used_percent: 10.0,
            free_bytes: 500_000_000_000,
            inode_used_percent: 1.0,
            read_only: false,
        },
        Filesystem {
            mount_point: "/srv".to_string(),
            used_percent: 10.0,
            free_bytes: 500_000_000_000,
            inode_used_percent: 1.0,
            read_only: true,
        },
    ];
    let mut app = App::new(
        Box::new(FakeSystemService::returning(vec![snapshot])),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "devbox".to_string(),
    );
    app.tick(10_800_000, "t".to_string());

    assert!(
        cursor_on_finding(&mut app, |f| matches!(
            &f.kind,
            FindingKind::ReadOnlyFilesystem { mount_point } if mount_point == "/srv"
        )),
        "a read-only mount is a finding"
    );
    app.handle_key(Key::char('.'));
    assert_eq!(app.buffer(), Buffer::Io);
    let view = app.view();
    assert!(
        matches!(
            view.rows.get(view.selected.expect("a cursor")),
            Some(Node::Filesystem { filesystem, .. }) if filesystem.mount_point == "/srv"
        ),
        "on /srv, not on the / row: {:#?}",
        view.rows.get(view.selected.unwrap())
    );
}

/// **A filter on the destination is cleared, so the jump lands on
/// something.**
///
/// `go_to_unit` states the rule this follows: "a jump that lands on
/// nothing because the target was folded away, or filtered out, is worse
/// than no jump". Without it, `.` on a failed-unit finding with an
/// unrelated filter left in the systemd buffer switched buffer and
/// parked the cursor on a section header, with the unit invisible.
#[test]
fn a_filter_left_on_the_destination_does_not_hide_the_landing() {
    let mut app = host_with_a_failed_unit();
    // Leave a filter in the systemd buffer that excludes the unit the
    // finding names.
    app.handle_key(Key::char('3'));
    app.handle_key(Key::char('/'));
    for c in "sshd".chars() {
        app.handle_key(Key::char(c));
    }
    app.handle_key(Key::new(KeyCode::Enter));
    app.handle_key(Key::char('1'));

    assert!(cursor_on_finding(&mut app, |f| matches!(
        &f.kind,
        FindingKind::FailedUnit { unit, .. } if unit == "nginx.service"
    )));
    app.handle_key(Key::char('.'));

    let view = app.view();
    assert!(
        matches!(
            view.rows.get(view.selected.expect("a cursor")),
            Some(Node::Unit { unit, .. }) if unit.name == "nginx.service"
        ),
        "the filter is cleared so the target is on screen: {:#?}",
        view.rows.get(view.selected.unwrap())
    );
}

/// A host with no processes claims no top consumer.
///
/// #22's criterion. The jump still happens - the operator asked for it -
/// but nothing is presented as the cause.
#[test]
fn an_empty_process_sample_names_no_top_consumer() {
    let mut snapshot = bare_snapshot();
    snapshot.pressure = Some(masys_domain::sample::Pressure {
        memory_some: masys_domain::sample::PsiLine {
            avg60: 40.0,
            ..Default::default()
        },
        ..Default::default()
    });
    snapshot.procs = Vec::new();
    let mut app = App::new(
        Box::new(FakeSystemService::returning(vec![snapshot])),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "devbox".to_string(),
    );
    app.tick(10_800_000, "t".to_string());

    assert!(cursor_on_finding(&mut app, |f| matches!(
        &f.kind,
        FindingKind::Pressure {
            resource: PressureResource::Memory,
            ..
        }
    )));
    app.handle_key(Key::char('.'));
    assert_eq!(app.buffer(), Buffer::Procs);
    assert!(
        !app.view()
            .rows
            .iter()
            .any(|row| matches!(row, Node::Proc { .. })),
        "there is no process to be the cause, and none is invented"
    );
    // Nothing is pointed at, which is the "claims no top consumer" half.
    // Weak on its own - `selected` is derived as `(!rows.is_empty())
    // .then(..)`, so an empty buffer answers `None` whatever the cursor
    // holds - which is why the criterion's other half is its own test
    // below, on a buffer that empties and fills again.
    assert_eq!(
        app.view().selected,
        None,
        "there is no top consumer, so nothing is pointed at"
    );
}

/// **A buffer empty for one tick remembers where the cursor was.**
///
/// #22's other half: "an empty or unavailable process sample moves no
/// cursor", and the sample going empty is exactly when a cursor is most
/// expensive to lose - a host under enough pressure to fail a read is one
/// an operator is watching a particular row of.
///
/// The trap is that nothing on screen shows the loss while the buffer is
/// empty: `selected` is `None` because there are no rows, not because the
/// position was kept. It shows up one tick later, when the processes come
/// back and the cursor is at the top instead of where it was left.
#[test]
fn a_sample_that_empties_and_fills_again_leaves_the_cursor_where_it_was() {
    let mut app = host_under_io_pressure();
    app.handle_key(Key::char('2'));
    app.handle_key(Key::new(KeyCode::Down));
    app.handle_key(Key::new(KeyCode::Down));
    let parked = app.view().selected.expect("a cursor on a populated buffer");
    let populated_rows = app.view().rows.len();
    assert!(parked > 0, "parked below the top, or this proves nothing");

    app.tick(10_802_000, "t".to_string());
    assert_eq!(
        app.view().selected,
        None,
        "the sample came back empty, so there is nothing to point at"
    );

    app.tick(10_803_000, "t".to_string());
    assert_eq!(
        app.view().selected,
        Some(parked),
        "and the processes are back, on the row the operator left"
    );
    assert_eq!(
        app.view().rows.len(),
        populated_rows,
        "the processes are back, so there is a row at {parked} to be on"
    );
}

// The journal findings' destinations moved to masys-view with the rest:
// a recent error from a unit opens that unit's log, and one from no unit
// - like every kernel line - names nothing. Asserted there, against the
// same expectations, in a table a new kind cannot slip past.

/// And the whole way through: `.` on that row leaves the operator
/// reading the log, on the unit the line came from.
#[test]
fn pressing_dot_on_a_recent_error_opens_that_units_log() {
    let now = 10_800_000;
    let mut system = FakeSystemService::returning(vec![bare_snapshot()]);
    system.journal = vec![masys_domain::journal::Entry {
        timestamp_ms: now - 1_000,
        unit: Some("sshd.service".to_string()),
        priority: masys_domain::journal::Priority::Error,
        message: "too many authentication failures".to_string(),
        origin: masys_domain::journal::Origin::Userspace,
    }];
    let mut app = App::new(
        Box::new(system),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "devbox".to_string(),
    );
    app.tick(now, "t".to_string());

    assert!(cursor_on_finding(&mut app, |f| matches!(
        &f.kind,
        FindingKind::RecentError { .. }
    )));
    app.handle_key(Key::char('.'));
    assert_eq!(app.buffer(), Buffer::Log);
    assert!(
        matches!(app.view().header, masys_view::Header::Log { unit, .. } if unit == "sshd.service"),
        "the log is open on the unit that logged the line"
    );
}

/// A kernel row has nowhere to go, and the key says so by doing nothing
/// rather than by moving somewhere plausible.
#[test]
fn pressing_dot_on_a_kernel_error_stays_put() {
    let now = 10_800_000;
    let mut system = FakeSystemService::returning(vec![bare_snapshot()]);
    system.journal = vec![masys_domain::journal::Entry {
        timestamp_ms: now - 1_000,
        unit: None,
        priority: masys_domain::journal::Priority::Error,
        message: "EXT4-fs error (device sda1)".to_string(),
        origin: masys_domain::journal::Origin::Kernel,
    }];
    let mut app = App::new(
        Box::new(system),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "devbox".to_string(),
    );
    app.tick(now, "t".to_string());

    assert!(cursor_on_finding(&mut app, |f| matches!(
        &f.kind,
        FindingKind::KernelError { .. }
    )));
    app.handle_key(Key::char('.'));
    assert_eq!(app.buffer(), Buffer::Status, "there is nowhere to go");
}

// `Unreadable` names nothing to jump to either, asserted in masys-view
// alongside the other four permanent `None`s.
