use masys_app::nix_buffer::{build_nix_rows, is_nix_unit};
use masys_domain::declarative::{Generation, HomeMode, Inputs, Profile, ProfileKind, RebootState};
use masys_domain::unit::{ActiveState, Unit, UnitKind};
use masys_view::{Node, SectionKind};
use std::collections::HashSet;

fn generation(id: u64, current: bool, booted: bool) -> Generation {
    Generation {
        id,
        created_ms: Some(1_000_000 + id * 1000),
        store_path: Some(format!("/nix/store/g{id}")),
        label: Some("26.11".to_string()),
        kernel: None,
        current,
        booted,
    }
}

/// A generation whose link could not be resolved: no store path, no
/// mtime. Two of these must never compare equal to each other.
fn dangling(id: u64) -> Generation {
    Generation { id, created_ms: None, store_path: None, label: None, kernel: None, current: false, booted: false }
}

fn system(generations: Vec<Generation>) -> Profile {
    Profile { kind: ProfileKind::System, path: "/nix/var/nix/profiles/system".to_string(), writable: Some(true), generations }
}

fn channels(generations: Vec<Generation>) -> Profile {
    Profile {
        kind: ProfileKind::Channels,
        path: "/nix/var/nix/profiles/per-user/root/channels".to_string(),
        writable: Some(true),
        generations,
    }
}

