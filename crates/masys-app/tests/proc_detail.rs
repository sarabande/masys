//! Opening a process row, and jumping from one to the unit that owns it.

mod fake;

use fake::{FakePlatformService, FakeSystemService, proc};
use masys_app::key::{Key, KeyCode};
use masys_app::{App, Buffer};
use masys_domain::proc_detail::{Fd, FdTarget, ProcDetail};
use masys_domain::sample::{Pressure, Proc, Snapshot, SystemState};
use masys_domain::unit::{ActiveState, Unit, UnitKind};
use masys_view::Node;
use std::collections::HashMap;

fn in_cgroup(mut p: Proc, cgroup: &str) -> Proc {
    p.cgroup = Some(cgroup.to_string());
    p
}

fn unit(name: &str, kind: UnitKind) -> Unit {
    Unit {
        name: name.to_string(),
        kind,
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

fn a_detail() -> ProcDetail {
    ProcDetail {
        cmdline: Some("/nix/store/abc-postgresql/bin/postgres -D /var/lib/postgresql/16".into()),
        exe: Some("/nix/store/abc-postgresql/bin/postgres".into()),
        cwd: Some("/var/lib/postgresql/16".into()),
        ppid: Some(1),
        virt_bytes: Some(4_300_000_000),
        swap_bytes: Some(0),
        env: vec![("PGDATA".into(), "/var/lib/postgresql/16".into())],
        fds: vec![
            Fd { number: 0, target: FdTarget::Path("/dev/null".into()) },
            Fd { number: 7, target: FdTarget::Tcp { local: "0.0.0.0:5432".into(), peer: None, state: "LISTEN".into() } },
        ],
    }
}

fn app_with(procs: Vec<Proc>, units: Vec<Unit>, details: HashMap<u32, ProcDetail>) -> App {
    let system = FakeSystemService {
        snapshot: Snapshot {
            taken_at_ms: 0,
            procs,
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
        },
        units,
        calls: Default::default(),
        fails_with: None,
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: details,
        detail_queries: Default::default(),
    };
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(1_000, "t".to_string()).expect("tick");
    app
}

fn app() -> App {
    app_with(
        vec![in_cgroup(proc(9932, "postgres", 0), "/system.slice/postgresql.service")],
        vec![unit("postgresql.service", UnitKind::Service)],
        HashMap::from([(9932, a_detail())]),
    )
}

fn press(app: &mut App, keys: &str) {
    for c in keys.chars() {
        app.handle_key(Key::char(c));
    }
}

/// Walks to the first process row, whichever row the buffer opens on.
fn on_a_process(app: &mut App) {
    press(app, "2");
    for _ in 0..10 {
        if matches!(app.view().rows.get(app.view().selected.unwrap_or(0)), Some(Node::Proc { .. })) {
            return;
        }
        app.handle_key(Key::new(KeyCode::Down));
    }
    panic!("no process row reached");
}

fn detail_row(app: &App) -> Option<&Node> {
    app.view().rows.iter().find(|row| matches!(row, Node::ProcDetail { .. }))
}

/// `tab` falls through to the row's own detail on any row with no
/// subtree, which is what a process row is - so the process detail rides
/// the key that already means "show me what is inside this".
#[test]
fn tab_opens_a_process_and_closes_it_again() {
    let mut app = app();
    on_a_process(&mut app);
    assert!(detail_row(&app).is_none(), "a row starts shut");

    app.handle_key(Key::new(KeyCode::Tab));
    assert!(detail_row(&app).is_some(), "{:#?}", app.view().rows);

    app.handle_key(Key::new(KeyCode::Tab));
    assert!(detail_row(&app).is_none(), "the same key shuts it");
}

/// The read is what the keypress pays for. A row that opened on every
/// tick would put a per-descriptor read behind a timer, which is exactly
/// what splitting `proc_detail` off the sweep exists to prevent.
#[test]
fn nothing_is_read_until_the_row_is_opened() {
    let system = FakeSystemService {
        snapshot: Snapshot {
            taken_at_ms: 0,
            procs: vec![in_cgroup(proc(9932, "postgres", 0), "/system.slice/postgresql.service")],
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
        },
        units: Vec::new(),
        calls: Default::default(),
        fails_with: None,
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: HashMap::from([(9932, a_detail())]),
        detail_queries: Default::default(),
    };
    let queries = system.detail_queries.clone();
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(1_000, "t".to_string()).expect("tick");

    on_a_process(&mut app);
    assert!(queries.borrow().is_empty(), "walking to a row reads nothing: {:?}", queries.borrow());

    app.handle_key(Key::new(KeyCode::Tab));
    assert_eq!(*queries.borrow(), vec![9932]);
}

/// The block sits directly beneath the row it describes. Anywhere else
/// and it would be describing whichever row it happened to land under.
#[test]
fn the_detail_sits_directly_under_its_own_row() {
    let mut app = app();
    on_a_process(&mut app);
    app.handle_key(Key::new(KeyCode::Tab));

    let rows = app.view().rows;
    let at = rows.iter().position(|row| matches!(row, Node::ProcDetail { .. })).expect("a detail row");
    let Some(Node::Proc { proc, .. }) = rows.get(at - 1) else { panic!("{:#?}", rows) };
    let Some(Node::ProcDetail { proc: described, detail }) = rows.get(at) else { unreachable!() };
    assert_eq!(proc.pid, described.pid);
    assert_eq!(detail.as_deref().and_then(|d| d.cmdline.clone()).as_deref(), Some(a_detail().cmdline.unwrap().as_str()));
}

/// Most of what the detail reads is gated on same-uid or CAP_SYS_PTRACE,
/// so a failed read is the ordinary state of another user's process. The
/// row still opens, on what `Proc` alone carries - refusing to open it
/// would make an unprivileged masys look broken rather than unprivileged.
#[test]
fn a_process_masys_cannot_read_still_opens() {
    let mut app = app_with(
        vec![in_cgroup(proc(9932, "postgres", 0), "/system.slice/postgresql.service")],
        Vec::new(),
        // No entry, so the fake answers with the empty detail an
        // unprivileged read produces.
        HashMap::new(),
    );
    on_a_process(&mut app);
    app.handle_key(Key::new(KeyCode::Tab));
    assert!(detail_row(&app).is_some(), "the row opens on what Proc carries: {:#?}", app.view().rows);
}

/// The association was always in the data - the Procs buffer groups by
/// cgroup, and on a systemd host a cgroup is a unit - it was simply not
/// reachable without reading the path off the screen.
#[test]
fn u_jumps_from_a_process_to_the_unit_that_owns_it() {
    let mut app = app();
    on_a_process(&mut app);
    press(&mut app, "u");

    assert_eq!(app.buffer(), Buffer::Systemd);
    let view = app.view();
    let Some(Node::Unit { unit, .. }) = view.rows.get(view.selected.expect("a cursor")) else {
        panic!("{:#?}", view.rows);
    };
    assert_eq!(unit.name, "postgresql.service");
}

/// A group row *is* a cgroup, so it needs no process to ask - and jumping
/// from the group is the more natural half of the pair.
#[test]
fn u_jumps_from_a_group_row_too() {
    let mut app = app();
    press(&mut app, "2");
    assert!(matches!(app.view().rows.first(), Some(Node::ProcGroup { .. })), "{:#?}", app.view().rows);
    press(&mut app, "u");

    assert_eq!(app.buffer(), Buffer::Systemd);
    let view = app.view();
    assert!(matches!(view.rows.get(view.selected.unwrap()), Some(Node::Unit { unit, .. }) if unit.name == "postgresql.service"));
}

/// A delegated sub-cgroup puts processes several levels below the unit
/// that owns them. The leaf of `/system.slice/foo.service/bar` is `bar`,
/// which names nothing - so the walk goes upward until it finds a unit
/// the poll actually saw.
#[test]
fn a_delegated_subcgroup_still_finds_its_unit() {
    let mut app = app_with(
        vec![in_cgroup(proc(9932, "worker", 0), "/system.slice/containerd.service/kubepods/pod-abc")],
        vec![unit("containerd.service", UnitKind::Service)],
        HashMap::new(),
    );
    on_a_process(&mut app);
    press(&mut app, "u");

    let view = app.view();
    assert!(matches!(view.rows.get(view.selected.unwrap()), Some(Node::Unit { unit, .. }) if unit.name == "containerd.service"));
}

/// A jump that lands on nothing because the target was filtered out is
/// worse than no jump: it moves you somewhere and shows you nothing, and
/// gives no hint which of the two hid it.
#[test]
fn the_jump_reveals_a_unit_the_systemd_filter_was_hiding() {
    let mut app = app_with(
        vec![in_cgroup(proc(9932, "postgres", 0), "/system.slice/postgresql.service")],
        vec![unit("postgresql.service", UnitKind::Service), unit("sshd.service", UnitKind::Service)],
        HashMap::new(),
    );
    // Narrow the systemd view to something else entirely, then leave it.
    press(&mut app, "3/sshd");
    app.handle_key(Key::new(KeyCode::Enter));
    assert!(!app.view().rows.iter().any(|r| matches!(r, Node::Unit { unit, .. } if unit.name == "postgresql.service")));

    on_a_process(&mut app);
    press(&mut app, "u");
    let view = app.view();
    assert!(matches!(view.rows.get(view.selected.unwrap()), Some(Node::Unit { unit, .. }) if unit.name == "postgresql.service"));
}

/// The kernel bucket has no unit and never will. Silent rather than an
/// error line, because `u` is bound on the view where the kernel bucket
/// lives and a message on every stray press would be noise.
#[test]
fn u_does_nothing_on_a_row_with_no_unit() {
    let mut app = app_with(vec![in_cgroup(proc(2, "kthreadd", 0), "/")], Vec::new(), HashMap::new());
    press(&mut app, "2");
    let before = app.buffer();
    press(&mut app, "u");
    assert_eq!(app.buffer(), before, "nowhere to go, so nothing moves");
}

/// `esc` comes back, the same way it does after `l` - which is what makes
/// this a drill-down rather than a one-way jump.
#[test]
fn esc_returns_from_the_jump_to_the_row_it_left() {
    let mut app = app();
    on_a_process(&mut app);
    let before = app.view().selected;
    press(&mut app, "u");
    assert_eq!(app.buffer(), Buffer::Systemd);

    app.handle_key(Key::new(KeyCode::Esc));
    assert_eq!(app.buffer(), Buffer::Procs);
    assert_eq!(app.view().selected, before, "back to the row, not to the top");
}

/// The Procs footer has to say the key exists, or nobody presses it.
#[test]
fn the_procs_footer_offers_the_jump() {
    let mut app = app();
    press(&mut app, "2");
    assert!(
        app.view().actions.iter().any(|b| b.chord == "u"),
        "{:?}",
        app.view().actions.iter().map(|b| b.chord.clone()).collect::<Vec<_>>()
    );
}

/// The count is of *results*, not of rows. A group row kept because one
/// of its processes matched is structure, and counting it would report a
/// search for one process as two hits.
#[test]
fn the_filter_counts_matches_rather_than_rows() {
    // The group is named `db.service` deliberately: a cgroup path that
    // *contained* the needle would match on its own name and be a result
    // in its own right, which is a different case - the one below.
    let mut app = app_with(
        vec![
            in_cgroup(proc(9932, "postgres", 0), "/system.slice/db.service"),
            in_cgroup(proc(9933, "postgres", 0), "/system.slice/db.service"),
            in_cgroup(proc(3000, "chrome", 0), "/user.slice"),
        ],
        Vec::new(),
        HashMap::new(),
    );
    press(&mut app, "2/postgres");

    let view = app.view();
    // Three rows survive - the group and its two processes - but only the
    // two processes are results.
    assert_eq!(view.rows.len(), 3, "{:#?}", view.rows);
    assert_eq!(view.filter_matches, Some(2));
}

/// A group whose own name matched *is* a result: it is the row the query
/// found, and the processes under it come along as its contents.
#[test]
fn a_group_matched_by_name_counts_once() {
    let mut app = app_with(vec![in_cgroup(proc(9932, "postgres", 0), "/system.slice/postgresql.service")], Vec::new(), HashMap::new());
    press(&mut app, "2/postgresql.service");
    assert_eq!(app.view().filter_matches, Some(1), "{:#?}", app.view().rows);
}

/// The number has to move with every keystroke, or it is describing a
/// query that is no longer on screen.
#[test]
fn the_count_follows_the_query_as_it_narrows() {
    let mut app = app_with(
        vec![in_cgroup(proc(9932, "postgres", 0), "/system.slice/db.service"), in_cgroup(proc(3000, "chrome", 0), "/user.slice")],
        Vec::new(),
        HashMap::new(),
    );
    press(&mut app, "2/");
    // An empty query is not a query, so there is nothing to count yet.
    assert_eq!(app.view().filter_matches, None);

    press(&mut app, "p");
    assert_eq!(app.view().filter_matches, Some(1), "postgres alone");

    press(&mut app, "zzz");
    assert_eq!(app.view().filter_matches, Some(0), "and now nothing");
}

/// The pause *is* the open row - there is no separate state to get out of
/// step with it, which is what makes folding enough to resume.
#[test]
fn an_open_process_pauses_the_tick_and_folding_resumes_it() {
    let mut app = app();
    assert!(!app.auto_refresh_paused(), "nothing is open yet");

    on_a_process(&mut app);
    app.handle_key(Key::new(KeyCode::Tab));
    assert!(app.auto_refresh_paused(), "an open row freezes the timer");

    app.handle_key(Key::new(KeyCode::Tab));
    assert!(!app.auto_refresh_paused(), "folding it resumes");
}

/// The pause follows the open row, not the buffer you are looking at: a
/// row left open while you glance at the systemd view is still a row
/// being read, and still costs a per-descriptor re-read every tick.
#[test]
fn the_pause_survives_a_look_at_another_buffer() {
    let mut app = app();
    on_a_process(&mut app);
    app.handle_key(Key::new(KeyCode::Tab));
    press(&mut app, "3");
    assert!(app.auto_refresh_paused(), "{:?}", app.buffer());
}

/// `g` is the escape hatch, and it works because it reaches `tick`
/// through the caller rather than through the timer this pauses.
#[test]
fn a_manual_refresh_still_works_while_paused() {
    let mut app = app();
    on_a_process(&mut app);
    app.handle_key(Key::new(KeyCode::Tab));
    assert!(app.wants_refresh(Key::char('g')), "g still asks the caller to tick");
    app.tick(2_000, "t2".to_string()).expect("a manual tick");
    assert!(app.auto_refresh_paused(), "and the row is still open afterwards");
}
