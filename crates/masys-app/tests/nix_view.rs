//! The session's half of the Nix view: holding the port, sampling it, and
//! turning what it said into the buffer's rows.
//!
//! `nix_buffer.rs`'s own tests cover the layout. What is under test here
//! is the wiring - that a reading reaches a row at all, that a *failed*
//! reading does not erase the last good one, and that a store nothing
//! could be measured about reports nothing rather than zero.

mod fake;

use fake::{FakeDeclarative, FakePlatformService, FakeSystemService, NoScanner, bare_snapshot};
use masys_app::buffer::Registry;
use masys_app::keymap::Keymap;
use masys_app::{App, Buffer, Key, KeyCode};
use masys_domain::declarative::{Generation, Input, InputSource, Inputs, Profile, ProfileKind};
use masys_domain::sample::{Filesystem, Snapshot};
use masys_domain::unit::{ActiveState, Timer, Unit, UnitKind};
use masys_view::{Node, SectionKind};

/// A session on a host that has a declarative service, with the keymap
/// its registry demands. The pairing is the composition root's job and
/// `App::with_declarative` asserts it, so a test that got it wrong would
/// panic here rather than fail somewhere downstream.
fn nixos(snapshot: Snapshot, units: Vec<Unit>, declarative: FakeDeclarative) -> App {
    let mut system = FakeSystemService::returning(vec![snapshot]);
    system.units = units;
    App::with_declarative(
        Box::new(system),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "nixbox".to_string(),
        Keymap::for_registry(Registry::new(true)),
        Some(Box::new(declarative)),
    )
}

fn press_five(app: &mut App) {
    app.handle_key(Key { code: KeyCode::Char('5'), ctrl: false, alt: false });
}

fn generation(id: u64, store_path: &str) -> Generation {
    Generation {
        id,
        created_ms: Some(1_000),
        store_path: Some(store_path.to_string()),
        label: Some("26.11".to_string()),
        kernel: None,
        current: false,
        booted: false,
    }
}

fn system_profile(ids: &[u64]) -> Profile {
    Profile {
        kind: ProfileKind::System,
        path: "/nix/var/nix/profiles/system".to_string(),
        writable: Some(true),
        generations: ids.iter().map(|id| generation(*id, &format!("/nix/store/{id}-nixos-system"))).collect(),
    }
}

fn timer(name: &str, next_ms: Option<u64>, last_ms: Option<u64>) -> Unit {
    Unit {
        name: name.to_string(),
        kind: UnitKind::Timer,
        active_state: ActiveState::Active,
        sub_state: "waiting".to_string(),
        exit_code: None,
        enabled: true,
        restart_timestamps_ms: Vec::new(),
        since_ms: 0,
        cgroup: None,
        slice: None,
        triggers: Vec::new(),
        timer: Some(Timer { next_ms, last_ms, activates: name.replace(".timer", ".service") }),
    }
}

fn store_row(app: &App) -> (Option<f32>, Option<u64>, Option<u32>) {
    app.view()
        .rows
        .iter()
        .find_map(|row| match row {
            Node::NixStore { used_percent, free_bytes, gc_roots, .. } => Some((*used_percent, *free_bytes, *gc_roots)),
            _ => None,
        })
        .expect("the Store section always renders")
}

fn generation_ids(app: &App) -> Vec<u64> {
    app.view()
        .rows
        .iter()
        .filter_map(|row| match row {
            Node::Generation { generation, .. } => Some(generation.id),
            _ => None,
        })
        .collect()
}

/// The registry's whole purpose, asserted through the session rather than
/// through the registry: pressing `5` on a Debian host must not switch
/// views, because there is no view there to switch to.
#[test]
fn digit_five_does_nothing_without_a_declarative_service() {
    let mut app = App::with_declarative(
        Box::new(FakeSystemService::returning(vec![bare_snapshot()])),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "debian-box".to_string(),
        Keymap::default(),
        None,
    );
    app.tick(1_000, "t".to_string()).expect("tick");

    press_five(&mut app);

    assert_eq!(app.buffer(), Buffer::Status);
}

#[test]
fn digit_five_opens_the_nix_view_when_the_service_is_present() {
    let mut app = nixos(bare_snapshot(), Vec::new(), FakeDeclarative::default());
    app.tick(1_000, "t".to_string()).expect("tick");

    press_five(&mut app);

    assert_eq!(app.buffer(), Buffer::Nix);
}

