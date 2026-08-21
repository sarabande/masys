//! The Nix view acting rather than reporting: a key becomes an operation
//! with arguments in it, and the operation reaches the port.
//!
//! Separate from `nix_view.rs`, which covers the reading half - what the
//! port said reaching a row. Nothing here executes a command: the fake
//! records what it was asked for, which is the only safe way to test a
//! module whose commands activate system configurations.

mod fake;

use fake::{FakeDeclarative, FakePlatformService, FakeSystemService, NoScanner, bare_snapshot};
use masys_app::buffer::Registry;
use masys_app::key::{Key, KeyCode};
use masys_app::keymap::Keymap;
use masys_app::{Aftermath, App, Flow};
use masys_domain::declarative::{Generation, NixOp, Profile, ProfileKind};
use masys_domain::unit::{ActiveState, Unit, UnitKind};
use masys_view::{KeyBinding, ModalView, Node};

type Ops = std::rc::Rc<std::cell::RefCell<Vec<NixOp>>>;
/// Mutations that reached `SystemService`, which is a different port from
/// the one the Nix operations use - and the whole point of the `r` tests
/// is which of the two a key arrives at.
type Calls = std::rc::Rc<std::cell::RefCell<Vec<String>>>;

/// A session on a host that has a declarative service, with the keymap
/// its registry demands - the pairing `App::with_declarative` asserts.
fn nixos(declarative: FakeDeclarative) -> App {
    nixos_running(declarative, Vec::new(), Default::default())
}

/// The same session with units in the sample, so the Nix view's own units
/// section has rows in it.
fn nixos_running(declarative: FakeDeclarative, units: Vec<Unit>, calls: Calls) -> App {
    let mut system = FakeSystemService::returning(vec![bare_snapshot()]);
    system.units = units;
    system.calls = calls;
    let mut app = App::with_declarative(
        Box::new(system),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "nixbox".to_string(),
        Keymap::for_registry(Registry::new(true)),
        Some(Box::new(declarative)),
    );
    app.tick(1_000, "t".to_string()).expect("tick");
    app.handle_key(Key::char('5'));
    app
}

/// One of this view's own units. `is_nix_unit` puts anything named
/// `nix-*` in the section, which is what makes a unit row reachable in
/// the Nix view at all - and therefore what makes `r` mean something
/// here.
fn nix_unit(name: &str) -> Unit {
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

/// A generation whose link resolves. `store_path` is the field every
/// operation on a generation is built out of, so the tests that matter
/// most are the ones where it is `None`.
fn generation(id: u64, current: bool) -> Generation {
    Generation {
        id,
        created_ms: Some(1_000),
        store_path: Some(format!("/nix/store/{id}-nixos-system")),
        label: Some("26.11".to_string()),
        kernel: None,
        current,
        booted: current,
    }
}

/// The ordinary shape: a system profile masys can write, which is what a
/// host has when masys is run as root. `writable` is spelled out on
/// every fixture rather than defaulted, because it now decides whether
/// two of the three keys act at all.
fn system_profile(generations: Vec<Generation>) -> Profile {
    Profile { kind: ProfileKind::System, path: "/nix/var/nix/profiles/system".to_string(), writable: Some(true), generations }
}

/// Two system generations, the newer of them current - this host's shape
/// in miniature.
fn two_generations(ops: &Ops) -> FakeDeclarative {
    FakeDeclarative {
        profiles: vec![system_profile(vec![generation(41, false), generation(42, true)])],
        ops: ops.clone(),
        ..Default::default()
    }
}

fn selected_generation(app: &App) -> Option<u64> {
    let view = app.view();
    match view.rows.get(view.selected?)? {
        Node::Generation { generation, .. } => Some(generation.id),
        _ => None,
    }
}

/// Walks the cursor down to a generation row, the way `on_a_process`
/// does in `actions.rs`. The Nix buffer opens on the Store section, so
/// every generation is some rows below wherever the cursor starts.
fn on_generation(app: &mut App, id: u64) {
    for _ in 0..40 {
        if selected_generation(app) == Some(id) {
            return;
        }
        app.handle_key(Key::new(KeyCode::Down));
    }
    panic!("no row for generation {id}");
}

fn key(app: &App, chord: &str) -> KeyBinding {
    app.view().actions.iter().find(|binding| binding.chord == chord).unwrap_or_else(|| panic!("the Nix view offers no {chord}")).clone()
}

/// Presses a row of the Nix transient: `e` opens the popup, the chord
/// runs the row, and the popup closes behind it. The Nix operations have
/// no top-level keys since Phase 3 - `a`, `d` and `c` were temporary and
/// this is where they went - so this is what pressing one now means.
fn transient_key(app: &mut App, chord: char) -> Flow {
    app.handle_key(Key::char('e'));
    app.handle_key(Key::char(chord))
}

/// One row of the Nix transient, as the popup draws it.
///
/// Where the footer used to be asked whether a key was dim, the popup's
/// own row carries the mark - the operations are rows now, and the footer
/// has no key of theirs to dim. Opens and closes the popup, so the app is
/// left where it was found.
fn transient_row(app: &mut App, chord: char) -> masys_view::ActionRow {
    app.handle_key(Key::char('e'));
    let found = match app.view().modal {
        Some(ModalView::Transient { groups, .. }) => groups.iter().flat_map(|group| &group.rows).find(|row| row.chord == chord).cloned(),
        _ => None,
    };
    app.handle_key(Key::new(KeyCode::Esc));
    found.unwrap_or_else(|| panic!("the Nix transient offers no `{chord}`"))
}

fn prompt(app: &App) -> String {
    match app.view().modal {
        Some(ModalView::Confirm { prompt }) => prompt.to_string(),
        _ => panic!("no confirmation is open"),
    }
}

/// Walks the cursor down to the unit row for `name`.
fn on_unit(app: &mut App, name: &str) {
    for _ in 0..40 {
        let found = {
            let view = app.view();
            matches!(view.rows.get(view.selected.unwrap_or(0)), Some(Node::Unit { unit, .. }) if unit.name == name)
        };
        if found {
            return;
        }
        app.handle_key(Key::new(KeyCode::Down));
    }
    panic!("no row for {name}");
}

/// The whole point of the change: a key becomes a real command, and the
/// terminal is handed over so the command can own the screen.
///
/// Both store paths are asserted rather than just the operation's kind.
/// `nix store diff-closures` takes two positional paths and reports the
/// second relative to the first, so an implementation that passed the
/// same path twice, or swapped them, would still produce a `NixOp::Diff`
/// and would still run.
#[test]
fn diffing_a_generation_names_the_row_and_the_profiles_current_path() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));
    on_generation(&mut app, 41);

    assert_eq!(transient_key(&mut app, 'd'), Flow::Suspend, "the session asks for the terminal");
    assert!(ops.borrow().is_empty(), "and nothing has run yet - the caller runs it");

    // What the binary does once it has released the terminal.
    app.run_suspended();

    assert_eq!(
        ops.borrow().as_slice(),
        [NixOp::Diff { from: "/nix/store/41-nixos-system".to_string(), to: "/nix/store/42-nixos-system".to_string() }]
    );
}

