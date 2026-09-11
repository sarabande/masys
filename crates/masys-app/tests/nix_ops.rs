//! The Nix buffer acting rather than reporting: a key becomes an operation
//! with arguments in it, and the operation reaches the port.
//!
//! Separate from `nix_session.rs`, which covers the reading half - what the
//! port said reaching a row. Nothing here executes a command: the fake
//! records what it was asked for, which is the only safe way to test a
//! module whose commands activate system configurations.

mod fake;

use fake::{FakeDeclarative, FakePlatformService, FakeSystemService, NoScanner, bare_snapshot};
use masys_app::buffer::{BufferGates, Registry};
use masys_app::key::{Key, KeyCode};
use masys_app::keymap::{ACTIONS, Action, Keymap, NixVerb};
use masys_app::nix_buffer::{NixBuffer, NixOffer};
use masys_app::{Aftermath, App, Flow};
use masys_domain::declarative::{Generation, NixOp, Profile, ProfileKind, RebuildVerb};
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

/// The same session with units in the sample, so the Nix buffer's own units
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
        Keymap::for_registry(Registry::new(BufferGates {
            declarative: true,
            packages: false,
        })),
        Some(Box::new(declarative)),
    );
    app.tick(1_000, "t".to_string());
    app.handle_key(Key::char('5'));
    app
}

/// One of this buffer's own units. `is_nix_unit` puts anything named
/// `nix-*` in the section, which is what makes a unit row reachable in
/// the Nix buffer at all - and therefore what makes `r` mean something
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
    Profile {
        kind: ProfileKind::System,
        path: "/nix/var/nix/profiles/system".to_string(),
        writable: Some(true),
        generations,
    }
}

/// Two system generations, the newer of them current - this host's shape
/// in miniature.
fn two_generations(ops: &Ops) -> FakeDeclarative {
    FakeDeclarative {
        profiles: vec![system_profile(vec![
            generation(41, false),
            generation(42, true),
        ])],
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
    app.view()
        .actions
        .iter()
        .find(|binding| binding.chord == chord)
        .unwrap_or_else(|| panic!("the Nix buffer offers no {chord}"))
        .clone()
}

/// Presses a row of a Nix family menu: `family` opens the popup, `chord`
/// runs the row, and the popup closes behind it. Every Nix operation but
/// `switch` and home-manager `switch` - both direct now - lives behind one
/// of the five family letters (`b`/`a`/`i`/`c`/`f`) rather than the single
/// `e` Phase 3 collapsed them into.
fn transient_key(app: &mut App, family: char, chord: char) -> Flow {
    app.handle_key(Key::char(family));
    app.handle_key(Key::char(chord))
}

/// One row of a Nix family menu, as the popup draws it.
///
/// Where the footer used to be asked whether a key was dim, the popup's
/// own row carries the mark - the operations are rows now, and the footer
/// has no key of theirs to dim. Opens and closes the popup, so the app is
/// left where it was found.
fn transient_row(app: &mut App, family: char, chord: char) -> masys_view::ActionRow {
    app.handle_key(Key::char(family));
    let found = match app.view().modal {
        Some(ModalView::Transient { groups, .. }) => groups
            .iter()
            .flat_map(|group| &group.rows)
            .find(|row| row.chord == chord)
            .cloned(),
        _ => None,
    };
    app.handle_key(Key::new(KeyCode::Esc));
    found.unwrap_or_else(|| panic!("the `{family}` popup offers no `{chord}`"))
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

    assert_eq!(
        transient_key(&mut app, 'a', 'd'),
        Flow::Suspend,
        "the session asks for the terminal"
    );
    assert!(
        ops.borrow().is_empty(),
        "and nothing has run yet - the caller runs it"
    );

    // What the binary does once it has released the terminal.
    app.run_suspended();

    assert_eq!(
        ops.borrow().as_slice(),
        [NixOp::Diff {
            from: "/nix/store/41-nixos-system".to_string(),
            to: "/nix/store/42-nixos-system".to_string()
        }]
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

    transient_key(&mut app, 'a', 'd');
    assert_eq!(app.run_suspended(), Aftermath::Unseen);

    failing.set(true);
    on_generation(&mut app, 41);
    transient_key(&mut app, 'a', 'd');
    assert_eq!(
        app.run_suspended(),
        Aftermath::Unseen,
        "and a failed operation most of all"
    );
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

    transient_key(&mut app, 'a', 'd');

    assert!(
        app.view().modal.is_none(),
        "a read-only operation must not prompt"
    );
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
    assert_eq!(
        selected_generation(&app),
        None,
        "the buffer opens on the Store section, not on a generation"
    );

    assert_eq!(
        transient_key(&mut app, 'a', 'd'),
        Flow::Continue,
        "the terminal is not wanted"
    );
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
}

/// And the popup says so rather than silently doing nothing. This
/// codebase has shipped a footer that disagreed with what a key did more
/// than once; the marking and the handler read the same function so they
/// cannot.
///
/// The Generation menu opens off a generation row too, now that `a`
/// reaches it directly rather than only existing when a generation row
/// built it - so this is the one place in masys where a popup opens with
/// *every* row of it marked, rather than the group simply being absent.
/// `activate` and `delete` running against no generation at all would be
/// two of the most destructive rows in masys with nothing on screen
/// saying what they act on; being unreachable that way is what "mark,
/// never hide" buys here.
#[test]
fn the_generation_menu_marks_every_row_off_a_generation_and_names_the_one_it_acts_on() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    let off_a_generation = transient(&mut app, 'a');
    assert_eq!(off_a_generation.len(), 1, "{off_a_generation:#?}");
    assert_eq!(
        off_a_generation[0].heading, "Generation",
        "no number to name - there is nothing under the cursor"
    );
    assert!(
        off_a_generation[0].rows.iter().all(|row| row.dimmed),
        "{off_a_generation:#?}"
    );
    assert!(
        off_a_generation[0]
            .rows
            .iter()
            .all(|row| row.note.as_deref() == Some("no generation under the cursor")),
        "{off_a_generation:#?}"
    );
    app.handle_key(Key::new(KeyCode::Esc));

    on_generation(&mut app, 41);
    let on_a_generation = transient(&mut app, 'a');
    let generation = &on_a_generation[0];

    assert_eq!(
        generation.heading, "Generation 41",
        "and it names the one it acts on"
    );
    assert!(
        generation.rows.iter().all(|row| !row.dimmed),
        "whose rows act: {generation:?}"
    );
}

/// `e`'s own dimming reverts to the Systemd buffer's rule now that it
/// means the same thing in both buffers again: the unit popup, and only
/// the unit popup. `App::open_nix_family` is the Nix buffer's own doorway,
/// on its own five letters - `e` has nothing left to do off a unit row
/// here, unlike while it doubled as the mega-popup key.
#[test]
fn e_is_dimmed_in_the_nix_buffer_off_a_unit_row_same_as_the_systemd_buffer() {
    let ops: Ops = Default::default();
    let mut app = nixos_running(
        two_generations(&ops),
        vec![nix_unit("nix-daemon.service")],
        Default::default(),
    );

    assert!(key(&app, "e").dimmed, "the Store header has no unit");

    on_generation(&mut app, 41);
    assert!(key(&app, "e").dimmed, "nor does a generation row have one");

    on_unit(&mut app, "nix-daemon.service");
    assert!(
        !key(&app, "e").dimmed,
        "but the Nix buffer's own units section does"
    );
}

/// A generation link that could not be resolved carries `None`, and
/// `None` here means "masys could not find out", never "no path". Handing
/// `nix store diff-closures` one argument would fail; papering the
/// unknown over with a default would be worse, because two unresolved
/// generations would compare equal and the diff would report no change.
#[test]
fn a_generation_whose_link_dangles_is_never_diffed() {
    let ops: Ops = Default::default();
    let dangling = Generation {
        store_path: None,
        ..generation(41, false)
    };
    let declarative = FakeDeclarative {
        profiles: vec![system_profile(vec![dangling, generation(42, true)])],
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);
    on_generation(&mut app, 41);

    assert_eq!(transient_key(&mut app, 'a', 'd'), Flow::Continue);
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(
        transient_row(&mut app, 'a', 'd').dimmed,
        "and the key is marked, not silently inert"
    );
}

/// The other half of the same rule, and the half that is easy to miss:
/// the row under the cursor resolves perfectly well, and it is *current*
/// that could not be read. "Against current" has no meaning then.
#[test]
fn a_current_generation_that_could_not_be_resolved_is_not_diffed_against() {
    let ops: Ops = Default::default();
    let current = Generation {
        store_path: None,
        ..generation(42, true)
    };
    let declarative = FakeDeclarative {
        profiles: vec![system_profile(vec![generation(41, false), current])],
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);
    on_generation(&mut app, 41);

    assert_eq!(transient_key(&mut app, 'a', 'd'), Flow::Continue);
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
        profiles: vec![system_profile(vec![
            generation(41, false),
            generation(42, false),
        ])],
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);
    on_generation(&mut app, 41);

    assert_eq!(transient_key(&mut app, 'a', 'd'), Flow::Continue);
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(transient_row(&mut app, 'a', 'd').dimmed);
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

    assert_eq!(
        transient_key(&mut app, 'a', 'a'),
        Flow::Continue,
        "the terminal is not wanted until it is answered"
    );
    let asked = prompt(&app);
    assert!(asked.contains("generation 41"), "{asked}");
    assert!(asked.contains("boot default"), "{asked}");
    assert!(ops.borrow().is_empty(), "nothing has run yet");

    assert_eq!(
        app.handle_key(Key::char('y')),
        Flow::Suspend,
        "the session asks for the terminal"
    );
    app.run_suspended();

    assert_eq!(
        ops.borrow().as_slice(),
        [NixOp::Activate {
            profile: "/nix/var/nix/profiles/system".to_string(),
            generation: 41
        }],
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
    transient_key(&mut app, 'a', 'a');

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
    let declarative = FakeDeclarative {
        profiles: vec![system_profile(vec![generation(42, true)]), home],
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);
    on_generation(&mut app, 46);

    assert_eq!(transient_key(&mut app, 'a', 'a'), Flow::Continue);
    assert!(app.view().modal.is_none(), "nothing is even asked");
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(
        transient_row(&mut app, 'a', 'a').dimmed,
        "and the footer marks it rather than offering it"
    );
}

/// The whole reason `Profile::writable` exists, and the sharpest edge on
/// this buffer.
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
    let root_owned = Profile {
        writable: Some(false),
        ..system_profile(vec![generation(42, true)])
    };
    let declarative = FakeDeclarative {
        profiles: vec![root_owned, home],
        gc_retention: Some("14d".to_string()),
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);

    assert_eq!(transient_key(&mut app, 'c', 'c'), Flow::Continue);
    assert!(app.view().modal.is_none(), "nothing is even asked");
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(
        transient_row(&mut app, 'c', 'c').dimmed,
        "and the footer marks it rather than offering it"
    );
}

