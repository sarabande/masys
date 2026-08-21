//! Opening a filesystem to see what is using it.
//!
//! Against a scripted `DirScanner`: the walker itself is `masys-scan`'s
//! to test against real trees, and what belongs here is only what the app
//! does with what it is handed - when a scan starts, when it does not
//! restart, what the rows say while it is still counting, and what
//! cancels it.

mod fake;

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use fake::{FakePlatformService, FakeSystemService};
use masys_app::key::{Key, KeyCode};
use masys_app::{App, Buffer};
use masys_domain::sample::{Filesystem, Pressure, Snapshot, SystemState};
use masys_domain::scan::{DirScanner, DirSize, ScanProgress};
use masys_view::Node;

/// A scanner that answers with whatever the test wrote into it, and
/// records what it was asked to do.
#[derive(Default)]
struct ScriptedScanner {
    calls: RefCell<Vec<String>>,
    progress: RefCell<ScanProgress>,
}

impl DirScanner for ScriptedScanner {
    fn start(&self, root: &Path) {
        self.calls.borrow_mut().push(format!("start {}", root.display()));
    }
    fn poll(&self) -> ScanProgress {
        self.progress.borrow().clone()
    }
    fn cancel(&self) {
        self.calls.borrow_mut().push("cancel".to_string());
    }
}

type Shared = std::rc::Rc<ScriptedScanner>;

/// `Rc` on both sides so the test can read the calls and write the
/// progress while the app holds the same scanner.
struct Handle(Shared);

impl DirScanner for Handle {
    fn start(&self, root: &Path) {
        self.0.start(root)
    }
    fn poll(&self) -> ScanProgress {
        self.0.poll()
    }
    fn cancel(&self) {
        self.0.cancel()
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        taken_at_ms: 0,
        procs: Vec::new(),
        pressure: Some(Pressure::default()),
        filesystems: vec![Filesystem {
            mount_point: "/".to_string(),
            used_percent: 81.0,
            free_bytes: 173 << 30,
            inode_used_percent: 6.0,
            read_only: false,
        }],
        disks: Vec::new(),
        interfaces: Vec::new(),
        clock_synced: true,
        utc_offset_secs: -21_600,
        oom_kills: Vec::new(),
        system_state: SystemState::Running,
        clock_ticks_per_sec: 100,
        machine: None,
        load: None,
        uptime_secs: None,
        memory: None,
    }
}

fn io_app() -> (App, Shared) {
    let scanner: Shared = Default::default();
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
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(Handle(std::rc::Rc::clone(&scanner))), "devbox".to_string());
    app.tick(1_000, "t".to_string()).expect("tick");
    // Into the IO view, onto the filesystem row past its section header.
    app.handle_key(Key::char('4'));
    app.handle_key(Key::new(KeyCode::Down));
    (app, scanner)
}

fn dir(path: &str, bytes: u64) -> DirSize {
    DirSize { path: PathBuf::from(path), bytes }
}

fn dir_rows(app: &App) -> Vec<(String, u64)> {
    app.view()
        .rows
        .iter()
        .filter_map(|r| match r {
            Node::DirEntry { path, bytes, .. } => Some((path.display().to_string(), *bytes)),
            _ => None,
        })
        .collect()
}

/// The IO view says how full a filesystem is and cannot say why. Opening
/// the row is the question "why", and it is the only thing that starts a
/// scan - masys never walks a filesystem nobody asked about.
#[test]
fn opening_a_filesystem_starts_a_scan_of_its_mount_point() {
    let (mut app, scanner) = io_app();
    assert!(scanner.calls.borrow().is_empty(), "nothing scans until asked");

    app.handle_key(Key::new(KeyCode::Enter));
    assert_eq!(*scanner.calls.borrow(), vec!["start /".to_string()]);
}