/// The reason the pause exists, stated where the operation is.
///
/// `nix store diff-closures` between generations 427 and 438 on this host
/// prints 84 lines and exits in 0.32 s. The composition root re-enters the
/// alternate screen as soon as this returns, and the alternate screen is
/// not where those 84 lines are, so an operation that reports `Seen` here
/// is an operation whose output the operator never sees.
///
/// A failing port answers the same way, and must: a `nixos-rebuild` that
/// stopped on an evaluation error has printed the error and nothing else
/// will ever say what it was.
#[test]
fn a_nix_operation_leaves_output_nobody_has_read() {
    let ops: Ops = Default::default();
    let declarative = two_generations(&ops);
    let failing = declarative.failing.clone();
    let mut app = nixos(declarative);
    on_generation(&mut app, 41);

    transient_key(&mut app, 'd');
    assert_eq!(app.run_suspended(), Aftermath::Unseen);

    failing.set(true);
    on_generation(&mut app, 41);
    transient_key(&mut app, 'd');
    assert_eq!(app.run_suspended(), Aftermath::Unseen, "and a failed operation most of all");
}

/// A diff reads two store paths and prints. It asks nothing first,
/// deliberately: a confirmation in front of an operation that changes
/// nothing teaches the operator that confirmations are noise, and the
/// next one they wave through will be a rebuild.
#[test]
fn a_diff_asks_nothing_before_it_runs() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));
    on_generation(&mut app, 41);

    transient_key(&mut app, 'd');

    assert!(app.view().modal.is_none(), "a read-only operation must not prompt");
    app.run_suspended();
    assert_eq!(ops.borrow().len(), 1, "and it ran on the one keypress");
}

/// `d` on the Store header resolves to the diff action and then quietly
/// does nothing, the way `k` does on a process group's header row. There
/// is no generation there to diff.
#[test]
fn diff_on_a_row_that_is_not_a_generation_runs_nothing() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));
    assert_eq!(selected_generation(&app), None, "the buffer opens on the Store section, not on a generation");

    assert_eq!(transient_key(&mut app, 'd'), Flow::Continue, "the terminal is not wanted");
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
}

/// And the popup says so rather than offering a row that will not act.
/// This codebase has twice shipped a footer that disagreed with what a
/// key did; the marking and the handler read the same function so they
/// cannot.
///
/// Off a generation row the Generation group is not marked - it is
/// *absent*, which is the one exception the design makes to "mark, never
/// hide". A group is hidden where it has no subject at all; a row is
/// marked where it has one it cannot act on. `activate` and `delete`
/// with no generation named would be the two most destructive rows in
/// masys with nothing on screen saying what they act on.
#[test]
fn the_generation_group_appears_only_on_a_generation_row() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    app.handle_key(Key::char('e'));
    let Some(ModalView::Transient { groups, .. }) = app.view().modal else { panic!("no transient") };
    assert!(!groups.iter().any(|group| group.heading.starts_with("Generation")), "the Store header has no generation");
    // And the popup is still worth opening: everything that does not need
    // a generation is listed.
    assert!(groups.iter().any(|group| group.heading == "Rebuild"), "{groups:#?}");
    app.handle_key(Key::new(KeyCode::Esc));

    on_generation(&mut app, 41);
    app.handle_key(Key::char('e'));
    let Some(ModalView::Transient { groups, .. }) = app.view().modal else { panic!("no transient") };
    let generation = groups.iter().find(|group| group.heading.starts_with("Generation")).expect("a Generation group");

    assert_eq!(generation.heading, "Generation 41", "and it names the one it acts on");
    assert!(generation.rows.iter().all(|row| !row.dimmed), "whose rows act: {generation:?}");
}