/// A profile whose writability could not be determined is not a profile
/// masys may delete from. `None` here is "I could not find out", and the
/// direction that costs a keypress is the right one against the
/// direction that costs generations.
#[test]
fn a_clean_is_never_offered_on_an_unmeasured_profile() {
    let ops: Ops = Default::default();
    let unknown = Profile {
        writable: None,
        ..system_profile(vec![generation(42, true)])
    };
    let declarative = FakeDeclarative {
        profiles: vec![unknown],
        gc_retention: Some("14d".to_string()),
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);

    assert_eq!(transient_key(&mut app, 'c', 'c'), Flow::Continue);
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(transient_row(&mut app, 'c', 'c').dimmed);
}

/// `all` over an empty list is vacuously true, which would offer the key
/// on a host masys found no profile to check - the fabricated-zero shape
/// one level up, where the fabrication is permission rather than a
/// reading.
#[test]
fn a_clean_is_never_offered_where_no_profile_was_found_to_check() {
    let ops: Ops = Default::default();
    let declarative = FakeDeclarative {
        profiles: Vec::new(),
        gc_retention: Some("14d".to_string()),
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);

    assert_eq!(transient_key(&mut app, 'c', 'c'), Flow::Continue);
    // The confirmation, not the run: pressing `c` never runs anything on
    // its own, so asserting only that nothing reached the port would pass
    // against the bug this is named for. Asserted for real when the guard
    // was removed and this test stayed green.
    assert!(app.view().modal.is_none(), "nothing is even asked");
    assert!(transient_row(&mut app, 'c', 'c').dimmed);
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
    let root_owned = Profile {
        writable: Some(false),
        ..system_profile(vec![generation(41, false), generation(42, true)])
    };
    let declarative = FakeDeclarative {
        profiles: vec![root_owned],
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);
    on_generation(&mut app, 41);

    assert_eq!(transient_key(&mut app, 'a', 'a'), Flow::Continue);
    assert!(app.view().modal.is_none(), "nothing is even asked");
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(transient_row(&mut app, 'a', 'a').dimmed);
}