/// One scan builds the whole retained tree, so drilling in costs nothing
/// further. Restarting on every keypress would throw away seconds of work
/// and never converge.
#[test]
fn drilling_into_a_directory_does_not_restart_the_scan() {
    let (mut app, scanner) = io_app();
    app.handle_key(Key::new(KeyCode::Enter));
    *scanner.progress.borrow_mut() = ScanProgress {
        root: Some(PathBuf::from("/")),
        dirs: vec![dir("/nix", 40 << 30), dir("/home", 20 << 30)],
        done: true,
        ..Default::default()
    };
    app.tick(3_000, "t".to_string()).expect("tick");

    // Onto the first directory row, and open it.
    while !matches!(app.view().rows.get(app.view().selected.unwrap_or(0)), Some(Node::DirEntry { .. })) {
        app.handle_key(Key::new(KeyCode::Down));
    }
    app.handle_key(Key::new(KeyCode::Enter));

    assert_eq!(*scanner.calls.borrow(), vec!["start /".to_string()], "still just the one scan");
}

/// Largest first: the question is what is using the disk, and the answer
/// is at the top or it is not an answer.
#[test]
fn directories_are_listed_largest_first() {
    let (mut app, scanner) = io_app();
    app.handle_key(Key::new(KeyCode::Enter));
    *scanner.progress.borrow_mut() = ScanProgress {
        root: Some(PathBuf::from("/")),
        dirs: vec![dir("/var", 4 << 30), dir("/nix", 40 << 30), dir("/home", 20 << 30)],
        done: true,
        ..Default::default()
    };
    app.tick(3_000, "t".to_string()).expect("tick");

    let rows = dir_rows(&app);
    assert_eq!(rows.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), vec!["/nix", "/home", "/var"]);
}

/// Only the children of what is open. A flat dump of every retained
/// directory would be thousands of rows with no structure at all.
#[test]
fn only_the_children_of_an_open_directory_are_shown() {
    let (mut app, scanner) = io_app();
    app.handle_key(Key::new(KeyCode::Enter));
    *scanner.progress.borrow_mut() = ScanProgress {
        root: Some(PathBuf::from("/")),
        dirs: vec![dir("/nix", 40 << 30), dir("/nix/store", 39 << 30), dir("/nix/var", 1 << 30)],
        done: true,
        ..Default::default()
    };
    app.tick(3_000, "t".to_string()).expect("tick");

    assert_eq!(dir_rows(&app).iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), vec!["/nix"], "the grandchildren stay shut");
}

/// Sizes climb while the walk proceeds, which is the whole point of a
/// progressive scan: a row that is still growing is more useful than a
/// blank screen for ten seconds.
#[test]
fn sizes_climb_as_the_scan_proceeds() {
    let (mut app, scanner) = io_app();
    app.handle_key(Key::new(KeyCode::Enter));

    *scanner.progress.borrow_mut() =
        ScanProgress { root: Some(PathBuf::from("/")), dirs: vec![dir("/nix", 5 << 30)], counted_bytes: 5 << 30, ..Default::default() };
    app.tick(3_000, "t".to_string()).expect("tick");
    assert_eq!(dir_rows(&app), vec![("/nix".to_string(), 5 << 30)]);

    *scanner.progress.borrow_mut() = ScanProgress {
        root: Some(PathBuf::from("/")),
        dirs: vec![dir("/nix", 40 << 30)],
        counted_bytes: 40 << 30,
        done: true,
        ..Default::default()
    };
    app.tick(5_000, "t".to_string()).expect("tick");
    assert_eq!(dir_rows(&app), vec![("/nix".to_string(), 40 << 30)]);
}

/// A scan nobody is looking at is CPU nobody asked to spend.
#[test]
fn closing_the_filesystem_cancels_the_scan() {
    let (mut app, scanner) = io_app();
    app.handle_key(Key::new(KeyCode::Enter));
    *scanner.progress.borrow_mut() =
        ScanProgress { root: Some(PathBuf::from("/")), dirs: vec![dir("/nix", 40 << 30)], done: true, ..Default::default() };
    app.tick(3_000, "t".to_string()).expect("tick");
    assert!(!dir_rows(&app).is_empty());

    app.handle_key(Key::new(KeyCode::Enter));
    assert!(scanner.calls.borrow().contains(&"cancel".to_string()), "{:?}", scanner.calls.borrow());
    assert!(dir_rows(&app).is_empty(), "and the rows go with it");
}

/// The view stays in the IO buffer while all this happens - opening a row
/// is not a drill-down to somewhere else.
#[test]
fn scanning_never_leaves_the_io_view() {
    let (mut app, _) = io_app();
    app.handle_key(Key::new(KeyCode::Enter));
    assert_eq!(app.buffer(), Buffer::Io);
}