/// A generation link that could not be resolved carries `None`, and
/// `None` here means "masys could not find out", never "no path". Handing
/// `nix store diff-closures` one argument would fail; papering the
/// unknown over with a default would be worse, because two unresolved
/// generations would compare equal and the diff would report no change.
#[test]
fn a_generation_whose_link_dangles_is_never_diffed() {
    let ops: Ops = Default::default();
    let dangling = Generation { store_path: None, ..generation(41, false) };
    let declarative =
        FakeDeclarative { profiles: vec![system_profile(vec![dangling, generation(42, true)])], ops: ops.clone(), ..Default::default() };
    let mut app = nixos(declarative);
    on_generation(&mut app, 41);

    assert_eq!(transient_key(&mut app, 'd'), Flow::Continue);
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(transient_row(&mut app, 'd').dimmed, "and the key is marked, not silently inert");
}

/// The other half of the same rule, and the half that is easy to miss:
/// the row under the cursor resolves perfectly well, and it is *current*
/// that could not be read. "Against current" has no meaning then.
#[test]
fn a_current_generation_that_could_not_be_resolved_is_not_diffed_against() {
    let ops: Ops = Default::default();
    let current = Generation { store_path: None, ..generation(42, true) };
    let declarative =
        FakeDeclarative { profiles: vec![system_profile(vec![generation(41, false), current])], ops: ops.clone(), ..Default::default() };
    let mut app = nixos(declarative);
    on_generation(&mut app, 41);

    assert_eq!(transient_key(&mut app, 'd'), Flow::Continue);
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
}

/// A host whose profile has no current generation at all - which is what
/// a `readdir` that found the numbered links but not the `system` symlink
/// looks like. There is nothing to diff against, and masys must not pick
/// the newest and call it current.
#[test]
fn a_profile_with_no_current_generation_offers_no_diff() {
    let ops: Ops = Default::default();
    let declarative = FakeDeclarative {
        profiles: vec![system_profile(vec![generation(41, false), generation(42, false)])],
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);
    on_generation(&mut app, 41);

    assert_eq!(transient_key(&mut app, 'd'), Flow::Continue);
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(transient_row(&mut app, 'd').dimmed);
}

/// Activating a generation changes what is running and what boots, so it
/// goes through the same `Pending` and `ModalView::Confirm` the ten unit
/// verbs and both signals already use.
///
/// The prompt names the generation and it names the boot default, on
/// `ask_kill`'s standard: it names the signal because SIGKILL cannot be
/// caught and the difference is the whole reason there are two bindings.
/// Here the difference is that `switch-to-configuration switch` installs
/// the bootloader entry as well as activating - an operator who read this
/// as "just for now" would be wrong in the way that matters.
#[test]
fn activating_a_generation_asks_first_and_names_what_it_will_do() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));
    on_generation(&mut app, 41);

    assert_eq!(transient_key(&mut app, 'a'), Flow::Continue, "the terminal is not wanted until it is answered");
    let asked = prompt(&app);
    assert!(asked.contains("generation 41"), "{asked}");
    assert!(asked.contains("boot default"), "{asked}");
    assert!(ops.borrow().is_empty(), "nothing has run yet");

    assert_eq!(app.handle_key(Key::char('y')), Flow::Suspend, "the session asks for the terminal");
    app.run_suspended();

    assert_eq!(
        ops.borrow().as_slice(),
        [NixOp::Activate { profile: "/nix/var/nix/profiles/system".to_string(), generation: 41 }],
        "the profile path is the port's own, not one masys composed"
    );
}

/// `y` confirms; anything else does not. An activation that happened
/// because a key was pressed twice is a machine on a different
/// configuration than the operator thinks.
#[test]
fn declining_an_activation_runs_nothing() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));
    on_generation(&mut app, 41);
    transient_key(&mut app, 'a');

    assert_eq!(app.handle_key(Key::char('n')), Flow::Continue);
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
}

/// Only a system generation can be activated, and not out of caution:
/// `NixOp::Activate`'s second command is the profile's own
/// `bin/switch-to-configuration`, which a system generation has and a
/// home one does not - measured on this host, where
/// `/nix/var/nix/profiles/system/bin` holds exactly that file and
/// `~/.local/state/nix/profiles/profile/bin` holds no activation script
/// at all. Offering the key here would move the profile and then fail on
/// a missing file, leaving it switched and nothing activated.
#[test]
fn a_home_generation_is_never_activated() {
    let ops: Ops = Default::default();
    let home = Profile {
        kind: ProfileKind::Home,
        path: "/home/user/.local/state/nix/profiles/profile".to_string(),
        writable: Some(true),
        generations: vec![generation(46, false), generation(47, true)],
    };
    let declarative =
        FakeDeclarative { profiles: vec![system_profile(vec![generation(42, true)]), home], ops: ops.clone(), ..Default::default() };
    let mut app = nixos(declarative);
    on_generation(&mut app, 46);

    assert_eq!(transient_key(&mut app, 'a'), Flow::Continue);
    assert!(app.view().modal.is_none(), "nothing is even asked");
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(transient_row(&mut app, 'a').dimmed, "and the footer marks it rather than offering it");
}