/// And `d` stays live throughout. `nix store diff-closures` reads two
/// store paths and writes nothing, so a read-only profile directory is
/// no reason to withhold it - a privilege check that dimmed the whole
/// buffer would be worse than the problem it solves.
#[test]
fn a_diff_is_offered_whatever_the_profile_permissions_say() {
    let ops: Ops = Default::default();
    let root_owned = Profile {
        writable: Some(false),
        ..system_profile(vec![generation(41, false), generation(42, true)])
    };
    let declarative = FakeDeclarative {
        profiles: vec![root_owned],
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);
    on_generation(&mut app, 41);

    assert!(!transient_row(&mut app, 'a', 'd').dimmed);
    assert_eq!(transient_key(&mut app, 'a', 'd'), Flow::Suspend);
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
        profiles: vec![system_profile(vec![
            generation(41, false),
            generation(42, true),
        ])],
        gc_retention: Some("14d".to_string()),
        ops: ops.clone(),
        ..Default::default()
    };
    let mut app = nixos(declarative);

    assert_eq!(transient_key(&mut app, 'c', 'c'), Flow::Continue);
    let asked = prompt(&app);
    assert!(asked.contains("older than 14d"), "{asked}");
    assert!(asked.contains("every profile"), "{asked}");

    assert_eq!(app.handle_key(Key::char('y')), Flow::Suspend);
    app.run_suspended();

    assert_eq!(
        ops.borrow().as_slice(),
        [NixOp::Clean {
            older_than: "14d".to_string()
        }]
    );
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

    assert_eq!(transient_key(&mut app, 'c', 'c'), Flow::Continue);
    assert!(app.view().modal.is_none(), "nothing to ask about");
    app.run_suspended();

    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(transient_row(&mut app, 'c', 'c').dimmed);
}

/// The collision, pinned from the unit side. `r` is `unit_restart`, it
/// resolves in this buffer because this buffer shows units, and on a unit row
/// it still restarts - the Nix buffer's own operations were given their own
/// letters rather than a second meaning for this one.
#[test]
fn r_still_restarts_the_unit_under_the_cursor_in_the_nix_buffer() {
    let ops: Ops = Default::default();
    let calls: Calls = Default::default();
    let mut app = nixos_running(
        two_generations(&ops),
        vec![nix_unit("nix-daemon.service")],
        calls.clone(),
    );
    on_unit(&mut app, "nix-daemon.service");

    app.handle_key(Key::char('r'));
    assert!(
        prompt(&app).contains("restart nix-daemon.service"),
        "{}",
        prompt(&app)
    );
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
    let mut app = nixos_running(
        two_generations(&ops),
        vec![nix_unit("nix-daemon.service")],
        calls.clone(),
    );
    on_generation(&mut app, 41);

    assert_eq!(app.handle_key(Key::char('r')), Flow::Continue);

    assert!(
        app.view().modal.is_none(),
        "no confirmation, because there is nothing to confirm"
    );
    assert!(calls.borrow().is_empty(), "{:?}", calls.borrow());
    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
    assert!(
        key(&app, "r").dimmed,
        "the footer must not offer restart against a generation"
    );
    assert!(
        !transient_row(&mut app, 'a', 'a').dimmed,
        "while the key that does act here is lit"
    );
}

// --- Phase 3: the Nix transient ---

/// A host with a flake reference and a system profile, so the transient
/// has something to be contextual about.
fn flake_host(ops: &Ops) -> FakeDeclarative {
    FakeDeclarative {
        profiles: vec![system_profile(vec![
            generation(41, false),
            generation(42, true),
        ])],
        gc_retention: Some("14d".to_string()),
        flake_ref: Some("/home/user/.dotfiles".to_string()),
        inputs: Some(flake_inputs()),
        ops: ops.clone(),
        ..Default::default()
    }
}

fn flake_inputs() -> masys_domain::declarative::Inputs {
    masys_domain::declarative::Inputs {
        source: masys_domain::declarative::InputSource::Flake {
            lock_path: "/home/user/.dotfiles/flake.lock".to_string(),
        },
        inputs: Vec::new(),
    }
}

fn channels_inputs() -> masys_domain::declarative::Inputs {
    masys_domain::declarative::Inputs {
        source: masys_domain::declarative::InputSource::Channels,
        inputs: Vec::new(),
    }
}

/// Opens a Nix family's popup and hands back its groups.
fn transient(app: &mut App, family: char) -> Vec<masys_view::ActionGroup> {
    app.handle_key(Key::char(family));
    let Some(ModalView::Transient { groups, .. }) = app.view().modal else {
        panic!("no `{family}` transient")
    };
    groups
}

fn find(groups: &[masys_view::ActionGroup], chord: char) -> masys_view::ActionRow {
    groups
        .iter()
        .flat_map(|group| &group.rows)
        .find(|row| row.chord == chord)
        .unwrap_or_else(|| panic!("no `{chord}` row"))
        .clone()
}

/// Five family letters open the whole `[C2]` operation set between them,
/// which is what the family menus exist for: seventeen operations reachable
/// without spending seventeen top-level letters (`switch` and
/// home-manager `switch` are the two that are direct; everything else
/// sorts into one of these five).
#[test]
fn the_nix_transient_lists_the_designs_groups() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    on_generation(&mut app, 41);

    let rebuild = transient(&mut app, 'b');
    assert_eq!(
        rebuild
            .iter()
            .map(|group| group.heading.as_str())
            .collect::<Vec<_>>(),
        vec!["Rebuild"]
    );
    for chord in ['s', 'b', 't', 'B', 'y'] {
        find(&rebuild, chord);
    }
    app.handle_key(Key::new(KeyCode::Esc));

    let generation = transient(&mut app, 'a');
    assert_eq!(
        generation
            .iter()
            .map(|group| group.heading.as_str())
            .collect::<Vec<_>>(),
        vec!["Generation 41"]
    );
    for chord in ['a', 'd', 'x'] {
        find(&generation, chord);
    }
    app.handle_key(Key::new(KeyCode::Esc));

    let inputs = transient(&mut app, 'i');
    assert_eq!(
        inputs
            .iter()
            .map(|group| group.heading.as_str())
            .collect::<Vec<_>>(),
        vec!["Inputs"]
    );
    for chord in ['f', 'F', 'n', 'N'] {
        find(&inputs, chord);
    }
    app.handle_key(Key::new(KeyCode::Esc));

    let store = transient(&mut app, 'c');
    assert_eq!(
        store
            .iter()
            .map(|group| group.heading.as_str())
            .collect::<Vec<_>>(),
        vec!["Store"]
    );
    for chord in ['c', 'D'] {
        find(&store, chord);
    }
    app.handle_key(Key::new(KeyCode::Esc));

    let search = transient(&mut app, 'f');
    assert_eq!(
        search
            .iter()
            .map(|group| group.heading.as_str())
            .collect::<Vec<_>>(),
        vec!["Search"]
    );
    for chord in ['p', 'v'] {
        find(&search, chord);
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
    let rebuild = transient(&mut app, 'b');

    for chord in ['s', 'b', 't', 'B', 'y'] {
        assert!(
            !find(&rebuild, chord).dimmed,
            "`{chord}` is a rebuild verb and runs bare"
        );
    }
    app.handle_key(Key::new(KeyCode::Esc));

    let inputs = transient(&mut app, 'i');
    for chord in ['f', 'F'] {
        let row = find(&inputs, chord);
        assert!(row.dimmed, "`{chord}` is flake-only");
        assert_eq!(
            row.note.as_deref(),
            Some("no flake configured"),
            "and says why"
        );
    }
    for chord in ['n', 'N'] {
        assert!(
            !find(&inputs, chord).dimmed,
            "`{chord}` is what this host actually has"
        );
    }
}