/// The wiring itself: a reading the port gave `tick` has to survive into
/// the rows `rebuild` produces, without a second sample.
#[test]
fn what_the_port_reported_reaches_the_rows() {
    let declarative = FakeDeclarative {
        profiles: vec![system_profile(&[41, 42])],
        gc_roots: 7,
        gc_retention: Some("14d".to_string()),
        ..Default::default()
    };
    let mut app = nixos(bare_snapshot(), Vec::new(), declarative);
    app.tick(1_000, "t".to_string()).expect("tick");

    press_five(&mut app);

    // Newest first, which is `build_nix_rows`' ordering - asserted here
    // only to show the generations arrived rather than being invented.
    assert_eq!(generation_ids(&app), vec![42, 41]);
    let (_, _, gc_roots) = store_row(&app);
    assert_eq!(gc_roots, Some(7));
}

/// A failed read is not an absence. `profiles()` returning `Err` must
/// leave the last good list standing: rendering "this host has no
/// generations" because a directory could not be read is a claim, where
/// keeping the previous answer is a shrug.
#[test]
fn a_failed_read_keeps_the_last_good_reading() {
    let failing = std::rc::Rc::new(std::cell::Cell::new(false));
    let declarative = FakeDeclarative {
        profiles: vec![system_profile(&[41, 42])],
        gc_roots: 7,
        gc_retention: Some("14d".to_string()),
        failing: failing.clone(),
        ..Default::default()
    };
    let mut app = nixos(bare_snapshot(), Vec::new(), declarative);
    app.tick(1_000, "t".to_string()).expect("tick");
    press_five(&mut app);
    assert_eq!(generation_ids(&app), vec![42, 41], "the first tick read them");

    failing.set(true);
    app.tick(3_000, "t".to_string()).expect("a declarative failure does not fail the tick");

    assert_eq!(generation_ids(&app), vec![42, 41], "a read that failed must not empty the list");
    let (_, _, gc_roots) = store_row(&app);
    assert_eq!(gc_roots, Some(7), "nor report nothing pinning the store");
}

/// `Option` through to the row. A store whose filesystem is not in the
/// sample has no reading, and "0% used, 0 bytes free" is a different and
/// much more alarming statement than "not measured".
#[test]
fn an_unmeasured_store_reports_no_reading_rather_than_zero() {
    let mut app = nixos(bare_snapshot(), Vec::new(), FakeDeclarative::default());
    app.tick(1_000, "t".to_string()).expect("tick");

    press_five(&mut app);

    // The count beside them is `Some(0)`, and that is the distinction:
    // this port answered, and its answer was zero. A store with nothing
    // pinning it is a real state and prints as a number.
    assert_eq!(store_row(&app), (None, None, Some(0)));
}

/// The other half of the same rule, and the one the Store row was getting
/// wrong: a count that was never read must not print as a count.
///
/// `0 gc roots` and `0 system generations` are not neutral defaults, they
/// are findings - nothing is pinning the store, there is nothing to roll
/// back to. `DeclarativeService::gc_roots` answers `u32` and returns
/// `Err` rather than `0` for exactly this reason; the session used to
/// store that `Err` as the `0` the port had refused to say.
#[test]
fn a_store_whose_reads_have_never_succeeded_counts_nothing() {
    let declarative = FakeDeclarative {
        profiles: vec![system_profile(&[41, 42])],
        gc_roots: 7,
        failing: std::rc::Rc::new(std::cell::Cell::new(true)),
        ..Default::default()
    };
    let mut app = nixos(bare_snapshot(), Vec::new(), declarative);
    app.tick(1_000, "t".to_string()).expect("a declarative failure does not fail the tick");

    press_five(&mut app);

    let (_, _, gc_roots) = store_row(&app);
    assert_eq!(gc_roots, None, "a refused read must not report an unpinned store");
    let counts = app.view().rows.iter().find_map(|row| match row {
        Node::NixStore { system_generations, home_generations, .. } => Some((*system_generations, *home_generations)),
        _ => None,
    });
    assert_eq!(counts, Some((None, None)), "nor a host with no generations to roll back to");
}