/// The whole reason `Profile::writable` exists, and the sharpest edge on
/// this view.
///
/// `nix-collect-garbage --delete-older-than` acts on *every* profile it
/// finds - nix-collect-garbage(1) for nix 2.34.8: "it looks in a few
/// locations, and acts on all profiles it finds there" - and it deletes
/// as it goes. Run by an ordinary user on a host whose home profile is
/// theirs and whose system profile is root's, it deletes their
/// home-manager generations and *then* fails, reporting a non-zero exit
/// and nothing about what already went. "Deleting previous
/// configurations makes rollbacks to them impossible", in the same page.
///
/// There is no elevation to reach for: `nix-collect-garbage`'s synopsis
/// carries no `--elevate` and it triggers no polkit action, unlike the
/// unit verbs. So the key is marked and the command never starts.
#[test]
fn a_clean_that_could_only_half_delete_is_never_offered() {
    let ops: Ops = Default::default();
    let home = Profile {
        kind: ProfileKind::Home,
        path: "/home/user/.local/state/nix/profiles/profile".to_string(),
        writable: Some(true),
        generations: vec![generation(46, true)],
    };
    let root_owned = Profile { writable: Some(false), ..system_profile(vec![generation(42, true)]) };
    let declarative =
        FakeDeclarative { profiles: vec![root_owned, home], gc_retention: Some("14d".to_string()), ops: ops.clone(), ..Default::default() };
    let mut app = nixos(declarative);

    assert_eq!(transient_key(&mut app, 'c'), Flow::Continue);
    assert!(app.view().modal.is_none(), "nothing is even asked");
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(transient_row(&mut app, 'c').dimmed, "and the footer marks it rather than offering it");
}

/// A profile whose writability could not be determined is not a profile
/// masys may delete from. `None` here is "I could not find out", and the
/// direction that costs a keypress is the right one against the
/// direction that costs generations.
#[test]
fn a_clean_is_never_offered_on_an_unmeasured_profile() {
    let ops: Ops = Default::default();
    let unknown = Profile { writable: None, ..system_profile(vec![generation(42, true)]) };
    let declarative =
        FakeDeclarative { profiles: vec![unknown], gc_retention: Some("14d".to_string()), ops: ops.clone(), ..Default::default() };
    let mut app = nixos(declarative);

    assert_eq!(transient_key(&mut app, 'c'), Flow::Continue);
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(transient_row(&mut app, 'c').dimmed);
}

/// `all` over an empty list is vacuously true, which would offer the key
/// on a host masys found no profile to check - the fabricated-zero shape
/// one level up, where the fabrication is permission rather than a
/// reading.
#[test]
fn a_clean_is_never_offered_where_no_profile_was_found_to_check() {
    let ops: Ops = Default::default();
    let declarative =
        FakeDeclarative { profiles: Vec::new(), gc_retention: Some("14d".to_string()), ops: ops.clone(), ..Default::default() };
    let mut app = nixos(declarative);

    assert_eq!(transient_key(&mut app, 'c'), Flow::Continue);
    // The confirmation, not the run: pressing `c` never runs anything on
    // its own, so asserting only that nothing reached the port would pass
    // against the bug this is named for. Asserted for real when the guard
    // was removed and this test stayed green.
    assert!(app.view().modal.is_none(), "nothing is even asked");
    assert!(transient_row(&mut app, 'c').dimmed);
    let _ = &ops;
}

/// The system profile is `drwxr-xr-x root root` on a NixOS host, so an
/// unprivileged `a` cannot move it. Harmless if attempted - `nix-env`
/// refuses and nothing changes - but `nix-env` has no `--elevate` and
/// triggers no polkit action, so the refusal is certain and belongs on
/// the key rather than in an error after a confirmation.
#[test]
fn activating_is_never_offered_where_the_profile_cannot_be_written() {
    let ops: Ops = Default::default();
    let root_owned = Profile { writable: Some(false), ..system_profile(vec![generation(41, false), generation(42, true)]) };
    let declarative = FakeDeclarative { profiles: vec![root_owned], ops: ops.clone(), ..Default::default() };
    let mut app = nixos(declarative);
    on_generation(&mut app, 41);

    assert_eq!(transient_key(&mut app, 'a'), Flow::Continue);
    assert!(app.view().modal.is_none(), "nothing is even asked");
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(transient_row(&mut app, 'a').dimmed);
}