/// And the mirror. The channel operations dim on a flake host by the same
/// rule that dims the flake ones on a channels host.
#[test]
fn the_channel_operations_dim_on_a_flake_host() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    let inputs = transient(&mut app, 'i');

    for chord in ['f', 'F'] {
        assert!(
            !find(&inputs, chord).dimmed,
            "`{chord}` is what this host has"
        );
    }
    for chord in ['n', 'N'] {
        let row = find(&inputs, chord);
        assert!(
            row.dimmed,
            "`{chord}` acts on channels this host does not read"
        );
        assert_eq!(row.note.as_deref(), Some("no channels on this host"));
    }
}

/// `-r` reroutes `switch` to `rollback` before the confirmation is even
/// built - `nixos-rebuild switch --rollback` *is* `NixOp::Rollback`, and
/// the domain's own doc on that variant says so. The prompt names what
/// will actually run, not the row that was pressed - proof `nix_rollback`
/// is reachable even though it is no longer a row anywhere (see
/// `every_named_action_is_reachable` in `keymap.rs`).
#[test]
fn rollback_is_reached_through_the_rebuild_switch() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    app.handle_key(Key::char('b'));
    app.handle_key(Key::char('r')); // arms the `-r` switch
    assert_eq!(app.handle_key(Key::char('s')), Flow::Continue, "asks first");
    assert!(
        prompt(&app).contains("activate the previous generation"),
        "{}",
        prompt(&app)
    );

    assert_eq!(app.handle_key(Key::char('y')), Flow::Suspend);
    app.run_suspended();

    assert_eq!(ops.borrow().as_slice(), [NixOp::Rollback]);
}

/// Unarmed, the same two keypresses run the row's own verb - the switch
/// changes what `s` means, not what any other row means, and toggling it
/// off again is symmetric with arming it.
#[test]
fn switch_runs_switch_when_the_rollback_switch_is_off() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    app.handle_key(Key::char('b'));
    assert_eq!(app.handle_key(Key::char('s')), Flow::Continue, "asks first");
    assert!(prompt(&app).contains("boot default"), "{}", prompt(&app));

    assert_eq!(app.handle_key(Key::char('y')), Flow::Suspend);
    app.run_suspended();

    assert_eq!(
        ops.borrow().as_slice(),
        [NixOp::Rebuild(RebuildVerb::Switch)]
    );
}

/// #17: a switch that restarts the wrong unit is what turned a routine
/// `nixos-rebuild switch` into a suspended, unclean-shutdown host - see the
/// issue for the incident. dry-activate already answers "which units would
/// restart" before the switch runs; the confirmation now names it as the row
/// to run first. It names the row rather than the key: `y` right here
/// confirms `switch` (see `switch_runs_switch_when_the_rollback_switch_is_off`)
/// and is not a way to reach dry-activate from this prompt.
#[test]
fn the_switch_confirmation_names_dry_activate_as_the_way_to_preview_restarts() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    app.handle_key(Key::char('b'));
    app.handle_key(Key::char('s'));

    assert!(prompt(&app).contains("dry-activate"), "{}", prompt(&app));
    assert!(prompt(&app).contains("restart"), "{}", prompt(&app));
}

/// The other half of #17: the row's own note said "print what activating
/// would change", which is true and answers nothing - every rebuild verb
/// changes something. Naming the units is what makes the row worth pressing
/// before `s`.
///
/// `is_root: true` isolates this from #14's polkit caveat below, which
/// would otherwise append to every rebuild row's note on the same
/// unprivileged-by-default fake.
#[test]
fn the_dry_activate_row_names_what_it_previews() {
    let mut app = nixos(FakeDeclarative {
        is_root: true,
        ..two_generations(&Default::default())
    });

    let row = transient_row(&mut app, 'b', 'y');
    assert_eq!(
        row.note.as_deref(),
        Some("print which units activating would restart")
    );
}

/// The row's note (above) is the popup's promise; this is the confirmation
/// the operator actually answers once they press it. `nix_buffer.rs`
/// documents the two as one description read from two places - a reworded
/// row behind a stale confirmation would keep the vague wording exactly
/// where committing to the act happens, which is the one place it matters.
#[test]
fn the_dry_activate_confirmation_names_the_same_units_as_the_row() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    assert_eq!(
        transient_key(&mut app, 'b', 'y'),
        Flow::Continue,
        "asks first"
    );
    assert!(prompt(&app).contains("which units"), "{}", prompt(&app));
    assert!(prompt(&app).contains("restart"), "{}", prompt(&app));
}

/// #14: masys offers `--elevate run0` for every rebuild verb but `build`,
/// measured from whether the mechanism exists
/// (`DeclarativeService::can_elevate`) - never from whether *this
/// operator* is in the polkit rules, which is not knowable without
/// trying. Chosen over measuring the policy or spending the build
/// silently: say so, on every row that could ask, wherever masys is not
/// already root.
#[test]
fn the_activating_rebuild_rows_note_that_polkit_might_refuse_where_masys_is_not_root() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    for chord in ['s', 'b', 't', 'y'] {
        let row = transient_row(&mut app, 'b', chord);
        assert!(
            row.note
                .as_deref()
                .is_some_and(|note| note.contains("activation needs polkit or root")),
            "{chord}: {:?}",
            row.note
        );
    }
}

/// `build` stops at a store path and never elevates - `ops::elevation`'s
/// own comment calls it "the one verb that stops at a store path" - so it
/// has nothing to warn about and its note stays exactly its label.
#[test]
fn build_never_notes_polkit_because_it_never_elevates() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    let row = transient_row(&mut app, 'b', 'B');
    assert_eq!(row.note.as_deref(), Some("build only"));
}

/// A root masys already has the privilege every one of these rows would
/// otherwise ask for, so the caveat above would be true of nothing -
/// costing a root masys the note is exactly the failure mode #14 rejected
/// measuring the policy to avoid.
#[test]
fn a_root_masys_gets_no_polkit_caveat_on_the_rebuild_rows() {
    let ops: Ops = Default::default();
    let mut app = nixos(FakeDeclarative {
        is_root: true,
        ..two_generations(&ops)
    });

    let row = transient_row(&mut app, 'b', 's');
    assert_eq!(
        row.note.as_deref(),
        Some("build, activate, and make it the boot default")
    );
}