/// `/nix` where the store has a filesystem of its own, `/` only as the
/// fallback - a host with both must not report the root filesystem's
/// usage as the store's.
#[test]
fn the_store_reading_prefers_the_nix_mount_to_the_root_one() {
    let mut snapshot = bare_snapshot();
    snapshot.filesystems = vec![
        Filesystem { mount_point: "/".to_string(), used_percent: 12.0, free_bytes: 1_000, inode_used_percent: 1.0, read_only: false },
        Filesystem { mount_point: "/nix".to_string(), used_percent: 87.0, free_bytes: 2_000, inode_used_percent: 2.0, read_only: false },
    ];
    let mut app = nixos(snapshot, Vec::new(), FakeDeclarative::default());
    app.tick(1_000, "t".to_string()).expect("tick");

    press_five(&mut app);

    assert_eq!(store_row(&app), (Some(87.0), Some(2_000), Some(0)));
}

#[test]
fn the_root_filesystem_answers_where_the_store_has_no_mount_of_its_own() {
    let mut snapshot = bare_snapshot();
    snapshot.filesystems =
        vec![Filesystem { mount_point: "/".to_string(), used_percent: 12.0, free_bytes: 1_000, inode_used_percent: 1.0, read_only: false }];
    let mut app = nixos(snapshot, Vec::new(), FakeDeclarative::default());
    app.tick(1_000, "t".to_string()).expect("tick");

    press_five(&mut app);

    assert_eq!(store_row(&app), (Some(12.0), Some(1_000), Some(0)));
}

/// The policy row is a join: the retention comes from the declarative
/// port, the schedule from the timer units `SystemService` already polls.
/// Only the gc job has a retention - nothing is retained by optimising.
#[test]
fn policy_rows_join_the_ports_retention_to_the_timer_units() {
    let units = vec![timer("nix-gc.timer", Some(61_000), Some(1_000)), timer("nix-optimise.timer", None, None)];
    let declarative = FakeDeclarative { gc_retention: Some("14d".to_string()), ..Default::default() };
    let mut app = nixos(bare_snapshot(), units, declarative);
    app.tick(10_000, "t".to_string()).expect("tick");

    press_five(&mut app);

    let policies: Vec<_> = app
        .view()
        .rows
        .iter()
        .filter_map(|row| match row {
            Node::NixPolicy { job, retention, next_in_ms, last_ago_ms, .. } => {
                Some((job.clone(), retention.clone(), *next_in_ms, *last_ago_ms))
            }
            _ => None,
        })
        .collect();

    // `Some(Some(..))` for the declared policy and `Some(None)` for
    // optimise, which retains nothing by its nature rather than by a
    // reading masys failed to take.
    assert_eq!(
        policies,
        vec![
            ("gc".to_string(), Some(Some("14d".to_string())), Some(51_000), Some(9_000)),
            ("optimise".to_string(), Some(None), None, None),
        ]
    );
}

/// And the third state, which is the one the row used to lose: a gc
/// retention that could not be read is not a gc job that keeps nothing.
///
/// The port's own doc says its `Err` "must not collapse into the same
/// `None` a genuine unbounded policy would produce". The session collapsed
/// it one layer up instead, and the row then reported the finding - a gc
/// that will collect every generation but the current one - on a host
/// where masys had simply not managed to look.
#[test]
fn a_retention_that_could_not_be_read_is_not_a_job_that_keeps_nothing() {
    let units = vec![timer("nix-gc.timer", Some(61_000), Some(1_000))];
    let declarative = FakeDeclarative {
        gc_retention: Some("14d".to_string()),
        failing: std::rc::Rc::new(std::cell::Cell::new(true)),
        ..Default::default()
    };
    let mut app = nixos(bare_snapshot(), units, declarative);
    app.tick(10_000, "t".to_string()).expect("a declarative failure does not fail the tick");

    press_five(&mut app);

    let retention = app.view().rows.iter().find_map(|row| match row {
        Node::NixPolicy { job, retention, .. } if job == "gc" => Some(retention.clone()),
        _ => None,
    });
    let retention = retention.expect("the gc row is there - its timer unit is in the sample");
    assert_eq!(retention, None, "a read that was refused must not read as a job that keeps nothing");
}