/// And `d` stays live throughout. `nix store diff-closures` reads two
/// store paths and writes nothing, so a read-only profile directory is
/// no reason to withhold it - a privilege check that dimmed the whole
/// view would be worse than the problem it solves.
#[test]
fn a_diff_is_offered_whatever_the_profile_permissions_say() {
    let ops: Ops = Default::default();
    let root_owned = Profile { writable: Some(false), ..system_profile(vec![generation(41, false), generation(42, true)]) };
    let declarative = FakeDeclarative { profiles: vec![root_owned], ops: ops.clone(), ..Default::default() };
    let mut app = nixos(declarative);
    on_generation(&mut app, 41);

    assert!(!transient_row(&mut app, 'd').dimmed);
    assert_eq!(transient_key(&mut app, 'd'), Flow::Suspend);
    app.run_suspended();

    assert_eq!(ops.borrow().len(), 1, "{:?}", ops.borrow());
}

/// The retention is the host's own declared policy, read out of
/// `nix-gc.service`. masys never picks one: a built-in default would be
/// generations deleted on a number nobody chose.
///
/// "in every profile" is in the prompt because that is what happens and
/// it is not what the row under the cursor suggests. nix-collect-garbage(1)
/// for nix 2.34.8: `--delete-older-than` acts on every profile it finds,
/// not the one the cursor is in.
#[test]
fn cleaning_uses_the_hosts_own_retention_and_says_what_it_deletes() {
    let ops: Ops = Default::default();
    let declarative = FakeDeclarative {
        profiles: vec![system_profile(vec![generation(41, false), generation(42, true)])],
        gc_retention: Some("14d".to_string()),
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);

    assert_eq!(transient_key(&mut app, 'c'), Flow::Continue);
    let asked = prompt(&app);
    assert!(asked.contains("older than 14d"), "{asked}");
    assert!(asked.contains("every profile"), "{asked}");

    assert_eq!(app.handle_key(Key::char('y')), Flow::Suspend);
    app.run_suspended();

    assert_eq!(ops.borrow().as_slice(), [NixOp::Clean { older_than: "14d".to_string() }]);
}

/// A host whose gc job declares no `--delete-older-than` has no retention
/// to clean by, and masys must not supply one. The key is marked rather
/// than quietly inert.
#[test]
fn a_host_that_declares_no_retention_offers_no_clean() {
    let ops: Ops = Default::default();
    let declarative = FakeDeclarative {
        profiles: vec![system_profile(vec![generation(42, true)])],
        gc_retention: None,
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);

    assert_eq!(transient_key(&mut app, 'c'), Flow::Continue);
    assert!(app.view().modal.is_none(), "nothing to ask about");
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(transient_row(&mut app, 'c').dimmed);
}

/// The collision, pinned from the unit side. `r` is `unit_restart`, it
/// resolves in this view because this view shows units, and on a unit row
/// it still restarts - the Nix view's own operations were given their own
/// letters rather than a second meaning for this one.
#[test]
fn r_still_restarts_the_unit_under_the_cursor_in_the_nix_view() {
    let ops: Ops = Default::default();
    let calls: Calls = Default::default();
    let mut app = nixos_running(two_generations(&ops), vec![nix_unit("nix-daemon.service")], calls.clone());
    on_unit(&mut app, "nix-daemon.service");

    app.handle_key(Key::char('r'));
    assert!(prompt(&app).contains("restart nix-daemon.service"), "{}", prompt(&app));
    app.handle_key(Key::char('y'));

    assert_eq!(calls.borrow().as_slice(), ["restart nix-daemon.service"]);
    assert!(ops.borrow().is_empty(), "no Nix operation was dispatched");
}

/// And from the generation side, which is the half that decided the
/// binding. `r` on a generation resolves to `Action::Unit(Restart)`,
/// finds no unit and returns - the shape `ask_kill` already uses for `k`
/// on a group row. It does not roll back, and the footer marks it so the
/// operator is not offered a restart against a row that has no unit.
#[test]
fn r_on_a_generation_row_restarts_nothing_and_activates_nothing() {
    let ops: Ops = Default::default();
    let calls: Calls = Default::default();
    let mut app = nixos_running(two_generations(&ops), vec![nix_unit("nix-daemon.service")], calls.clone());
    on_generation(&mut app, 41);

    assert_eq!(app.handle_key(Key::char('r')), Flow::Continue);

    assert!(app.view().modal.is_none(), "no confirmation, because there is nothing to confirm");
    assert!(calls.borrow().is_empty(), "{:?}", calls.borrow());
    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(key(&app, "r").dimmed, "the footer must not offer restart against a generation");
    assert!(!transient_row(&mut app, 'a').dimmed, "while the key that does act here is lit");
}

// --- Phase 3: the Nix transient ---

/// A host with a flake reference and a system profile, so the transient
/// has something to be contextual about.
fn flake_host(ops: &Ops) -> FakeDeclarative {
    FakeDeclarative {
        profiles: vec![system_profile(vec![generation(41, false), generation(42, true)])],
        gc_retention: Some("14d".to_string()),
        flake_ref: Some("/home/user/.dotfiles".to_string()),
        inputs: Some(flake_inputs()),
        ops: ops.clone(),
        ..Default::default()
    }
}