/// `home switch` in `Module` is not merely ineffective - there is no
/// `home-manager` binary to run, because home-manager was generated into
/// the system closure. `h` is direct now (the one true one-shot leaf, no
/// popup of its own), so the reason lives only in `NixBuffer::note` - not
/// on screen anywhere, the same trade the footer already makes for a
/// dimmed unit verb like `D` or `M`. What the footer *does* show is
/// asked of the same `offer` the popups ask, so it cannot disagree with
/// what pressing `h` would do.
#[test]
fn home_switch_is_marked_where_nixos_rebuild_activates_it() {
    let ops: Ops = Default::default();
    let module = FakeDeclarative {
        home_mode: Some(masys_domain::declarative::HomeMode::Module),
        ..flake_host(&ops)
    };
    let app = nixos(module);

    assert!(key(&app, "h").dimmed);
}

/// And in standalone mode it is the one host where it acts.
#[test]
fn home_switch_acts_in_standalone_mode() {
    let ops: Ops = Default::default();
    let standalone = FakeDeclarative {
        home_mode: Some(masys_domain::declarative::HomeMode::Standalone),
        ..flake_host(&ops)
    };
    let app = nixos(standalone);

    assert!(!key(&app, "h").dimmed);
}

/// `clean` carries the period it will use. "delete old generations" with
/// no number is the row an operator should not press.
#[test]
fn clean_says_the_retention_it_will_delete_by() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));

    assert_eq!(
        find(&transient(&mut app, 'c'), 'c').note.as_deref(),
        Some("--delete-older-than 14d")
    );
}

/// A host that declares no retention and a host whose retention could not
/// be read are different facts, and the row says which. Both mark it -
/// neither is a period to delete generations by - but only one of them is
/// the host's own answer.
#[test]
fn an_unread_retention_and_an_undeclared_one_read_differently() {
    let ops: Ops = Default::default();

    let declared_none = FakeDeclarative {
        gc_retention: None,
        ..flake_host(&ops)
    };
    let mut app = nixos(declared_none);
    let row = find(&transient(&mut app, 'c'), 'c');
    assert!(row.dimmed);
    assert_eq!(
        row.note.as_deref(),
        Some("no retention declared"),
        "the port answered, and its answer was none"
    );

    let unread = FakeDeclarative {
        gc_retention: None,
        failing: std::rc::Rc::new(std::cell::Cell::new(true)),
        ..flake_host(&ops)
    };
    let mut app = nixos(unread);
    let row = find(&transient(&mut app, 'c'), 'c');
    assert!(row.dimmed);
    assert_eq!(
        row.note.as_deref(),
        Some("retention unread"),
        "no read has come back at all"
    );
}

/// `search packages` has no source but the operator: a query is not a row
/// and not a reading. Pressing it opens the prompt rather than running
/// anything, which is what `NixOffer::Asks` exists to say.
#[test]
fn a_verb_that_needs_a_value_opens_the_prompt_instead_of_running() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    let groups = transient(&mut app, 'f');
    assert!(
        !find(&groups, 'p').dimmed,
        "the row is live - what is missing is the value, not the host"
    );

    app.handle_key(Key::char('p'));
    let Some(ModalView::Input { prompt, typed, .. }) = app.view().modal else {
        panic!("no prompt")
    };
    assert_eq!(prompt, "search nixpkgs");
    assert_eq!(typed, "");
    assert!(
        ops.borrow().is_empty(),
        "and nothing ran: {:?}",
        ops.borrow()
    );
}

/// Typing, then enter, builds the operation from what was typed and
/// suspends into it - a search only reads, so it asks nothing first.
#[test]
fn a_typed_query_reaches_the_port_as_the_operation_it_names() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    transient(&mut app, 'f');
    app.handle_key(Key::char('p'));
    for c in "ripgrep".chars() {
        app.handle_key(Key::char(c));
    }

    let Some(ModalView::Input { typed, .. }) = app.view().modal else {
        panic!("no prompt")
    };
    assert_eq!(typed, "ripgrep");

    assert_eq!(
        app.handle_key(Key::new(KeyCode::Enter)),
        Flow::Suspend,
        "a search takes the terminal"
    );
    app.run_suspended();
    assert_eq!(
        *ops.borrow(),
        vec![NixOp::SearchPackages {
            query: "ripgrep".to_string()
        }]
    );
}

/// Backspace deletes, and escape backs out without running.
#[test]
fn the_prompt_edits_and_backs_out() {
    let ops: Ops = Default::default();
    // A host `nixos-option` works on, since that is the prompt being
    // typed into - on this development host the row is marked, for the
    // reason `option_value_is_marked_where_no_nixos_config_resolves`
    // records.
    let mut app = nixos(FakeDeclarative {
        nixos_config: Some("/etc/nixos/configuration.nix".to_string()),
        ..flake_host(&ops)
    });
    transient(&mut app, 'f');
    app.handle_key(Key::char('v'));
    for c in "boot.loaderX".chars() {
        app.handle_key(Key::char(c));
    }
    app.handle_key(Key::new(KeyCode::Backspace));

    let Some(ModalView::Input { prompt, typed, .. }) = app.view().modal else {
        panic!("no prompt")
    };
    assert_eq!(prompt, "option");
    assert_eq!(typed, "boot.loader");

    app.handle_key(Key::new(KeyCode::Esc));
    assert!(app.view().modal.is_none(), "the prompt is gone");
    assert!(
        ops.borrow().is_empty(),
        "and nothing ran: {:?}",
        ops.borrow()
    );
}

/// Enter on an empty prompt does nothing. `nix search nixpkgs ""` matches
/// every package in nixpkgs and `nix-env --delete-generations ""` is an
/// argument error; neither is what pressing enter on an empty prompt
/// meant, and neither is worth finding out by running it.
#[test]
fn an_empty_prompt_does_not_commit() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    transient(&mut app, 'f');
    app.handle_key(Key::char('p'));

    assert_eq!(app.handle_key(Key::new(KeyCode::Enter)), Flow::Continue);
    assert!(
        matches!(app.view().modal, Some(ModalView::Input { .. })),
        "the prompt is still waiting"
    );
    assert!(ops.borrow().is_empty(), "{:?}", ops.borrow());
}

/// A typed spec still goes through the confirmation, because deleting
/// generations changes the machine. The prompt names what it will do.
#[test]
fn a_typed_generation_spec_is_confirmed_before_it_runs() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    transient(&mut app, 'c');
    app.handle_key(Key::char('D'));
    for c in "+5".chars() {
        app.handle_key(Key::char(c));
    }
    app.handle_key(Key::new(KeyCode::Enter));

    assert!(prompt(&app).contains("delete"), "{}", prompt(&app));
    assert!(
        ops.borrow().is_empty(),
        "not before it is answered: {:?}",
        ops.borrow()
    );

    app.handle_key(Key::char('y'));
    app.run_suspended();
    assert_eq!(
        *ops.borrow(),
        vec![NixOp::DeleteGenerations {
            profile: "/nix/var/nix/profiles/system".to_string(),
            spec: "+5".to_string()
        }]
    );
}