fn headers(rows: &[Node]) -> Vec<SectionKind> {
    rows.iter()
        .filter_map(|row| match row {
            Node::SectionHeader { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect()
}

/// A healthy host has nothing to complain about, so the reboot row is not
/// there at all. Same triage thesis as the Status buffer.
///
/// Store is the exception the thesis needs, and is asserted here rather
/// than in a test of its own: it reports the declared maintenance policy,
/// not a complaint, so it renders on the healthiest host there is.
#[test]
fn no_reboot_row_when_no_reboot_is_pending() {
    let rows = build_nix_rows(
        Some(&[system(vec![generation(1, true, true)])]),
        None,
        None,
        HomeMode::Absent,
        None,
        Some(0),
        2_000_000,
        None,
        None,
        Vec::new(),
        Vec::new(),
        &HashSet::new(),
    );
    assert!(!rows.iter().any(|row| matches!(row, Node::RebootPending { .. })));
    assert!(headers(&rows).contains(&SectionKind::NixStore), "Store renders even with nothing wrong, got {:?}", headers(&rows));
}

/// The dangling generation is the point of the arrangement, not scenery.
/// It resolves to no store path, and neither of the paths being looked up
/// is one - so a builder that lets an unknown match anything reports it as
/// both the booted and the current system, and this fails.
#[test]
fn a_pending_reboot_leads_the_view_and_carries_both_generation_numbers() {
    let state = RebootState {
        booted_store_path: "/nix/store/g2".to_string(),
        current_store_path: "/nix/store/g3".to_string(),
        kernel_changed: false,
        initrd_changed: false,
    };
    let profiles = [system(vec![dangling(1), generation(2, false, true), generation(3, true, false)])];

    let rows = build_nix_rows(
        Some(&profiles),
        Some(&state),
        None,
        HomeMode::Absent,
        None,
        Some(0),
        2_000_000,
        None,
        None,
        Vec::new(),
        Vec::new(),
        &HashSet::new(),
    );

    let Some(Node::RebootPending { booted, current, kernel_changed, .. }) = rows.first() else {
        panic!("the reboot row leads the view, got {:?}", rows.first());
    };
    assert_eq!((*booted, *current), (Some(2), Some(3)));
    assert!(!kernel_changed);

    // And its row reports no age at all. An mtime that could not be read
    // is not a generation built on 1970-01-01.
    let age = rows.iter().find_map(|row| match row {
        Node::Generation { generation, age_ms, .. } if generation.id == 1 => Some(*age_ms),
        _ => None,
    });
    assert_eq!(age, Some(None), "a dangling generation has no age to report");
}

/// Newest first: the generation you want is almost always the last one or
/// the one before it, and enough rows of ascending numbers accumulate to
/// put both off screen.
#[test]
fn generations_are_listed_newest_first() {
    let profiles = [system(vec![generation(1, false, false), generation(2, false, false), generation(3, true, false)])];

    let rows = build_nix_rows(
        Some(&profiles),
        None,
        None,
        HomeMode::Absent,
        None,
        Some(0),
        2_000_000,
        None,
        None,
        Vec::new(),
        Vec::new(),
        &HashSet::new(),
    );

    let ids: Vec<u64> = rows
        .iter()
        .filter_map(|row| match row {
            Node::Generation { generation, .. } => Some(generation.id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![3, 2, 1]);
}

/// Absent is not empty. A host with no home-manager gets no section, not a
/// section saying zero.
///
/// The empty profile is real: `/nix/var/nix/profiles/per-user/root` exists
/// and holds nothing on a flake host, so the adapter reports a `Channels`
/// profile with no generations. A "Channel generations 0" header sends the
/// operator hunting for channels they do not have.
#[test]
fn a_profile_with_no_generations_contributes_no_section() {
    let profiles = [system(vec![generation(1, true, false)]), channels(Vec::new())];
    let rows = build_nix_rows(
        Some(&profiles),
        None,
        None,
        HomeMode::Absent,
        None,
        Some(0),
        2_000_000,
        None,
        None,
        Vec::new(),
        Vec::new(),
        &HashSet::new(),
    );
    assert_eq!(headers(&rows).iter().filter(|kind| matches!(kind, SectionKind::Generations(_))).count(), 1);
}

/// And no separator left where that section would have been: the spacer
/// between sections is a separator, not a trailing margin.
#[test]
fn no_inputs_means_no_inputs_section() {
    let rows = build_nix_rows(
        Some(&[system(vec![generation(1, true, false)])]),
        None,
        None,
        HomeMode::Absent,
        None,
        Some(0),
        2_000_000,
        None,
        None,
        Vec::new(),
        Vec::new(),
        &HashSet::new(),
    );
    assert!(!headers(&rows).contains(&SectionKind::Inputs));
    assert!(!matches!(rows.last(), Some(Node::Spacer)), "the buffer does not end on a blank line, got {:?}", rows.last());
}

/// The drift check. Nothing else reports this state.
#[test]
fn a_lock_that_has_moved_past_the_running_system_is_reported() {
    use masys_domain::declarative::{Input, InputSource};
    let inputs = Inputs {
        source: InputSource::Flake { lock_path: "/x/flake.lock".to_string() },
        inputs: vec![Input {
            name: "nixpkgs".to_string(),
            origin: Some("NixOS/nixpkgs".to_string()),
            rev: Some("bbbb".to_string()),
            last_modified_secs: Some(1_785_828_668),
            direct: true,
        }],
    };

    let rows = build_nix_rows(
        Some(&[system(vec![generation(1, true, false)])]),
        None,
        Some(&inputs),
        HomeMode::Absent,
        Some("aaaa"),
        Some(0),
        2_000_000,
        None,
        None,
        Vec::new(),
        Vec::new(),
        &HashSet::new(),
    );

    let Some(Node::SectionHeader { title, .. }) =
        rows.iter().find(|row| matches!(row, Node::SectionHeader { kind: SectionKind::Inputs, .. }))
    else {
        panic!("an Inputs section header");
    };
    assert!(title.contains("drift"), "header should report drift, got {title:?}");
}

/// A section that was folded stays folded when its heading changes under
/// it - which, for two of this view's headings, is a thing that happens.
///
/// The Inputs title carries both revisions and the drift verdict, so a
/// rebuild in the next terminal turns `drift - not rebuilt` into
/// `in sync`. While `collapsed` was keyed on the title, that renamed the
/// section: the fold silently came undone and the old key was stranded in
/// the set for the rest of the session. It is the exact workflow this view
/// is for - watch for drift, rebuild, watch it clear.
///
/// The fold key comes from `SectionKind`, which does not move.
#[test]
fn folding_inputs_survives_the_drift_verdict_clearing() {
    use masys_domain::declarative::{Input, InputSource};
    let inputs = Inputs {
        source: InputSource::Flake { lock_path: "/x/flake.lock".to_string() },
        inputs: vec![Input {
            name: "nixpkgs".to_string(),
            origin: Some("NixOS/nixpkgs".to_string()),
            rev: Some("bbbb".to_string()),
            last_modified_secs: Some(1_785_828_668),
            direct: true,
        }],
    };
    let build = |running: &str, collapsed: &HashSet<String>| {
        build_nix_rows(
            Some(&[system(vec![generation(1, true, false)])]),
            None,
            Some(&inputs),
            HomeMode::Absent,
            Some(running),
            Some(0),
            2_000_000,
            None,
            None,
            Vec::new(),
            Vec::new(),
            collapsed,
        )
    };
    let title_of = |rows: &[Node], wanted: SectionKind| {
        rows.iter()
            .find_map(|row| match row {
                Node::SectionHeader { title, kind, .. } if *kind == wanted => Some(title.clone()),
                _ => None,
            })
            .expect("the section is there")
    };
    let inputs_shown = |rows: &[Node]| rows.iter().any(|row| matches!(row, Node::Input { .. }));

    // Folded while the lock is ahead of the running system, the way
    // `App::cycle_section` folds it: by the key its `SectionKind` gives.
    let drifting = build("aaaa", &HashSet::new());
    let drifting_title = title_of(&drifting, SectionKind::Inputs);
    let collapsed: HashSet<String> = [SectionKind::Inputs.fold_key(&drifting_title)].into_iter().collect();
    assert!(!inputs_shown(&build("aaaa", &collapsed)), "the fold takes effect");

    // And now the rebuild lands.
    let synced = build("bbbb", &collapsed);
    assert_ne!(title_of(&synced, SectionKind::Inputs), drifting_title, "the heading has to change for this test to mean anything");
    assert!(!inputs_shown(&synced), "the section reopened itself when its heading changed");
}

/// The same hazard on the other moving heading. `home_mode` is a
/// keep-last-good reading, so the Home-manager title starts out plain and
/// gains `(activated by nixos-rebuild)` on the first tick where the read
/// comes back - renaming the section under anyone who folded it in
/// between.
#[test]
fn folding_home_manager_survives_learning_how_it_is_deployed() {
    let home = Profile {
        kind: ProfileKind::Home,
        path: "/home/user/.local/state/nix/profiles/profile".to_string(),
        writable: Some(true),
        generations: vec![generation(1, true, false)],
    };
    let build = |home_mode: HomeMode, collapsed: &HashSet<String>| {
        build_nix_rows(
            Some(std::slice::from_ref(&home)),
            None,
            None,
            home_mode,
            None,
            Some(0),
            2_000_000,
            None,
            None,
            Vec::new(),
            Vec::new(),
            collapsed,
        )
    };
    let generations_shown = |rows: &[Node]| rows.iter().any(|row| matches!(row, Node::Generation { .. }));

    let key = SectionKind::Generations(ProfileKind::Home).fold_key("Home-manager");
    let collapsed: HashSet<String> = [key].into_iter().collect();
    assert!(!generations_shown(&build(HomeMode::Absent, &collapsed)), "the fold takes effect");
    assert!(!generations_shown(&build(HomeMode::Module, &collapsed)), "the section reopened itself when its heading changed");
}

/// Three generation sections are on screen at once and each folds on its
/// own, so their keys have to differ - `SectionKind::Generations` alone
/// would file all three under one string and fold them together.
#[test]
fn each_profile_folds_under_a_key_of_its_own() {
    let keys: HashSet<String> = [ProfileKind::System, ProfileKind::Home, ProfileKind::Channels]
        .into_iter()
        .map(|profile| SectionKind::Generations(profile).fold_key("System generations"))
        .collect();
    assert_eq!(keys.len(), 3, "two profiles share a fold key: {keys:?}");
}

/// A channel records no `last_modified_secs` at all - not a zero, an
/// absence. Fabricating `0` there would render as "0d old", indistinguishable
/// from an input that was actually rebuilt today.
#[test]
fn an_input_with_no_recorded_mtime_has_no_age() {
    use masys_domain::declarative::{Input, InputSource};
    let inputs = Inputs {
        source: InputSource::Channels,
        inputs: vec![Input { name: "nixos".to_string(), origin: None, rev: None, last_modified_secs: None, direct: true }],
    };

    let rows = build_nix_rows(
        Some(&[system(vec![generation(1, true, false)])]),
        None,
        Some(&inputs),
        HomeMode::Absent,
        None,
        Some(0),
        2_000_000,
        None,
        None,
        Vec::new(),
        Vec::new(),
        &HashSet::new(),
    );

    let age = rows.iter().find_map(|row| match row {
        Node::Input { input, age_days } if input.name == "nixos" => Some(*age_days),
        _ => None,
    });
    assert_eq!(age, Some(None), "a channel input with no recorded mtime has no age to report");
}

/// The one predicate the section costs, and the two deliberate holes in
/// it.
///
/// `nix-gc.timer` and `nix-optimise.timer` are already Store policy rows,
/// carrying their schedule, their last run and their retention - more than
/// a unit row says about either. Listing them again six lines below would
/// be the same two facts twice. Their *services* stay: `nix-gc.service` is
/// where a collection is started from and where a failed one shows.
#[test]
fn nix_owns_its_units_but_the_store_section_keeps_the_two_timers() {
    for name in [
        "nix-daemon.service",
        "nix-daemon.socket",
        "nix-store.mount",
        "nix-gc.service",
        "nix-optimise.service",
        "home-manager-operator.service",
        "nixos-upgrade.service",
    ] {
        assert!(is_nix_unit(name), "{name} is one of Nix's own");
    }
    for name in ["nix-gc.timer", "nix-optimise.timer"] {
        assert!(!is_nix_unit(name), "{name} is already a Store policy row");
    }
    for name in ["sshd.service", "dbus.socket", "home-assistant.service", "unixodbc.service"] {
        assert!(!is_nix_unit(name), "{name} is not Nix's");
    }
}

fn an_open_detail(name: &str) -> Node {
    let Node::Unit { unit, .. } = a_unit(name) else { unreachable!() };
    Node::UnitDetail { unit, detail: None }
}

fn a_unit(name: &str) -> Node {
    Node::Unit {
        unit: Unit {
            name: name.to_string(),
            kind: UnitKind::Service,
            active_state: ActiveState::Active,
            sub_state: "running".to_string(),
            exit_code: None,
            enabled: true,
            restart_timestamps_ms: Vec::new(),
            since_ms: 1_000,
            cgroup: None,
            slice: None,
            timer: None,
            triggers: Vec::new(),
        },
        age_ms: Some(1_000),
        expanded: false,
        depth: 0,
        children: false,
    }
}

/// The section the design's mock has and the view did not: `SectionKind`
/// named it, `cycle_section` and `group_names` both matched on it, and
/// nothing constructed it.
///
/// Last of the sections, because it is the only one about what is
/// *running* - everything above it is what the configuration declares and
/// what was realised from it.
#[test]
fn nix_units_are_a_section_of_their_own_at_the_end() {
    let rows = build_nix_rows(
        Some(&[system(vec![generation(1, true, true)])]),
        None,
        None,
        HomeMode::Absent,
        None,
        Some(0),
        2_000_000,
        None,
        None,
        Vec::new(),
        // With one of them open, which is what an operator pressing
        // `enter` produces: the detail is a row of its own beside the
        // unit's, and the heading counts units.
        vec![a_unit("nix-daemon.service"), an_open_detail("nix-daemon.service"), a_unit("nix-daemon.socket")],
        &HashSet::new(),
    );
    let header = rows.iter().find_map(|row| match row {
        Node::SectionHeader { title, kind: SectionKind::NixUnits, count } => Some((title.clone(), *count)),
        _ => None,
    });
    assert_eq!(header, Some(("Nix units".to_string(), Some(2))), "a heading that grew because somebody opened a row reports the cursor");
    assert_eq!(headers(&rows).last(), Some(&SectionKind::NixUnits), "it comes last, got {:?}", headers(&rows));
    assert_eq!(rows.iter().filter(|row| matches!(row, Node::Unit { .. })).count(), 2);
}

/// Absent is not empty, the rule every other section here follows: a host
/// where the poll found none of Nix's units gets no heading, rather than
/// one reading zero.
#[test]
fn no_nix_units_means_no_nix_units_section() {
    let rows = build_nix_rows(
        Some(&[system(vec![generation(1, true, true)])]),
        None,
        None,
        HomeMode::Absent,
        None,
        Some(0),
        2_000_000,
        None,
        None,
        Vec::new(),
        Vec::new(),
        &HashSet::new(),
    );
    assert!(!headers(&rows).contains(&SectionKind::NixUnits));
    assert!(!matches!(rows.last(), Some(Node::Spacer)), "and no separator where it would have been");
}

/// It folds like every other section here, and under a key from its kind
/// rather than its title.
#[test]
fn the_nix_units_section_folds() {
    let collapsed: HashSet<String> = [SectionKind::NixUnits.fold_key("Nix units")].into_iter().collect();
    let rows = build_nix_rows(
        Some(&[system(vec![generation(1, true, true)])]),
        None,
        None,
        HomeMode::Absent,
        None,
        Some(0),
        2_000_000,
        None,
        None,
        Vec::new(),
        vec![a_unit("nix-daemon.service"), a_unit("nix-daemon.socket")],
        &collapsed,
    );
    let count = rows.iter().find_map(|row| match row {
        Node::SectionHeader { kind: SectionKind::NixUnits, count, .. } => Some(*count),
        _ => None,
    });
    assert_eq!(count, Some(Some(2)), "the heading stays, and keeps saying how many are behind it");
    assert!(!rows.iter().any(|row| matches!(row, Node::Unit { .. })), "the rows are gone");
}