fn flake_inputs() -> masys_domain::declarative::Inputs {
    masys_domain::declarative::Inputs {
        source: masys_domain::declarative::InputSource::Flake { lock_path: "/home/user/.dotfiles/flake.lock".to_string() },
        inputs: Vec::new(),
    }
}

fn channels_inputs() -> masys_domain::declarative::Inputs {
    masys_domain::declarative::Inputs { source: masys_domain::declarative::InputSource::Channels, inputs: Vec::new() }
}

/// Opens the transient and hands back its groups.
fn transient(app: &mut App) -> Vec<masys_view::ActionGroup> {
    app.handle_key(Key::char('e'));
    let Some(ModalView::Transient { groups, .. }) = app.view().modal else { panic!("no transient") };
    groups
}

fn find(groups: &[masys_view::ActionGroup], chord: char) -> masys_view::ActionRow {
    groups.iter().flat_map(|group| &group.rows).find(|row| row.chord == chord).unwrap_or_else(|| panic!("no `{chord}` row")).clone()
}

/// `e` opens the whole `[C2]` operation set, which is what the transient
/// exists for: thirteen operations reachable without spending thirteen
/// top-level letters.
#[test]
fn the_nix_transient_lists_the_designs_groups() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    on_generation(&mut app, 41);
    let groups = transient(&mut app);

    assert_eq!(
        groups.iter().map(|group| group.heading.as_str()).collect::<Vec<_>>(),
        vec!["Rebuild", "Home-manager", "Generation 41", "Inputs", "Store", "Search"]
    );
    // Every operation `NixOp` names and nothing invented to fill a group.
    for chord in ['s', 'b', 't', 'B', 'y', 'R', 'h', 'a', 'd', 'x', 'f', 'F', 'n', 'N', 'c', 'D', 'p', 'v'] {
        find(&groups, chord);
    }
}

/// The rule the design states twice because the symmetric version reads
/// fine and is wrong: a bare `nixos-rebuild switch` is the correct
/// command on a channels host, so the rebuild verbs stay **live** with no
/// flake reference. Only the genuinely flake-only pair is marked.
#[test]
fn the_rebuild_verbs_stay_live_with_no_flake_reference() {
    let ops: Ops = Default::default();
    let declarative = FakeDeclarative {
        profiles: vec![system_profile(vec![generation(42, true)])],
        inputs: Some(channels_inputs()),
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);
    let groups = transient(&mut app);

    for chord in ['s', 'b', 't', 'B', 'y', 'R'] {
        assert!(!find(&groups, chord).dimmed, "`{chord}` is a rebuild verb and runs bare");
    }
    for chord in ['f', 'F'] {
        let row = find(&groups, chord);
        assert!(row.dimmed, "`{chord}` is flake-only");
        assert_eq!(row.note.as_deref(), Some("no flake configured"), "and says why");
    }
    for chord in ['n', 'N'] {
        assert!(!find(&groups, chord).dimmed, "`{chord}` is what this host actually has");
    }
}

/// And the mirror. The channel operations dim on a flake host by the same
/// rule that dims the flake ones on a channels host.
#[test]
fn the_channel_operations_dim_on_a_flake_host() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    let groups = transient(&mut app);

    for chord in ['f', 'F'] {
        assert!(!find(&groups, chord).dimmed, "`{chord}` is what this host has");
    }
    for chord in ['n', 'N'] {
        let row = find(&groups, chord);
        assert!(row.dimmed, "`{chord}` acts on channels this host does not read");
        assert_eq!(row.note.as_deref(), Some("no channels on this host"));
    }
}

/// `home switch` in `Module` is not merely ineffective - there is no
/// `home-manager` binary to run, because home-manager was generated into
/// the system closure. The design's own wording is the note.
#[test]
fn home_switch_is_marked_where_nixos_rebuild_activates_it() {
    let ops: Ops = Default::default();
    let module = FakeDeclarative { home_mode: Some(masys_domain::declarative::HomeMode::Module), ..flake_host(&ops) };
    let mut app = nixos(module);
    let row = find(&transient(&mut app), 'h');

    assert!(row.dimmed);
    assert_eq!(row.note.as_deref(), Some("activated by nixos-rebuild switch"));
}

/// And in standalone mode it is the one host where it acts.
#[test]
fn home_switch_acts_in_standalone_mode() {
    let ops: Ops = Default::default();
    let standalone = FakeDeclarative { home_mode: Some(masys_domain::declarative::HomeMode::Standalone), ..flake_host(&ops) };
    let mut app = nixos(standalone);
    let row = find(&transient(&mut app), 'h');

    assert!(!row.dimmed);
    assert_eq!(row.note, None, "and nothing is said, because nothing is wrong");
}

/// `clean` carries the period it will use. "delete old generations" with
/// no number is the row an operator should not press.
#[test]
fn clean_says_the_retention_it_will_delete_by() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));

    assert_eq!(find(&transient(&mut app), 'c').note.as_deref(), Some("--delete-older-than 14d"));
}