/// The generation under the cursor supplies its own spec, so `x` asks for
/// no value - only for confirmation.
#[test]
fn deleting_the_generation_under_the_cursor_needs_no_prompt() {
    let ops: Ops = Default::default();
    let mut app = nixos(flake_host(&ops));
    on_generation(&mut app, 41);
    transient(&mut app, 'a');
    app.handle_key(Key::char('x'));

    assert!(prompt(&app).contains("delete"), "{}", prompt(&app));
    app.handle_key(Key::char('y'));
    app.run_suspended();
    assert_eq!(
        *ops.borrow(),
        vec![NixOp::DeleteGenerations {
            profile: "/nix/var/nix/profiles/system".to_string(),
            spec: "41".to_string()
        }]
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
    let row = find(&transient(&mut app, 'f'), 'v');

    assert!(
        row.dimmed,
        "the row would fail before it evaluated anything"
    );
    assert_eq!(
        row.note.as_deref(),
        Some("no nixos-config on the search path")
    );

    // And pressing it opens no prompt, because there is nothing a typed
    // option name could be read against.
    app.handle_key(Key::char('v'));
    assert!(
        app.view().modal.is_none(),
        "no prompt is opened for a row that cannot act"
    );
}

/// And a host that has one - a channels install, or a flake host that
/// sets `nix.nixPath` - reaches it. The mark is about the search path,
/// not about the paradigm.
#[test]
fn option_value_acts_where_a_nixos_config_resolves() {
    let ops: Ops = Default::default();
    let configured = FakeDeclarative {
        nixos_config: Some("/etc/nixos/configuration.nix".to_string()),
        ..flake_host(&ops)
    };
    let mut app = nixos(configured);
    let row = find(&transient(&mut app, 'f'), 'v');

    assert!(
        !row.dimmed,
        "a flake host with a nixPath is still a host nixos-option works on"
    );
    assert_eq!(row.note, None);

    app.handle_key(Key::char('v'));
    assert!(
        matches!(app.view().modal, Some(ModalView::Input { .. })),
        "and it asks for the option name"
    );
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
    // nixos-config. Seven of the sixteen rows across the five family
    // popups are marked here (`switch` and home-manager `switch` are
    // direct now and carry no row), which makes it the case worth asking.
    let unprivileged = Profile {
        kind: ProfileKind::System,
        path: "/nix/var/nix/profiles/system".to_string(),
        writable: Some(false),
        generations: vec![generation(437, false), generation(438, true)],
    };
    let declarative = FakeDeclarative {
        profiles: vec![unprivileged],
        home_mode: Some(masys_domain::declarative::HomeMode::Module),
        ..flake_host(&ops)
    };
    let mut app = nixos(declarative);
    on_generation(&mut app, 437);

    let mut rows = Vec::new();
    for family in ['b', 'a', 'i', 'c', 'f'] {
        rows.extend(
            transient(&mut app, family)
                .into_iter()
                .flat_map(|group| group.rows),
        );
        app.handle_key(Key::new(KeyCode::Esc));
    }

    let marked: Vec<&masys_view::ActionRow> = rows.iter().filter(|row| row.dimmed).collect();
    assert!(
        marked.len() >= 7,
        "this host marks most of the popups: {marked:#?}"
    );
    for row in marked {
        assert!(
            row.note.is_some(),
            "`{}` is marked and says nothing: {row:?}",
            row.label
        );
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
    let mut app = nixos(FakeDeclarative {
        profiles: vec![unprivileged],
        ..flake_host(&ops)
    });
    let row = find(&transient(&mut app, 'c'), 'c');

    assert!(row.dimmed);
    assert_eq!(
        row.note.as_deref(),
        Some("every profile must be writable - run masys as root")
    );
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
    let declarative = FakeDeclarative {
        profiles: vec![system_profile(vec![generation(438, true)]), home],
        ..flake_host(&ops)
    };
    let mut app = nixos(declarative);
    on_generation(&mut app, 46);
    let row = find(&transient(&mut app, 'a'), 'a');

    assert!(row.dimmed);
    assert_eq!(
        row.note.as_deref(),
        Some("only a system generation activates")
    );
}

/// A host that can raise privilege makes the profile-writing rows live,
/// and says they will ask.
///
/// Before this they were a dead end: marked *run masys as root*, and
/// pressing them did nothing. The only way through was to quit and start
/// the whole terminal program again as root - which grants the process
/// reading your journal and holding a keymap that can restart any unit
/// far more than the one directory `nix-env` writes.
#[test]
fn a_host_that_can_elevate_offers_the_profile_writing_rows() {
    let ops: Ops = Default::default();
    let read_only = Profile {
        kind: ProfileKind::System,
        path: "/nix/var/nix/profiles/system".to_string(),
        writable: Some(false),
        generations: vec![generation(437, false), generation(438, true)],
    };
    let mut app = nixos(FakeDeclarative {
        profiles: vec![read_only],
        can_elevate: true,
        ..flake_host(&ops)
    });
    on_generation(&mut app, 437);
    let generation = transient(&mut app, 'a');
    for chord in ['a', 'x'] {
        let row = find(&generation, chord);
        assert!(
            !row.dimmed,
            "`{}` acts on a host that can elevate: {row:?}",
            row.label
        );
        assert_eq!(
            row.note.as_deref(),
            Some("asks for root"),
            "and says so: {row:?}"
        );
    }
    app.handle_key(Key::new(KeyCode::Esc));

    let store = transient(&mut app, 'c');
    let row = find(&store, 'D');
    assert!(
        !row.dimmed,
        "`{}` acts on a host that can elevate: {row:?}",
        row.label
    );
    assert_eq!(
        row.note.as_deref(),
        Some("asks for root"),
        "and says so: {row:?}"
    );
}

/// The same host without a way to raise privilege keeps the old refusal.
///
/// The pair is the point: without it, making the rows live unconditionally
/// would pass the test above and say nothing.
#[test]
fn a_host_that_cannot_elevate_still_refuses_them() {
    let ops: Ops = Default::default();
    let read_only = Profile {
        kind: ProfileKind::System,
        path: "/nix/var/nix/profiles/system".to_string(),
        writable: Some(false),
        generations: vec![generation(437, false), generation(438, true)],
    };
    let mut app = nixos(FakeDeclarative {
        profiles: vec![read_only],
        can_elevate: false,
        ..flake_host(&ops)
    });
    on_generation(&mut app, 437);
    let generation = transient(&mut app, 'a');
    for chord in ['a', 'x'] {
        let row = find(&generation, chord);
        assert!(
            row.dimmed,
            "`{}` is refused where nothing can raise privilege: {row:?}",
            row.label
        );
        assert_eq!(
            row.note.as_deref(),
            Some("profile is read-only - run masys as root")
        );
    }
    app.handle_key(Key::new(KeyCode::Esc));

    let store = transient(&mut app, 'c');
    let row = find(&store, 'D');
    assert!(
        row.dimmed,
        "`{}` is refused where nothing can raise privilege: {row:?}",
        row.label
    );
    assert_eq!(
        row.note.as_deref(),
        Some("profile is read-only - run masys as root")
    );
}

/// `clean` keeps its retention beside the privilege note, where both
/// apply.
///
/// The period is what the key will delete by and the note is what it will
/// ask for first, and an operator wants the period most on the row that
/// is about to run as root. Where masys *cannot* elevate the privilege
/// note still stands alone, because there the key does nothing and the
/// period it would have used is not the useful half.
#[test]
fn an_elevatable_clean_says_both_what_it_asks_for_and_what_it_deletes() {
    let ops: Ops = Default::default();
    let read_only = Profile {
        kind: ProfileKind::System,
        path: "/nix/var/nix/profiles/system".to_string(),
        writable: Some(false),
        generations: vec![generation(438, true)],
    };
    let host = |can_elevate: bool| {
        let mut app = nixos(FakeDeclarative {
            profiles: vec![read_only.clone()],
            gc_retention: Some("14d".to_string()),
            can_elevate,
            ..flake_host(&ops)
        });
        let row = find(&transient(&mut app, 'c'), 'c');
        (row.dimmed, row.note.clone())
    };

    assert_eq!(
        host(true),
        (
            false,
            Some("asks for root . --delete-older-than 14d".to_string())
        )
    );
    assert_eq!(
        host(false),
        (
            true,
            Some("every profile must be writable - run masys as root".to_string())
        )
    );
}

// ---------------------------------------------------------------------
// `offer` and `op_typed`: two matches over one enum, and only one of them
// is exhaustive.
//
// `NixBuffer::offer`'s doc carries the invariant the footer, the popup and
// the dispatch all lean on - *a row that is offered is a row that will
// act*. For the three verbs that need a value nobody has read, `offer`
// keeps only half of it: it answers `Asks`, and the operation itself comes
// from `op_typed`, a second match ending in `_ => None`. A verb added to
// the first and forgotten in the second offers a live row, takes what the
// operator types, and drops it - and until these tests, nothing said so.
// ---------------------------------------------------------------------

/// Whether a verb can ever reach the input sub-step.
///
/// Written out rather than derived, and exhaustive deliberately: a new
/// `NixVerb` variant stops this file compiling until somebody classifies
/// it. That is the whole point - `op_typed`'s own `_ => None` cannot fail
/// that way, because a verb it has never heard of reads exactly like one
/// that needs no value.
fn can_ask(verb: NixVerb) -> bool {
    match verb {
        NixVerb::SearchPackages
        | NixVerb::OptionValue
        | NixVerb::DeleteGenerations
        // The variant is free text, because the candidate names come from
        // `ListImageVariants` on the row above rather than from a picker.
        | NixVerb::BuildImage => true,
        // `build-image` with no variant is the row that *prints* the
        // candidates, so it asks for nothing itself.
        NixVerb::BuildVm
        | NixVerb::ListImageVariants
        | NixVerb::Rebuild(_)
        | NixVerb::Rollback
        | NixVerb::HomeSwitch
        | NixVerb::DeleteHere
        | NixVerb::FlakeUpdate
        | NixVerb::FlakeCheck
        | NixVerb::ChannelUpdate
        | NixVerb::ChannelRollback
        | NixVerb::Upgrade
        | NixVerb::Repl
        | NixVerb::Activate
        | NixVerb::Diff
        | NixVerb::Clean => false,
    }
}

/// Every Nix verb a key can deliver.
///
/// Read out of `ACTIONS` rather than listed again here: a verb with no
/// catalogue entry has no name, so no binding resolves to it and no popup
/// row is built from it.
fn bound_verbs() -> Vec<NixVerb> {
    ACTIONS
        .iter()
        .filter_map(|(_, action)| match action {
            Action::Nix(verb) => Some(*verb),
            _ => None,
        })
        .collect()
}

/// A host that answers every read, so no verb is marked for want of a
/// fact. The asking verbs are live here and nowhere more so.
fn answering_host() -> NixBuffer {
    NixBuffer {
        profiles: Some(vec![system_profile(vec![generation(42, true)])]),
        nixos_config: Some(Some("/etc/nixos/configuration.nix".to_string())),
        flake_ref: Some(Some("/home/user/.dotfiles".to_string())),
        gc_retention: Some(Some("14d".to_string())),
        home_mode: Some(masys_domain::declarative::HomeMode::Standalone),
        can_elevate: true,
        ..Default::default()
    }
}

/// A verb that asks for a value can always build the operation it asked
/// for.
///
/// The direction that matters: `offer` said the row is live and the input
/// sub-step will open, so something has to come back when the operator
/// answers it.
#[test]
fn every_verb_that_asks_for_a_value_can_build_its_operation() {
    let nix = answering_host();
    for verb in bound_verbs().into_iter().filter(|verb| can_ask(*verb)) {
        assert!(
            nix.op_typed(verb, "anything").is_some(),
            "{verb:?} opens the input sub-step, but `op_typed` falls through to `_ => None` \
             and the operator's answer is dropped"
        );
    }
}

/// And a verb that asks for nothing builds nothing from a typed value.
///
/// The other direction, which keeps `_ => None` honest: a verb whose
/// arguments were resolved at offer time must not quietly accept a second,
/// typed set.
#[test]
fn a_verb_that_asks_for_nothing_builds_no_typed_operation() {
    let nix = answering_host();
    for verb in bound_verbs().into_iter().filter(|verb| !can_ask(*verb)) {
        assert!(
            nix.op_typed(verb, "anything").is_none(),
            "{verb:?} resolves its arguments at offer time, but `op_typed` built an operation \
             out of a typed value anyway"
        );
    }
}

/// `offer` asks only where the verb above says it can.
///
/// Pins the classification against the function it describes, so `can_ask`
/// cannot drift into a list of what somebody remembered.
#[test]
fn offer_opens_the_input_sub_step_only_where_the_verb_asks() {
    for (host, nix) in [
        ("a host that has answered nothing", NixBuffer::default()),
        ("a host that answers everything", answering_host()),
    ] {
        for verb in bound_verbs() {
            let asks = matches!(nix.offer(None, verb), NixOffer::Asks { .. });
            assert!(
                !asks || can_ask(verb),
                "on {host}, {verb:?} offers the input sub-step but is classified as asking \
                 for nothing"
            );
        }
    }
}

/// `-u` reroutes `switch` to `upgrade` the way `-r` reroutes it to
/// `rollback` - `nixos-rebuild switch --upgrade` *is* `NixOp::Upgrade`.
/// Proof `nix_upgrade` is reachable even though it is not a row anywhere
/// (see `every_named_action_is_reachable` in `keymap.rs`).
#[test]
fn upgrade_is_reached_through_the_rebuild_switch() {
    let ops: Ops = Default::default();
    // A channels host, because that is the only kind `--upgrade` runs on.
    let mut app = nixos(FakeDeclarative {
        inputs: Some(channels_inputs()),
        ..two_generations(&ops)
    });

    app.handle_key(Key::char('b'));
    app.handle_key(Key::char('u')); // arms the `-u` switch
    assert_eq!(app.handle_key(Key::char('s')), Flow::Continue, "asks first");
    assert!(
        prompt(&app).contains("update the nixos channel"),
        "{}",
        prompt(&app)
    );

    assert_eq!(app.handle_key(Key::char('y')), Flow::Suspend);
    app.run_suspended();

    assert_eq!(ops.borrow().as_slice(), [NixOp::Upgrade]);
}

/// Both switches armed is a contradiction - `--rollback` builds nothing,
/// so channels updated on the way to it would go unread - and rollback
/// wins because it is the one that changes least: no channel moves and
/// the generation it activates already exists.
#[test]
fn rollback_wins_when_both_switches_are_armed() {
    let ops: Ops = Default::default();
    let mut app = nixos(FakeDeclarative {
        inputs: Some(channels_inputs()),
        ..two_generations(&ops)
    });

    app.handle_key(Key::char('b'));
    app.handle_key(Key::char('u'));
    app.handle_key(Key::char('r'));
    app.handle_key(Key::char('s'));
    assert!(
        prompt(&app).contains("activate the previous generation"),
        "{}",
        prompt(&app)
    );

    assert_eq!(app.handle_key(Key::char('y')), Flow::Suspend);
    app.run_suspended();

    assert_eq!(ops.borrow().as_slice(), [NixOp::Rollback]);
}

/// The reason `--upgrade` is its own variant rather than a flag on
/// `Rebuild`: it updates *channels*, and a flake host has none. So it is
/// gated with `ChannelUpdate` and not with the rebuild verbs, which stay
/// live everywhere - arming `-u` on a flake host must not produce a
/// command asking `nixos-rebuild` to refresh inputs the configuration
/// does not read.
#[test]
fn upgrade_does_not_run_on_a_flake_host_where_a_bare_switch_does() {
    let ops: Ops = Default::default();
    let mut app = nixos(FakeDeclarative {
        inputs: Some(masys_domain::declarative::Inputs {
            source: masys_domain::declarative::InputSource::Flake {
                lock_path: "/home/user/.dotfiles/flake.lock".to_string(),
            },
            ..channels_inputs()
        }),
        ..two_generations(&ops)
    });

    app.handle_key(Key::char('b'));
    app.handle_key(Key::char('u'));
    app.handle_key(Key::char('s'));
    app.handle_key(Key::char('y'));
    app.run_suspended();
    assert!(
        !ops.borrow().contains(&NixOp::Upgrade),
        "upgrade ran on a flake host: {:?}",
        ops.borrow()
    );
}

/// `p` on the Rebuild popup opens a repl, and does it without asking.
///
/// No confirmation because there is nothing to confirm: a repl evaluates
/// and changes nothing, which is what puts it in `only_reads` beside
/// `diff` and `search`. So the keypress suspends directly.
#[test]
fn p_on_the_rebuild_popup_opens_a_repl_without_asking() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    app.handle_key(Key::char('b'));
    assert_eq!(
        app.handle_key(Key::char('p')),
        Flow::Suspend,
        "a repl needs no confirmation"
    );
    assert_eq!(
        app.run_suspended(),
        Aftermath::Seen,
        "a repl holds the terminal until the operator quits it, so by the \
         time it returns its screen has been read - masys must not charge \
         a keypress to dismiss a screen that is already gone"
    );

    assert_eq!(ops.borrow().as_slice(), [NixOp::Repl]);
}

/// And every other Nix operation does not hold it.
///
/// The other half of `holds_the_terminal`, which is worth having because
/// the predicate is only interesting as a *distinction*: a version of it
/// that answered `true` for everything would pass the test above. These
/// stream and exit, so their output is still on screen with nobody having
/// read it, and masys pauses.
#[test]
fn an_operation_that_streams_and_exits_leaves_its_output_unread() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    app.handle_key(Key::char('b'));
    app.handle_key(Key::char('s'));
    app.handle_key(Key::char('y'));
    assert_eq!(
        app.run_suspended(),
        Aftermath::Unseen,
        "a rebuild prints and exits, so its output is unread"
    );
}