/// Absent is not empty, one level down: a job whose timer unit this host
/// does not have contributes no row at all. A row saying "optimise: never"
/// for a host that has not scheduled it sends the operator looking for a
/// timer that was never there.
#[test]
fn a_job_with_no_timer_unit_contributes_no_row() {
    let units = vec![timer("nix-gc.timer", Some(61_000), None)];
    let mut app = nixos(bare_snapshot(), units, FakeDeclarative::default());
    app.tick(10_000, "t".to_string()).expect("tick");

    press_five(&mut app);

    let jobs: Vec<_> = app
        .view()
        .rows
        .iter()
        .filter_map(|row| match row {
            Node::NixPolicy { job, .. } => Some(job.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(jobs, vec!["gc".to_string()]);
}

fn flake_inputs(names: &[&str]) -> Inputs {
    Inputs {
        source: InputSource::Flake { lock_path: "/etc/nixos/flake.lock".to_string() },
        inputs: names
            .iter()
            .map(|name| Input { name: name.to_string(), origin: None, rev: None, last_modified_secs: None, direct: true })
            .collect(),
    }
}

fn service(name: &str, active_state: ActiveState) -> Unit {
    Unit {
        name: name.to_string(),
        kind: UnitKind::Service,
        active_state,
        sub_state: "running".to_string(),
        exit_code: None,
        enabled: true,
        restart_timestamps_ms: Vec::new(),
        since_ms: 500,
        cgroup: None,
        slice: None,
        triggers: Vec::new(),
        timer: None,
    }
}

/// The section costs one predicate over the units masys already polls, so
/// what this asserts is the join: units that arrived through
/// `SystemService` for every other view show up under a Nix heading with
/// no second read.
///
/// Worst first, like every other list of units in masys - a failed
/// `nix-gc.service` is the first row under the heading, not the fourth.
#[test]
fn nix_units_come_from_the_poll_masys_already_runs() {
    let units = vec![
        service("sshd.service", ActiveState::Active),
        service("nix-daemon.service", ActiveState::Active),
        service("home-manager-operator.service", ActiveState::Active),
        service("nix-gc.service", ActiveState::Failed),
        timer("nix-gc.timer", Some(61_000), Some(1_000)),
    ];
    let mut app = nixos(bare_snapshot(), units, FakeDeclarative::default());
    app.tick(1_000, "t".to_string()).expect("tick");

    press_five(&mut app);

    let named: Vec<String> = app
        .view()
        .rows
        .iter()
        .filter_map(|row| match row {
            Node::Unit { unit, .. } => Some(unit.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(named, vec!["nix-gc.service", "home-manager-operator.service", "nix-daemon.service"], "worst first, then by name");
    assert!(!named.iter().any(|name| name == "nix-gc.timer"), "the Store section already reports that timer as policy");
    assert!(!named.iter().any(|name| name == "sshd.service"), "and the systemd view owns the rest of the host");
}

/// Puts the cursor on the first row of the given kind, so a fold test does
/// not have to know how many rows precede it.
fn step_onto_section(app: &mut App, kind: SectionKind) {
    for _ in 0..40 {
        if matches!(app.view().rows.get(app.view().selected.unwrap_or(usize::MAX)), Some(Node::SectionHeader { kind: k, .. }) if *k == kind)
        {
            return;
        }
        app.handle_key(Key::new(KeyCode::Down));
    }
    panic!("never landed on a {kind:?} header: {:#?}", app.view().rows);
}

/// magit's `TAB`, on this view: the Nix view added `Generations`,
/// `Inputs`, `NixStore` and `NixUnits` as section kinds, and
/// `cycle_section` only knew `Units` and `Journal` - so `tab` on a
/// "System generations" header fell through to `toggle_detail`, which
/// has nothing for a section header either, and did nothing at all. No
/// compiler error: both are `_ =>` arms.
#[test]
fn tab_folds_the_generations_section_in_the_nix_view() {
    let declarative = FakeDeclarative { profiles: vec![system_profile(&[1, 2, 3])], ..Default::default() };
    let mut app = nixos(bare_snapshot(), Vec::new(), declarative);
    app.tick(1_000, "t".to_string()).expect("tick");
    press_five(&mut app);
    assert_eq!(generation_ids(&app).len(), 3, "rows start visible");

    step_onto_section(&mut app, SectionKind::Generations(ProfileKind::System));
    app.handle_key(Key::new(KeyCode::Tab));
    assert_eq!(generation_ids(&app), Vec::<u64>::new(), "tab folds the section's rows away");

    app.handle_key(Key::new(KeyCode::Tab));
    assert_eq!(generation_ids(&app).len(), 3, "and tab brings them back");
}

/// And `TAB` reaches the new section, which is the half that was missing
/// the last time a section was added here: `SectionKind::NixUnits` was
/// already in `cycle_section`'s match, waiting for a producer.
#[test]
fn tab_folds_the_nix_units_section() {
    let units = vec![service("nix-daemon.service", ActiveState::Active), service("home-manager-operator.service", ActiveState::Active)];
    let mut app = nixos(bare_snapshot(), units, FakeDeclarative::default());
    app.tick(1_000, "t".to_string()).expect("tick");
    press_five(&mut app);
    let shown = |app: &App| app.view().rows.iter().filter(|row| matches!(row, Node::Unit { .. })).count();
    assert_eq!(shown(&app), 2, "rows start visible");

    step_onto_section(&mut app, SectionKind::NixUnits);
    app.handle_key(Key::new(KeyCode::Tab));
    assert_eq!(shown(&app), 0, "tab folds the section's rows away");

    app.handle_key(Key::new(KeyCode::Tab));
    assert_eq!(shown(&app), 2, "and tab brings them back");
}

/// magit's `S-TAB` on this view. `Generations` and `Inputs` fold like any
/// other section; `NixStore` does not, because it is this view's
/// equivalent of the Status buffer's `System` section - always rendered,
/// never hidden, the "checks actually ran" indicator - and `System` is
/// excluded from the Status buffer's own collapse-all for that reason.
#[test]
fn shift_tab_folds_every_foldable_section_but_leaves_the_store_alone() {
    let declarative =
        FakeDeclarative { profiles: vec![system_profile(&[1, 2])], inputs: Some(flake_inputs(&["nixpkgs"])), ..Default::default() };
    let mut app = nixos(bare_snapshot(), Vec::new(), declarative);
    app.tick(1_000, "t".to_string()).expect("tick");
    press_five(&mut app);
    assert!(!generation_ids(&app).is_empty(), "generations start visible");
    assert!(app.view().rows.iter().any(|row| matches!(row, Node::Input { .. })), "inputs start visible");

    app.handle_key(Key::new(KeyCode::BackTab));
    assert_eq!(generation_ids(&app), Vec::<u64>::new(), "every generation folded");
    assert!(!app.view().rows.iter().any(|row| matches!(row, Node::Input { .. })), "every input folded");
    let _ = store_row(&app); // still renders - `.expect` inside panics otherwise.

    app.handle_key(Key::new(KeyCode::BackTab));
    assert!(!generation_ids(&app).is_empty(), "and back to rows");
    assert!(app.view().rows.iter().any(|row| matches!(row, Node::Input { .. })));
}

/// The pairing check in `with_declarative`, in both directions. It is a
/// `debug_assert`, so the guard below is not decoration: a release-mode
/// test run would find nothing to panic about, and the tests must say so
/// rather than fail.
#[cfg(debug_assertions)]
mod pairing {
    use super::*;

    /// A service the keymap cannot reach: `5` is bound to nothing, and the
    /// view exists with no way in.
    #[test]
    #[should_panic(expected = "disagree about whether this host has a Nix view")]
    fn a_service_with_a_default_keymap_is_caught() {
        let _ = App::with_declarative(
            Box::new(FakeSystemService::returning(vec![bare_snapshot()])),
            Box::new(FakePlatformService::default()),
            Box::new(NoScanner),
            "nixbox".to_string(),
            Keymap::default(),
            Some(Box::new(FakeDeclarative::default())),
        );
    }

    /// The worse direction, and the one `Registry` was added to prevent:
    /// a keymap offering `5` on a host with no service, so the digit opens
    /// a screen with nothing on it.
    #[test]
    #[should_panic(expected = "disagree about whether this host has a Nix view")]
    fn a_nix_keymap_with_no_service_is_caught() {
        let _ = App::with_declarative(
            Box::new(FakeSystemService::returning(vec![bare_snapshot()])),
            Box::new(FakePlatformService::default()),
            Box::new(NoScanner),
            "debian-box".to_string(),
            Keymap::for_registry(Registry::new(true)),
            None,
        );
    }
}