/// A host that declares no retention and a host whose retention could not
/// be read are different facts, and the row says which. Both mark it -
/// neither is a period to delete generations by - but only one of them is
/// the host's own answer.
#[test]
fn an_unread_retention_and_an_undeclared_one_read_differently() {
    let ops: Ops = Default::default();

    let declared_none = FakeDeclarative { gc_retention: None, ..flake_host(&ops) };
    let mut app = nixos(declared_none);
    let row = find(&transient(&mut app), 'c');
    assert!(row.dimmed);
    assert_eq!(row.note.as_deref(), Some("no retention declared"), "the port answered, and its answer was none");

    let unread = FakeDeclarative { gc_retention: None, failing: std::rc::Rc::new(std::cell::Cell::new(true)), ..flake_host(&ops) };
    let mut app = nixos(unread);
    let row = find(&transient(&mut app), 'c');
    assert!(row.dimmed);
    assert_eq!(row.note.as_deref(), Some("retention unread"), "no read has come back at all");
}

/// `search packages` has no source but the operator: a query is not a row
/// and not a reading. Pressing it opens the prompt rather than running
/// anything, which is what `NixOffer::Asks` exists to say.
#[test]
fn a_verb_that_needs_a_value_opens_the_prompt_instead_of_running() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    let groups = transient(&mut app);
    assert!(!find(&groups, 'p').dimmed, "the row is live - what is missing is the value, not the host");

    app.handle_key(Key::char('p'));
    let Some(ModalView::Input { prompt, typed, .. }) = app.view().modal else { panic!("no prompt") };
    assert_eq!(prompt, "search nixpkgs");
    assert_eq!(typed, "");
    assert!(ops.borrow().is_empty(), "and nothing ran: {:?}", ops.borrow());
}

/// Typing, then enter, builds the operation from what was typed and
/// suspends into it - a search only reads, so it asks nothing first.
#[test]
fn a_typed_query_reaches_the_port_as_the_operation_it_names() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    transient(&mut app);
    app.handle_key(Key::char('p'));
    for c in "ripgrep".chars() {
        app.handle_key(Key::char(c));
    }

    let Some(ModalView::Input { typed, .. }) = app.view().modal else { panic!("no prompt") };
    assert_eq!(typed, "ripgrep");

    assert_eq!(app.handle_key(Key::new(KeyCode::Enter)), Flow::Suspend, "a search takes the terminal");
    app.run_suspended();
    assert_eq!(*ops.borrow(), vec![NixOp::SearchPackages { query: "ripgrep".to_string() }]);
}

/// Backspace deletes, and escape backs out without running.
#[test]
fn the_prompt_edits_and_backs_out() {
    let ops: Ops = Default::default();
    // A host `nixos-option` works on, since that is the prompt being
    // typed into - on this development host the row is marked, for the
    // reason `option_value_is_marked_where_no_nixos_config_resolves`
    // records.
    let mut app = nixos(FakeDeclarative { nixos_config: Some("/etc/nixos/configuration.nix".to_string()), ..flake_host(&ops) });
    transient(&mut app);
    app.handle_key(Key::char('v'));
    for c in "boot.loaderX".chars() {
        app.handle_key(Key::char(c));
    }
    app.handle_key(Key::new(KeyCode::Backspace));

    let Some(ModalView::Input { prompt, typed, .. }) = app.view().modal else { panic!("no prompt") };
    assert_eq!(prompt, "option");
    assert_eq!(typed, "boot.loader");

    app.handle_key(Key::new(KeyCode::Esc));
    assert!(app.view().modal.is_none(), "the prompt is gone");
    assert!(ops.borrow().is_empty(), "and nothing ran: {:?}", ops.borrow());
}

/// Enter on an empty prompt does nothing. `nix search nixpkgs ""` matches
/// every package in nixpkgs and `nix-env --delete-generations ""` is an
/// argument error; neither is what pressing enter on an empty prompt
/// meant, and neither is worth finding out by running it.
#[test]
fn an_empty_prompt_does_not_commit() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    transient(&mut app);
    app.handle_key(Key::char('p'));

    assert_eq!(app.handle_key(Key::new(KeyCode::Enter)), Flow::Continue);
    assert!(matches!(app.view().modal, Some(ModalView::Input { .. })), "the prompt is still waiting");
    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
}

/// A typed spec still goes through the confirmation, because deleting
/// generations changes the machine. The prompt names what it will do.
#[test]
fn a_typed_generation_spec_is_confirmed_before_it_runs() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    transient(&mut app);
    app.handle_key(Key::char('D'));
    for c in "+5".chars() {
        app.handle_key(Key::char(c));
    }
    app.handle_key(Key::new(KeyCode::Enter));

    assert!(prompt(&app).contains("delete"), "{}", prompt(&app));
    assert!(ops.borrow().is_empty(), "not before it is answered: {:?}", ops.borrow());

    app.handle_key(Key::char('y'));
    app.run_suspended();
    assert_eq!(
        *ops.borrow(),
        vec![NixOp::DeleteGenerations { profile: "/nix/var/nix/profiles/system".to_string(), spec: "+5".to_string() }]
    );
}