/// `r` still toggles the rollback switch rather than running the repl.
///
/// The reason `repl` took `p`: `answer_transient` reads one character and
/// asks `action` before `toggle`, so a row bound to `r` would shadow the
/// `-r` switch entirely and make rollback unreachable.
#[test]
fn r_on_the_rebuild_popup_still_arms_the_rollback_switch() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    app.handle_key(Key::char('b'));
    app.handle_key(Key::char('r'));
    app.handle_key(Key::char('s'));
    assert!(
        prompt(&app).contains("activate the previous generation"),
        "{}",
        prompt(&app)
    );
    app.handle_key(Key::char('y'));
    app.run_suspended();
    assert_eq!(ops.borrow().as_slice(), [NixOp::Rollback]);
}

/// `v` on the Rebuild popup builds a VM runner, after confirming.
///
/// Confirmed for `build`'s reason rather than `repl`'s: it writes to the
/// store, leaves a `./result`, and takes minutes.
#[test]
fn v_on_the_rebuild_popup_builds_a_vm_runner() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    app.handle_key(Key::char('b'));
    app.handle_key(Key::char('v'));
    assert!(
        prompt(&app).contains("qemu runner"),
        "it asks first: {}",
        prompt(&app)
    );
    app.handle_key(Key::char('y'));
    app.run_suspended();

    assert_eq!(ops.borrow().as_slice(), [NixOp::BuildVm]);
}

