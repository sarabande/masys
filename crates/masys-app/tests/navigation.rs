mod fake;

use fake::{FakePlatformService, FakeSystemService, proc};
use masys_app::app::Flow;
use masys_app::key::{Key, KeyCode};
use masys_app::{App, Buffer};
use masys_domain::sample::{Pressure, Snapshot, SystemState};
use masys_view::{Header, Node};

fn snapshot() -> Snapshot {
    Snapshot {
        taken_at_ms: 0,
        procs: vec![
            with_cgroup(proc(1, "chrome", 0), Some("/user.slice/app.slice")),
            with_cgroup(proc(2, "rust-analyzer", 0), Some("/user.slice/app.slice")),
            // A kernel thread, which reports the root cgroup rather than none.
            with_cgroup(proc(3, "kworker/0:0", 0), Some("/")),
        ],
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

fn with_cgroup(mut p: masys_domain::sample::Proc, cgroup: Option<&str>) -> masys_domain::sample::Proc {
    p.cgroup = cgroup.map(str::to_string);
    p
}

fn app() -> App {
    let system = FakeSystemService {
        snapshot: snapshot(),
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
    app.tick(1_000, "t".to_string()).expect("tick");
    app
}

fn press(app: &mut App, keys: &str) -> Flow {
    let mut flow = Flow::Continue;
    for c in keys.chars() {
        flow = app.handle_key(Key::char(c));
    }
    flow
}

/// Movement is on arrows now, not letters.
fn press_key(app: &mut App, code: KeyCode, times: usize) {
    for _ in 0..times {
        app.handle_key(Key::new(code));
    }
}

#[test]
fn a_session_opens_on_the_status_buffer() {
    let app = app();
    assert_eq!(app.buffer(), Buffer::Status);
    assert!(matches!(app.view().header, Header::Status { .. }));
}

/// A digit jumps straight to its view - the only way to switch. Letters
/// are the view's own; none of them navigates.
#[test]
fn a_digit_switches_views() {
    let mut app = app();
    press(&mut app, "2");
    assert_eq!(app.buffer(), Buffer::Procs);
    assert!(matches!(app.view().header, Header::Procs { .. }));

    press(&mut app, "1");
    assert_eq!(app.buffer(), Buffer::Status);

    // The letters these jumps used to occupy no longer move anything.
    for stale in ["s", "t", "u", "j", "d"] {
        press(&mut app, stale);
        assert_eq!(app.buffer(), Buffer::Status, "`{stale}` still switches views");
    }
}

/// `b` was the prefix the design first called for. Still unbound, it must
/// be inert rather than swallowing the key after it - a stale prefix that
/// eats one press is worse than one that does nothing.
#[test]
fn b_no_longer_swallows_the_next_key() {
    let mut app = app();
    press(&mut app, "b");
    assert_eq!(app.buffer(), Buffer::Status);
    press(&mut app, "2");
    assert_eq!(app.buffer(), Buffer::Procs, "the digit after b is read normally");
}

/// `q` quits from every view. It used to bury to Status first, which is
/// two meanings on one key - and with a digit reaching any view directly
/// there is nothing left for the burying half to do.
#[test]
fn q_quits_from_every_view() {
    for (digit, buffer) in [("1", Buffer::Status), ("2", Buffer::Procs), ("3", Buffer::Systemd), ("4", Buffer::Io)] {
        let mut app = app();
        press(&mut app, digit);
        assert_eq!(app.buffer(), buffer);
        assert_eq!(press(&mut app, "q"), Flow::Quit, "q must quit from {buffer:?}");
    }
}

#[test]
fn the_cursor_moves_and_stops_at_both_ends() {
    let mut app = app();
    let first = app.view().selected.expect("a cursor on a non-empty buffer");
    press_key(&mut app, KeyCode::Up, 1);
    assert_eq!(app.view().selected, Some(first), "the cursor does not run off the top");

    press_key(&mut app, KeyCode::Down, 24);
    let last = app.view().selected.expect("a cursor");
    press_key(&mut app, KeyCode::Down, 1);
    assert_eq!(app.view().selected, Some(last), "the cursor does not run off the bottom");
}

/// A blank line is spacing, not a row: landing the cursor on one would
/// make movement feel like it stuttered.
#[test]
fn the_cursor_never_lands_on_a_spacer() {
    let mut app = app();
    for _ in 0..30 {
        let selected = app.view().selected.expect("a cursor");
        assert!(!matches!(app.view().rows[selected], Node::Spacer), "row {selected} is a spacer");
        app.handle_key(Key::new(KeyCode::Down));
    }
}

/// Each buffer keeps its own cursor: switching away and back should not
/// silently move where you were.
#[test]
fn each_buffer_remembers_its_own_cursor() {
    let mut app = app();
    press_key(&mut app, KeyCode::Down, 2);
    let status_cursor = app.view().selected.expect("a cursor");

    press(&mut app, "2");
    press_key(&mut app, KeyCode::Down, 1);
    let procs_cursor = app.view().selected.expect("a cursor");

    press(&mut app, "1");
    assert_eq!(app.view().selected, Some(status_cursor), "Status kept its place");
    press(&mut app, "2");
    assert_eq!(app.view().selected, Some(procs_cursor), "Procs kept its place");
}

/// Rows are rebuilt on every tick, and a buffer can shrink between them.
/// A cursor left past the end would index out of bounds in the renderer.
#[test]
fn the_cursor_is_clamped_when_the_buffer_shrinks() {
    let mut app = app();
    press(&mut app, "2");
    press_key(&mut app, KeyCode::Down, 8);

    // A tick whose sample has one process instead of three.
    let mut small = snapshot();
    small.procs = vec![proc(1, "chrome", 0)];
    app.replace_system(Box::new(FakeSystemService {
        snapshot: small,
        units: Vec::new(),
        calls: Default::default(),
        fails_with: None,
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    }));
    app.tick(2_000, "t".to_string()).expect("tick");

    let selected = app.view().selected.expect("a cursor");
    assert!(selected < app.view().rows.len(), "cursor {selected} is past {} rows", app.view().rows.len());
}

#[test]
fn g_refreshes_without_changing_buffer_or_cursor() {
    let mut app = app();
    press(&mut app, "2");
    press_key(&mut app, KeyCode::Down, 1);
    let before = app.view().selected;
    press(&mut app, "g");
    assert_eq!(app.buffer(), Buffer::Procs);
    assert_eq!(app.view().selected, before);
}

/// The design says kernel threads have "no cgroup". On a live host that
/// is not true: they report the *root* cgroup, `/`. Bucketing on an empty
/// value therefore never fires, and every kernel thread lands in a group
/// named `/` that sorts first and buries the real services.
#[test]
fn kernel_threads_bucket_separately_from_real_cgroups() {
    use masys_app::procs::{KERNEL_GROUP, build_proc_rows};
    use std::collections::HashSet;

    let mut kthread = proc(2, "kthreadd", 0);
    kthread.cgroup = Some("/".to_string());
    let mut service = proc(3, "sshd", 0);
    service.cgroup = Some("/system.slice/sshd.service".to_string());
    let mut orphan = proc(4, "weird", 0);
    orphan.cgroup = None;

    let rows = build_proc_rows(
        &[kthread, service, orphan],
        &[],
        &HashSet::new(),
        masys_app::keymap::Sort::Name,
        false,
        100,
        0,
        &Default::default(),
    );
    let groups: Vec<String> = rows
        .iter()
        .filter_map(|r| match r {
            Node::ProcGroup { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(groups, vec!["/system.slice/sshd.service".to_string(), KERNEL_GROUP.to_string()]);
}

/// 145 `kworker/*` rows are noise, which is why the design's own mockup
/// draws the kernel bucket collapsed.
#[test]
fn the_kernel_bucket_starts_collapsed() {
    let mut app = app();
    press(&mut app, "2");
    let rows = app.view().rows;
    let kernel = rows
        .iter()
        .find_map(|r| match r {
            Node::ProcGroup { name, expanded, .. } if name == masys_app::procs::KERNEL_GROUP => Some(*expanded),
            _ => None,
        })
        .expect("a kernel group");
    assert!(!kernel, "the kernel bucket is folded away until asked for");
}