/// The generation under the cursor supplies its own spec, so `x` asks for
/// no value - only for confirmation.
#[test]
fn deleting_the_generation_under_the_cursor_needs_no_prompt() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    on_generation(&mut app, 41);
    transient(&mut app);
    app.handle_key(Key::char('x'));

    assert!(prompt(&app).contains("delete"), "{}", prompt(&app));
    app.handle_key(Key::char('y'));
    app.run_suspended();
    assert_eq!(
        *ops.borrow(),
        vec![NixOp::DeleteGenerations { profile: "/nix/var/nix/profiles/system".to_string(), spec: "41".to_string() }]
    );
}

/// `nixos-option` reads `<nixos-config>` and there is no flag to hand it
/// one. This host is a flake install with no `nix.nixPath` and no
/// `/etc/nixos`, and the command fails there before it evaluates
/// anything:
///
/// ```text
/// error: file 'nixos-config' was not found in the Nix search path
/// ```
///
/// Found by running it, which is the only way this would have been
/// found: every synopsis check passes and the argv is correct.
#[test]
fn option_value_is_marked_where_no_nixos_config_resolves() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    let row = find(&transient(&mut app), 'v');

    assert!(row.dimmed, "the row would fail before it evaluated anything");
    assert_eq!(row.note.as_deref(), Some("no nixos-config on the search path"));

    // And pressing it opens no prompt, because there is nothing a typed
    // option name could be read against.
    app.handle_key(Key::char('v'));
    assert!(app.view().modal.is_none(), "no prompt is opened for a row that cannot act");
}

/// And a host that has one - a channels install, or a flake host that
/// sets `nix.nixPath` - reaches it. The mark is about the search path,
/// not about the paradigm.
#[test]
fn option_value_acts_where_a_nixos_config_resolves() {
    let ops: Ops = Default::default();
    let configured = FakeDeclarative { nixos_config: Some("/etc/nixos/configuration.nix".to_string()), ..flake_host(&ops) };
    let mut app = nixos(configured);
    let row = find(&transient(&mut app), 'v');

    assert!(!row.dimmed, "a flake host with a nixPath is still a host nixos-option works on");
    assert_eq!(row.note, None);

    app.handle_key(Key::char('v'));
    assert!(matches!(app.view().modal, Some(ModalView::Input { .. })), "and it asks for the option name");
}

/// Every marked row says why it is marked. A mark with no reason beside
/// it is the one thing the design's marking rule exists to prevent - it
/// reads as "this tool cannot do that" rather than teaching something
/// true about the host.
#[test]
fn no_row_is_marked_without_saying_why() {
    let ops: Ops = Default::default();
    // The development host, measured: a flake install running
    // unprivileged, home-manager as a module, no channels, no
    // nixos-config. Seven of eighteen rows are marked here, which makes
    // it the case worth asking.
    let unprivileged = Profile {
        kind: ProfileKind::System,
        path: "/nix/var/nix/profiles/system".to_string(),
        writable: Some(false),
        generations: vec![generation(437, false), generation(438, true)],
    };
    let declarative =
        FakeDeclarative { profiles: vec![unprivileged], home_mode: Some(masys_domain::declarative::HomeMode::Module), ..flake_host(&ops) };
    let mut app = nixos(declarative);
    on_generation(&mut app, 437);
    let groups = transient(&mut app);

    let marked: Vec<&masys_view::ActionRow> = groups.iter().flat_map(|group| &group.rows).filter(|row| row.dimmed).collect();
    assert!(marked.len() >= 7, "this host marks most of the popup: {marked:#?}");
    for row in marked {
        assert!(row.note.is_some(), "`{}` is marked and says nothing: {row:?}", row.label);
    }
}

/// And the reason is the *blocking* one. `clean` marked
/// `--delete-older-than 14d` reads as a row that will do that; on an
/// unprivileged masys it will not, and the retention is not why.
#[test]
fn a_marked_clean_says_the_privilege_not_the_retention() {
    let ops: Ops = Default::default();
    let unprivileged = Profile {
        kind: ProfileKind::System,
        path: "/nix/var/nix/profiles/system".to_string(),
        writable: Some(false),
        generations: vec![generation(438, true)],
    };
    let mut app = nixos(FakeDeclarative { profiles: vec![unprivileged], ..flake_host(&ops) });
    let row = find(&transient(&mut app), 'c');

    assert!(row.dimmed);
    assert_eq!(row.note.as_deref(), Some("every profile must be writable - run masys as root"));
}

/// A home generation marks `activate` for a reason of its own: the second
/// command is the profile's own `bin/switch-to-configuration`, and only a
/// system generation has one.
#[test]
fn a_home_generation_says_why_it_cannot_be_activated() {
    let ops: Ops = Default::default();
    let home = Profile {
        kind: ProfileKind::Home,
        path: "/home/user/.local/state/nix/profiles/profile".to_string(),
        writable: Some(true),
        generations: vec![generation(46, false), generation(47, true)],
    };
    let declarative = FakeDeclarative { profiles: vec![system_profile(vec![generation(438, true)]), home], ..flake_host(&ops) };
    let mut app = nixos(declarative);
    on_generation(&mut app, 46);
    let row = find(&transient(&mut app), 'a');

    assert!(row.dimmed);
    assert_eq!(row.note.as_deref(), Some("only a system generation activates"));
}