/// **`i` lists the image variants and asks nothing; `I` asks for one.**
///
/// The pair that makes `build-image` reachable at all. Listing is a read
/// (it prints the candidates and changes nothing), so it runs straight
/// off the keypress. Building one is a build, and the variant is typed,
/// because the names come from the row above rather than from a picker.
#[test]
fn i_lists_image_variants_and_shift_i_builds_one() {
    let ops: Ops = Default::default();
    let mut app = nixos(two_generations(&ops));

    app.handle_key(Key::char('b'));
    assert_eq!(
        app.handle_key(Key::char('i')),
        Flow::Suspend,
        "listing the variants needs no confirmation"
    );
    app.run_suspended();
    assert_eq!(ops.borrow().as_slice(), [NixOp::ListImageVariants]);

    ops.borrow_mut().clear();
    app.handle_key(Key::char('b'));
    app.handle_key(Key::char('I'));
    for c in "proxmox".chars() {
        app.handle_key(Key::char(c));
    }
    app.handle_key(Key::new(KeyCode::Enter));
    app.handle_key(Key::char('y'));
    app.run_suspended();

    assert_eq!(
        ops.borrow().as_slice(),
        [NixOp::BuildImage {
            variant: "proxmox".to_string()
        }],
        "the typed variant reaches the command"
    );
}
