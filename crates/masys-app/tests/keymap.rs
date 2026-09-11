use masys_app::buffer::{Buffer, BufferGates, Registry};
use masys_app::key::{Key, KeyCode};
use masys_app::keymap::{Action, Keymap, Sort, UnitVerb};

/// The two hosts masys has to be right on: one with a declarative service
/// and one without.
///
/// Every claim about which digits exist has to hold on both, because the
/// answer differs between them - and that difference is the whole reason
/// the registry exists. A test that only ever built the default keymap
/// would be checking the Debian answer and calling it the rule.
/// Every host shape the two gates can produce.
///
/// All four combinations rather than the two this built until 2026-08-30,
/// which varied `declarative` and pinned `packages` to `false` - so every
/// guard that iterates this helper was, without saying so, a guard that
/// never saw the Packages buffer. The gates are independent (a Debian
/// host lists packages and has no generations), so the honest cover is
/// the product, not a slice through it.
fn hosts() -> [(Registry, Keymap); 4] {
    [(false, false), (false, true), (true, false), (true, true)].map(|(declarative, packages)| {
        let registry = Registry::new(BufferGates {
            declarative,
            packages,
        });
        (registry.clone(), Keymap::for_registry(registry))
    })
}

/// No `hjkl` yet: which letters mean movement is unsettled, and binding
/// them would both prejudge that and spend four letters from the
/// namespace reserved for contextual actions. Arrows prejudge nothing.
#[test]
fn the_default_keymap_moves_with_arrows_and_binds_no_movement_letters() {
    let keymap = Keymap::default();
    for letter in ['h', 'j', 'k', 'l'] {
        let action = keymap.resolve(Buffer::Status, Key::char(letter));
        assert!(
            !matches!(
                action,
                Some(Action::MoveDown)
                    | Some(Action::MoveUp)
                    | Some(Action::PageDown)
                    | Some(Action::PageUp)
            ),
            "{letter} must not move the cursor yet, but resolves to {action:?}"
        );
    }
    for (key, action) in [
        (Key::new(KeyCode::Down), Action::MoveDown),
        (Key::new(KeyCode::Up), Action::MoveUp),
        (Key::new(KeyCode::PageDown), Action::PageDown),
        (Key::new(KeyCode::PageUp), Action::PageUp),
    ] {
        assert_eq!(keymap.resolve(Buffer::Status, key), Some(action), "{key:?}");
    }
}

#[test]
fn the_base_keymap_binds_the_global_verbs() {
    let keymap = Keymap::default();
    for (key, action) in [
        (Key::char('q'), Action::Quit),
        (Key::char('g'), Action::Refresh),
        (Key::char('?'), Action::Help),
        (Key::new(KeyCode::Tab), Action::Cycle),
    ] {
        assert_eq!(keymap.resolve(Buffer::Status, key), Some(action), "{key:?}");
    }
}

/// The design's one test-enforced keymap invariant: a per-buffer overlay
/// may not shadow a protected key. Without it, per-buffer keymaps are what
/// make a TUI unlearnable - the first draft of the design had the procs
/// buffer bind `g` to grouping, shadowing the global refresh.
/// The design's one test-enforced keymap invariant: a per-buffer overlay
/// may not shadow a protected key. Without it, per-buffer keymaps are what
/// make a TUI unlearnable - the design's own first draft had the procs
/// buffer bind `g` to grouping, shadowing the global refresh.
#[test]
fn no_overlay_shadows_a_protected_key() {
    for keymap in [Keymap::default()] {
        for buffer in Buffer::ALL {
            for key in every_key() {
                if !keymap.is_protected(key) {
                    continue;
                }
                let base = keymap
                    .base_action(key)
                    .expect("a protected key is bound in the base map");
                assert_eq!(
                    keymap.resolve(*buffer, key),
                    Some(base),
                    "{buffer:?} shadows the protected key {key:?}, which must keep meaning {base:?}"
                );
            }
        }
    }
}

/// The protected set is the design's - "the movement keys, q, ?, g, b" -
/// and is derived from what is bound rather than hardcoded, so a layout
/// that moves on different letters protects *those*.
#[test]
fn the_protected_set_follows_the_layout() {
    let arrows = Keymap::default();
    for key in [
        Key::new(KeyCode::Down),
        Key::new(KeyCode::Up),
        Key::char('q'),
        Key::char('?'),
        Key::char('g'),
    ] {
        assert!(arrows.is_protected(key), "{key:?} should be protected");
    }
    // A letter bound only by an overlay is not protected, so the buffer
    // that owns it keeps it.
    assert!(
        !arrows.is_protected(Key::char('m')),
        "m is the Procs memory sort, not global navigation"
    );
}

/// The rule this change is built on: a key is global when it means the
/// same thing in every buffer. So every global must resolve identically in
/// all of them - if one buffer disagrees, it is not a global, it is a buffer
/// key wearing a global's clothes.
#[test]
fn every_global_means_the_same_thing_in_every_buffer() {
    for (registry, keymap) in hosts() {
        for key in globals(&registry) {
            let meaning = keymap
                .base_action(key)
                .unwrap_or_else(|| panic!("{key:?} is listed global but bound to nothing"));
            for buffer in Buffer::ALL {
                assert_eq!(
                    keymap.resolve(*buffer, key),
                    Some(meaning),
                    "{key:?} means something else in {buffer:?}"
                );
            }
        }
    }
}

/// `/` and `tab` were global in behaviour but absent from the protected
/// set, so an overlay could quietly take either. Under the rule they are
/// globals like any other and no buffer may claim them.
#[test]
fn the_protected_set_is_exactly_the_globals() {
    for (registry, keymap) in hosts() {
        for key in globals(&registry) {
            assert!(
                keymap.is_protected(key),
                "{key:?} is global and must be protected"
            );
        }
        for key in [
            Key::char('r'),
            Key::char('c'),
            Key::char('m'),
            Key::char('l'),
        ] {
            assert!(
                !keymap.is_protected(key),
                "{key:?} belongs to a view, not to everyone"
            );
        }
    }
}

/// Views move to digits so the five letters they used to hold - `s t u j
/// d` - go back to the buffers themselves. Those five were never navigation
/// *within* a buffer; they were a menu, and a menu is what digits are for.
#[test]
fn a_digit_opens_its_buffer_and_no_bare_letter_does() {
    for (registry, keymap) in hosts() {
        for spec in registry.specs() {
            let Some(digit) = spec.key else { continue };
            assert_eq!(
                keymap.resolve(Buffer::Status, Key::char(digit)),
                Some(Action::Open(spec.buffer)),
                "{digit} opens {:?}",
                spec.buffer
            );
        }
        for letter in ['s', 't', 'u', 'j', 'd'] {
            assert!(
                !matches!(
                    keymap.resolve(Buffer::Status, Key::char(letter)),
                    Some(Action::Open(_))
                ),
                "`{letter}` still opens a buffer; it belongs to the buffers now"
            );
        }
    }
}

/// Section jumping is the clearest case the rule exists for: a letter,
/// and global anyway, because it is the same act everywhere.
#[test]
fn section_jumping_stays_global_because_it_means_one_thing() {
    let keymap = Keymap::default();
    for buffer in Buffer::ALL {
        assert_eq!(
            keymap.resolve(*buffer, Key::char('n')),
            Some(Action::NextSection),
            "{buffer:?}"
        );
        assert_eq!(
            keymap.resolve(*buffer, Key::char('p')),
            Some(Action::PrevSection),
            "{buffer:?}"
        );
    }
}

/// The globals, as the design doc lists them, for a host with `registry`'s
/// buffers.
fn globals(registry: &Registry) -> Vec<Key> {
    let mut keys = vec![
        Key::new(KeyCode::Down),
        Key::new(KeyCode::Up),
        Key::new(KeyCode::PageDown),
        Key::new(KeyCode::PageUp),
        Key::new(KeyCode::Home),
        Key::new(KeyCode::End),
        Key::new(KeyCode::Tab),
        Key::char('n'),
        Key::char('p'),
        Key::char('/'),
        Key::char('g'),
        Key::char('?'),
        Key::char('q'),
    ];
    // The digits actually in the ring on this host - the log buffer has
    // none, and a buffer the host does not have is not a global because it
    // is not anything.
    keys.extend(
        registry
            .specs()
            .iter()
            .filter_map(|spec| spec.key)
            .map(Key::char),
    );
    keys
}

/// `C` and `E` are gone: `shift-tab` already folds and unfolds every
/// group, so they were two letters spent on what one global key does.
///
/// Measured before removing them - on this host `E` gave 347 process
/// rows, `C` gave 0, and `shift-tab` cycled between exactly those two.
#[test]
fn fold_all_and_unfold_are_not_separate_keys() {
    let keymap = Keymap::default();
    for key in [Key::char('C'), Key::char('E')] {
        assert_eq!(
            keymap.resolve(Buffer::Procs, key),
            None,
            "{key:?} still binds something in procs"
        );
    }
    for name in ["collapse_all", "expand_all"] {
        assert!(
            masys_app::keymap::ACTIONS
                .iter()
                .all(|(known, _)| *known != name),
            "`{name}` is still configurable"
        );
    }
}

/// Every key an overlay might plausibly try to claim.
fn every_key() -> Vec<Key> {
    let mut keys: Vec<Key> = ('a'..='z').chain('A'..='Z').map(Key::char).collect();
    keys.extend([
        Key::char('?'),
        Key::new(KeyCode::Down),
        Key::new(KeyCode::Up),
        Key::new(KeyCode::Tab),
    ]);
    keys
}

/// "Each buffer has its own actions" - and the help says which are which.
#[test]
fn each_buffer_has_its_own_bindings_and_they_are_described() {
    let keymap = Keymap::default();
    assert_eq!(
        keymap.resolve(Buffer::Procs, Key::char('c')),
        Some(Action::SortBy(Sort::Cpu))
    );
    assert_eq!(
        keymap.resolve(Buffer::Procs, Key::char('m')),
        Some(Action::SortBy(Sort::Memory))
    );
    // `a` for alphabetical: `n` is next-section now.
    assert_eq!(
        keymap.resolve(Buffer::Procs, Key::char('a')),
        Some(Action::SortBy(Sort::Name))
    );
    assert_eq!(
        keymap.resolve(Buffer::Status, Key::char('c')),
        None,
        "a Procs key does not leak into Status"
    );

    let headings: Vec<String> = keymap
        .describe(Buffer::Procs)
        .into_iter()
        .map(|g| g.heading)
        .collect();
    assert_eq!(
        headings,
        vec!["Movement", "Global", "Buffers", "Procs only"]
    );
}

/// A buffer with no verbs of its own gets the global keys and nothing
/// else - no empty "<buffer> only" heading, the same rule every buffer
/// follows for a section with nothing in it.
///
/// Asked of Packages, which is the thinnest buffer masys has: a list and
/// a header, with nothing to act on. It was asked of Status until
/// 2026-08-30, when the finding jump gave Status a key of its own and
/// Status stopped being an example of this rule. The rule is unchanged;
/// only the buffer that demonstrates it moved.
#[test]
fn a_buffer_with_no_actions_of_its_own_gets_no_section() {
    let keymap = Keymap::for_registry(Registry::new(BufferGates::ALL));
    let headings: Vec<String> = keymap
        .describe(Buffer::Packages)
        .into_iter()
        .map(|g| g.heading)
        .collect();
    assert_eq!(headings, vec!["Movement", "Global", "Buffers"]);
    assert!(
        keymap
            .actions(Buffer::Packages)
            .iter()
            .all(|b| b.label == "filter"),
        "only the global filter"
    );
}

/// **Status has exactly one key of its own: the finding jump.**
///
/// It is triage, and the jump is what makes it a place to act from
/// rather than only read. Owned by Status, so it appears in that
/// buffer's footer and section and in no other's - a key advertised
/// where it does nothing is the shape that put `[5] nix` on a Debian
/// host.
#[test]
fn status_owns_the_finding_jump_and_nothing_else() {
    let keymap = Keymap::default();
    let headings: Vec<String> = keymap
        .describe(Buffer::Status)
        .into_iter()
        .map(|g| g.heading)
        .collect();
    assert_eq!(
        headings,
        vec!["Movement", "Global", "Buffers", "Status only"]
    );
    assert_eq!(
        keymap.resolve(Buffer::Status, Key::char('.')),
        Some(Action::JumpToFinding)
    );
    for buffer in Buffer::ALL {
        if *buffer == Buffer::Status {
            continue;
        }
        assert_eq!(
            keymap.resolve(*buffer, Key::char('.')),
            None,
            "{buffer:?} must not offer the finding jump"
        );
    }
}

/// `T` was the top-processes toggle and is now bound to nothing, in any
/// buffer - the rankings it hid are gone with it.
#[test]
fn the_top_processes_toggle_is_gone() {
    let keymap = Keymap::default();
    for buffer in Buffer::ALL {
        assert_eq!(
            keymap.resolve(*buffer, Key::char('T')),
            None,
            "{buffer:?} still binds T"
        );
    }
    assert!(
        masys_app::keymap::ACTIONS
            .iter()
            .all(|(name, _)| *name != "toggle_top_processes"),
        "still configurable"
    );
}

/// The footer lists every buffer, with the one you are in marked.
///
/// It used to omit the current view, which made the row shorter but also
/// made it move: the same key sat in a different place depending on where
/// you were, so the footer could not be read by position. Listing all of
/// them keeps the row fixed and turns it into a map - and the marked
/// entry is the only thing that changes as you move.
#[test]
fn the_footer_lists_every_buffer_and_marks_the_current_one() {
    for (registry, keymap) in hosts() {
        for buffer in Buffer::ALL {
            let hints = keymap.hints(*buffer);
            let buffers: Vec<&masys_view::KeyBinding> = hints
                .iter()
                .filter(|h| h.chord.chars().all(|c| c.is_ascii_digit()))
                .collect();
            let ring: Vec<Buffer> = registry
                .specs()
                .iter()
                .filter(|s| s.key.is_some())
                .map(|s| s.buffer)
                .collect();
            assert_eq!(
                buffers.len(),
                ring.len(),
                "every buffer in the ring is listed in {buffer:?}"
            );

            // The log buffer is a drill-down and not in the ring, so standing
            // in it marks nothing - which is honest: no digit would take you
            // back. Nor does a buffer this host does not have, for the plainer
            // reason that you cannot be standing in one.
            let active: Vec<&str> = buffers
                .iter()
                .filter(|h| h.active)
                .map(|h| h.label.as_str())
                .collect();
            let expected = usize::from(ring.contains(buffer));
            assert_eq!(active.len(), expected, "in {buffer:?}: {active:?}");
            if expected == 1 {
                assert_eq!(
                    active[0],
                    buffer.title().to_lowercase(),
                    "and it is the one you are in"
                );
            }
        }
    }
}

/// The global verbs are not buffers and are never marked.
#[test]
fn the_global_verbs_are_never_marked_current() {
    let hints = Keymap::default().hints(Buffer::Status);
    for verb in ["g", "?", "q"] {
        let hint = hints
            .iter()
            .find(|h| h.chord == verb)
            .unwrap_or_else(|| panic!("{verb} missing"));
        assert!(!hint.active, "{verb} is marked as a buffer");
    }
}

/// View switching is on digits, so every bare letter stays available for
/// whatever the row under the cursor supports.
#[test]
fn a_key_that_names_no_buffer_resolves_to_nothing() {
    let keymap = Keymap::default();
    let opens = |c: char| match keymap.resolve(Buffer::Status, Key::char(c)) {
        Some(Action::Open(buffer)) => Some(buffer),
        _ => None,
    };
    assert_eq!(opens('2'), Some(Buffer::Procs));
    assert_eq!(opens('1'), Some(Buffer::Status));
    assert_eq!(opens('z'), None);
    // And the letters that used to: they belong to the buffers now.
    assert_eq!(opens('t'), None);
    assert_eq!(opens('s'), None);
}

/// `b` was the buffer prefix and is now free. It must mean nothing
/// rather than lingering as a key that silently swallows the next press.
#[test]
fn b_is_unbound_now_that_the_prefix_is_gone() {
    let keymap = Keymap::default();
    assert_eq!(keymap.resolve(Buffer::Status, Key::char('b')), None);
    assert!(
        !keymap.is_protected(Key::char('b')),
        "an overlay may claim it"
    );
}

#[test]
fn an_unbound_key_resolves_to_nothing() {
    assert_eq!(
        Keymap::default().resolve(Buffer::Status, Key::char('Z')),
        None
    );
}

/// The footer is the only place most keys are ever read from, so a
/// footer that prints a table rather than the live keymap advertises
/// keys that no longer do anything. Rebinding is the whole feature.
#[test]
fn the_footer_names_the_key_that_is_actually_bound() {
    // `G` rather than `R`, which reload now holds: a global taking a
    // letter a buffer already owns is a real conflict, and the keymap
    // reports it rather than swallowing it - which is a different test's
    // subject, not this one's.
    let (keymap, problems) = Keymap::with_overrides_for(
        Registry::default(),
        &[("refresh".to_string(), "G".to_string())],
    );
    assert!(problems.is_empty(), "{problems:?}");

    let hints = keymap.hints(Buffer::Status);
    let refresh = hints
        .iter()
        .find(|h| h.label == "refresh")
        .expect("a refresh hint");
    assert_eq!(refresh.chord, "G");
    assert!(
        !hints.iter().any(|h| h.chord == "g"),
        "and nothing still offers the old key: {hints:?}"
    );
}

/// The same rule for a buffer jump, which reaches the footer through the
/// registry rather than through the keymap and so had its own copy of
/// the key.
#[test]
fn the_footer_names_a_rebound_buffer_jump() {
    let (keymap, problems) = Keymap::with_overrides_for(
        Registry::default(),
        &[("buffer_io".to_string(), "L".to_string())],
    );
    assert!(problems.is_empty(), "{problems:?}");

    let hints = keymap.hints(Buffer::Status);
    let io = hints.iter().find(|h| h.label == "io").expect("an io hint");
    assert_eq!(io.chord, "L");
}

/// And the help, which must agree with the footer - two places that
/// disagree about a key are worse than one that is merely wrong.
#[test]
fn the_help_names_the_key_that_is_actually_bound() {
    let (keymap, _) = Keymap::with_overrides_for(
        Registry::default(),
        &[("buffer_io".to_string(), "L".to_string())],
    );
    let chords: Vec<String> = keymap
        .describe(Buffer::Status)
        .into_iter()
        .flat_map(|g| g.bindings)
        .map(|b| b.chord)
        .collect();
    assert!(chords.contains(&"L".to_string()), "{chords:?}");
    assert!(
        !chords.contains(&"4".to_string()),
        "the old digit is gone: {chords:?}"
    );
}

/// A rebound buffer jump moves in `resolve` - and `resolve` is the whole
/// answer, because it is what `App::handle_key` calls.
///
/// This test replaced one that asked `Keymap::resolve_buffer` the same
/// question. That lookup read the registry, which is a catalogue of which
/// buffers a host *has* and does not move when the operator rebinds a key,
/// so it answered `None` here while the app opened io perfectly well. Two
/// tables, and the tests were pinned to the one nothing read.
#[test]
fn a_rebound_buffer_jump_moves_and_the_old_digit_stops_opening_it() {
    let (keymap, problems) = Keymap::with_overrides_for(
        Registry::default(),
        &[("buffer_io".to_string(), "L".to_string())],
    );
    assert!(problems.is_empty(), "{problems:?}");

    assert_eq!(
        keymap.resolve(Buffer::Status, Key::char('L')),
        Some(Action::Open(Buffer::Io)),
        "the override moved the binding"
    );
    assert!(
        !matches!(
            keymap.resolve(Buffer::Status, Key::char('4')),
            Some(Action::Open(Buffer::Io))
        ),
        "and the digit it moved away from no longer opens io"
    );
}

/// The `?` popup is the only discovery mechanism masys has. A key that is
/// bound but unlisted is a key nobody finds - which is what happened to
/// `/`, `n` and `p`, all of them added after the groups were written.
#[test]
fn the_help_lists_every_key_the_base_map_binds() {
    let keymap = Keymap::default();
    let listed: Vec<String> = keymap
        .describe(Buffer::Status)
        .into_iter()
        .flat_map(|g| g.bindings)
        .map(|b| b.chord)
        .collect();

    for (name, action) in masys_app::keymap::ACTIONS {
        // Per-buffer actions live in the open buffer's own group and are
        // covered by the overlay test below.
        // An action with no key of its own is reached from a transient,
        // and a transient's rows are not the base map's business.
        let Some(key) = key_for(*action) else {
            continue;
        };
        if masys_app::keymap::Keymap::default()
            .base_action(key)
            .is_none()
        {
            continue;
        }
        assert!(
            listed.contains(&key.spelling()),
            "`{name}` is bound to `{}` but the help never lists it",
            key.spelling()
        );
    }
}

/// The default binding for an action, as the table spells it, or `None`
/// for an action that has no key of its own because a transient is where
/// it is reached - `unit_enable` since the transient engine landed.
fn key_for(action: Action) -> Option<Key> {
    let name = action.name().expect("every action has a config name");
    let spelling = masys_app::keymap::DEFAULT_BINDINGS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, k)| *k)?;
    Some(Key::parse(spelling).expect("a parseable default"))
}

/// Every action any transient offers on a row of its own.
///
/// Built from a real definition rather than a list written out here,
/// which is what keeps this honest: a verb dropped from the popup stops
/// counting as reachable the moment it is dropped.
fn actions_in_transients() -> Vec<Action> {
    let ownership = masys_domain::platform::Ownership::Imperative;
    use masys_app::keymap::NixFamily;
    let mut defs = vec![masys_app::systemd_buffer::unit_transient(
        "sshd.service",
        Some(&ownership),
    )];
    // Every Nix family, with a generation under the cursor so the
    // Generation family's three verbs - reachable only there - count too.
    for family in [
        NixFamily::Rebuild,
        NixFamily::Generation,
        NixFamily::Inputs,
        NixFamily::Store,
        NixFamily::Search,
    ] {
        defs.push(masys_app::nix_buffer::nix_family_transient(
            family,
            Some(436),
            Some("14d"),
        ));
    }
    defs.into_iter()
        .flat_map(|def| def.groups)
        .flat_map(|group| group.rows)
        .map(|row| row.action)
        .collect()
}

/// A buffer missing from the registry compiles fine and is simply
/// unreachable - no key opens it, and its title falls back to something
/// shared, so its cursor collides with every other unregistered buffer's.
/// That is exactly the bug this test exists to catch, having already
/// happened once.
///
/// Against the catalogue and the host that has every buffer in it: the claim
/// is that no buffer was left out of the table, and a host that omits one
/// deliberately cannot answer that.
#[test]
fn every_buffer_is_registered_and_reachable() {
    let keymap = Keymap::for_registry(Registry::new(BufferGates::ALL));
    for buffer in Buffer::ALL {
        let spec = masys_app::buffer::BUFFERS
            .iter()
            .find(|spec| spec.buffer == *buffer)
            .unwrap_or_else(|| panic!("{buffer:?} has no BUFFERS entry, so no key can reach it"));
        // The log buffer has no digit: it is a drill-down, reached by `l`
        // from a unit and left by `esc`.
        let Some(key) = spec.key else { continue };
        assert_eq!(
            keymap.resolve(Buffer::Status, Key::char(key)),
            Some(Action::Open(*buffer)),
            "{key} must open {buffer:?}"
        );
    }
}

/// Two buffers on one letter would make the second unreachable, and the
/// registry is ordered so the first would silently win.
#[test]
fn no_two_buffers_share_a_key() {
    let mut keys: Vec<char> = masys_app::buffer::BUFFERS
        .iter()
        .filter_map(|spec| spec.key)
        .collect();
    let before = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), before, "duplicate buffer keys: {keys:?}");
}

/// Titles are the identity for per-buffer cursor state, so a duplicate
/// would make two buffers share one cursor.
#[test]
fn no_two_buffers_share_a_title() {
    let mut titles: Vec<&str> = masys_app::buffer::BUFFERS
        .iter()
        .map(|spec| spec.title)
        .collect();
    let before = titles.len();
    titles.sort_unstable();
    titles.dedup();
    assert_eq!(titles.len(), before, "duplicate buffer titles: {titles:?}");
}

/// Single-letter jumps - the only way to switch buffers.
#[test]
fn a_bare_letter_opens_its_buffer() {
    for (registry, keymap) in hosts() {
        for spec in registry.specs() {
            let Some(key) = spec.key else { continue };
            assert_eq!(
                keymap.resolve(Buffer::Status, Key::char(key)),
                Some(Action::Open(spec.buffer)),
                "{key} opens {:?}",
                spec.buffer
            );
        }
    }
}

/// Navigation letters are global, so no buffer's overlay may quietly
/// repurpose one - the same rule that protects `q` and `g`.
#[test]
fn buffer_letters_are_protected_from_overlays() {
    for (registry, keymap) in hosts() {
        for spec in registry.specs() {
            let Some(key) = spec.key else { continue };
            assert!(
                keymap.is_protected(Key::char(key)),
                "{key} must be protected"
            );
            for buffer in Buffer::ALL {
                assert_eq!(
                    keymap.resolve(*buffer, Key::char(key)),
                    Some(Action::Open(spec.buffer)),
                    "{buffer:?} must not shadow {key}"
                );
            }
        }
    }
}

/// A buffer letter that collides with a per-buffer action would silently
/// lose to navigation now that it is protected, so the tables must not
/// contain one at all.
#[test]
fn no_buffer_letter_collides_with_a_per_buffer_action() {
    let keymap = Keymap::default();
    for buffer in Buffer::ALL {
        for group in keymap.describe(*buffer) {
            if !group.heading.ends_with("only") {
                continue;
            }
            for binding in group.bindings {
                assert!(
                    !masys_app::buffer::BUFFERS
                        .iter()
                        .any(|spec| spec.chord().as_deref() == Some(binding.chord.as_str())),
                    "{buffer:?} binds {:?}, which is a buffer jump",
                    binding.chord
                );
            }
        }
    }
}

/// The other half of the shadowing rule, and the half that actually
/// bites: a protected key silently *wins* over an overlay, so a
/// per-buffer binding that collides with one is not an error - it just
/// stops working. Every overlay key must survive in every layout.
#[test]
fn no_overlay_binding_is_silently_shadowed_in_any_layout() {
    for (name, keymap) in [("default", Keymap::default())] {
        for buffer in Buffer::ALL {
            for group in keymap.describe(*buffer) {
                if !group.heading.ends_with("only") {
                    continue;
                }
                for binding in group.bindings {
                    let mut chars = binding.chord.chars();
                    let (Some(c), None) = (chars.next(), chars.next()) else {
                        continue;
                    };
                    let action = keymap.resolve(*buffer, Key::char(c));
                    assert!(
                        !matches!(
                            action,
                            Some(Action::MoveDown)
                                | Some(Action::MoveUp)
                                | Some(Action::PageDown)
                                | Some(Action::PageUp)
                        ),
                        "{name} layout: {buffer:?} lists {:?} as its own key, but it resolves to {action:?}",
                        binding.chord
                    );
                }
            }
        }
    }
}

/// The registry says which letter reaches a buffer and the binding table
/// says what that letter does. Two tables that must agree, so a test says
/// so - the alternative is a buffer whose footer advertises one key while
/// another one opens it.
#[test]
fn the_registry_and_the_binding_table_agree_on_every_buffer_key() {
    // The host that has every buffer: the claim is about the two tables, and
    // a host missing one of them has nothing to say about that row. This
    // said "every buffer" while passing `packages: false` from the day the
    // Packages buffer landed until 2026-08-30, which is precisely how a
    // buffer with no binding at all got past it.
    let registry = Registry::new(BufferGates::ALL);
    let keymap = Keymap::for_registry(registry.clone());
    for spec in registry.specs() {
        let bound = masys_app::keymap::DEFAULT_BINDINGS
            .iter()
            .find(|(name, _)| Action::from_name(name) == Some(Action::Open(spec.buffer)))
            .map(|(_, key)| *key);
        match spec.key {
            Some(key) => {
                let bound =
                    bound.unwrap_or_else(|| panic!("{:?} has no default binding", spec.buffer));
                assert_eq!(
                    bound,
                    key.to_string(),
                    "{:?}: registry says {key}",
                    spec.buffer
                );
                assert_eq!(
                    keymap.resolve(Buffer::Status, Key::char(key)),
                    Some(Action::Open(spec.buffer))
                );
            }
            // A buffer outside the ring must have no binding either, or a
            // key would reach it and the registry would not say which.
            None => assert_eq!(
                bound, None,
                "{:?} is not in the ring but is bound to {bound:?}",
                spec.buffer
            ),
        }
    }
}

/// Every action in the table must be *reachable*, or it is an action
/// nothing can run.
///
/// A key was the only way to reach one until the transient engine landed,
/// and this test asked for a default binding. That is now too narrow:
/// `unit_enable` gave `e` up to the transient and lives on the popup's
/// Persistence row, which is a way to reach it and not a key. So the
/// question is reachability, and a popup row answers it.
#[test]
fn every_named_action_is_reachable() {
    let in_transients = actions_in_transients();
    for (name, action) in masys_app::keymap::ACTIONS {
        // `nix_rollback` gave up its row for a switch on the verb it
        // pairs with - reachable, just not as a row or a default binding.
        // Proved in `nix_ops.rs`'s `rollback_is_reached_through_the_rebuild_switch`.
        // And `nix_upgrade`, which never had a row: `--upgrade` is a
        // flag on `switch`, so it is the `-u` switch on the same popup.
        // Proved in `nix_ops.rs`'s `upgrade_is_reached_through_the_rebuild_switch`.
        if *name == "nix_rollback" || *name == "nix_upgrade" {
            continue;
        }
        let bound = masys_app::keymap::DEFAULT_BINDINGS
            .iter()
            .any(|(bound, _)| bound == name);
        assert!(
            bound || in_transients.contains(action),
            "`{name}` is configurable but nothing can reach it"
        );
    }
}

/// And the one action that gave up its key really is in a transient, so
/// the test above cannot pass by finding an empty popup.
#[test]
fn enable_is_reachable_from_the_transient_that_took_its_key() {
    assert!(
        masys_app::keymap::DEFAULT_BINDINGS
            .iter()
            .all(|(name, _)| *name != "unit_enable"),
        "`e` went to the transient"
    );
    assert!(
        actions_in_transients().contains(&Action::Unit(masys_app::keymap::UnitVerb::Enable)),
        "and enable went with it"
    );
}

/// And no two actions may default to the same key in the same scope,
/// which would make one of them unreachable.
///
/// Asked of `describe`, which is every binding available in a buffer -
/// the base map's groups plus that buffer's own overlay. The version
/// before this walked `DEFAULT_BINDINGS` and skipped any binding whose
/// key did not `resolve` back to it, which is exactly the loser of a
/// collision: two actions on one key left one of them filtered out, and
/// the check could never fire for the case it is named for. Confirmed by
/// binding `nix_activate` to `r`, which `unit_restart` already holds in
/// the same overlay - the old test stayed green, this one names both.
///
/// Cross-buffer reuse is still fine and is not what this asks about: `a`
/// sorts by name in Procs and activates a generation in Nix, in two
/// different overlays, and `describe` is per buffer.
#[test]
fn no_two_default_bindings_collide() {
    for (_, keymap) in hosts() {
        for buffer in Buffer::ALL {
            let mut seen: Vec<(String, String)> = Vec::new();
            for group in keymap.describe(*buffer) {
                for binding in group.bindings {
                    if let Some((_, other)) = seen.iter().find(|(chord, _)| *chord == binding.chord)
                    {
                        panic!(
                            "{buffer:?}: `{}` is both `{other}` and `{}`",
                            binding.chord, binding.label
                        );
                    }
                    seen.push((binding.chord, binding.label));
                }
            }
        }
    }
}

/// The point of configurability: any action can be moved to any key.
#[test]
fn an_override_rebinds_by_action_name() {
    let (keymap, problems) = Keymap::with_overrides_for(
        Registry::default(),
        &[("next_section".to_string(), "z".to_string())],
    );
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        keymap.resolve(Buffer::Status, Key::char('z')),
        Some(Action::NextSection)
    );
    assert_eq!(
        keymap.resolve(Buffer::Status, Key::char('n')),
        None,
        "the default is vacated, not kept alongside"
    );
}

/// A typo should cost you the customisation, not the key: the default
/// stays and the problem is reported, rather than the action becoming
/// unreachable.
#[test]
fn a_bad_key_leaves_the_default_in_place_and_says_so() {
    // `hyper+n` rather than `ctrl+n`, which this used to name: ctrl was
    // unparseable only because `Key` could not represent it, and that was
    // the bug, not the example.
    let (keymap, problems) = Keymap::with_overrides_for(
        Registry::default(),
        &[("next_section".to_string(), "hyper+n".to_string())],
    );
    assert_eq!(
        keymap.resolve(Buffer::Status, Key::char('n')),
        Some(Action::NextSection),
        "still bound"
    );
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("hyper+n"), "{problems:?}");
}

/// A misspelled action name is reported rather than silently ignored -
/// otherwise a config that does nothing looks exactly like one that works.
#[test]
fn an_unknown_action_name_is_reported() {
    let (_, problems) = Keymap::with_overrides_for(
        Registry::default(),
        &[("restart_everything".to_string(), "z".to_string())],
    );
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("restart_everything"), "{problems:?}");
}

/// Per-buffer actions are configurable too, and stay in their buffer.
#[test]
fn a_per_buffer_action_can_be_rebound() {
    let (keymap, problems) = Keymap::with_overrides_for(
        Registry::default(),
        &[("unit_restart".to_string(), "z".to_string())],
    );
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        keymap.resolve(Buffer::Systemd, Key::char('z')),
        Some(Action::Unit(masys_app::keymap::UnitVerb::Restart))
    );
    assert_eq!(
        keymap.resolve(Buffer::Procs, Key::char('z')),
        None,
        "still only in its own buffer"
    );
}

/// The Nix units section exists because these keys were supposed to reach
/// it. `owner` filed them under Systemd alone, so they did not.
#[test]
fn a_unit_verb_resolves_in_the_nix_buffer_as_well_as_the_systemd_one() {
    let keymap = Keymap::for_registry(Registry::new(BufferGates {
        declarative: true,
        packages: false,
    }));

    for buffer in [Buffer::Systemd, Buffer::Nix] {
        assert_eq!(
            keymap.resolve(buffer, Key::char('r')),
            Some(Action::Unit(UnitVerb::Restart)),
            "{buffer:?}"
        );
        assert_eq!(
            keymap.resolve(buffer, Key::char('l')),
            Some(Action::Logs),
            "{buffer:?}"
        );
    }
}

/// The `r` collision, decided in the table rather than in the handler.
///
/// `r` is `unit_restart`, and it resolves in the Nix buffer because that
/// buffer shows units. The Nix buffer's own operations took letters of their
/// own rather than a second meaning for that one: the footer and the `?`
/// help both build their labels from the *action* a key resolves to, so a
/// key whose action depended on the row under the cursor would be a key
/// neither of them could state.
///
/// `a`, `d` and `c` cost three letters while they existed. They are gone
/// now - the transient holds all thirteen operations and `r` still means
/// restart here - so what is left to pin is that the Nix buffer took
/// *no* top-level letters and gave none of them a second meaning.
#[test]
fn the_nix_operations_took_their_own_letters_and_left_r_to_units() {
    let keymap = Keymap::for_registry(Registry::new(BufferGates {
        declarative: true,
        packages: false,
    }));

    assert_eq!(
        keymap.resolve(Buffer::Nix, Key::char('r')),
        Some(Action::Unit(UnitVerb::Restart))
    );
    for chord in ['a', 'd', 'c'] {
        assert!(
            !matches!(
                keymap.resolve(Buffer::Nix, Key::char(chord)),
                Some(Action::Nix(_))
            ),
            "`{chord}` was a Nix operation and is now a row in the transient"
        );
    }
    assert_eq!(
        keymap.resolve(Buffer::Nix, Key::char('e')),
        Some(Action::Transient),
        "which `e` opens"
    );

    // And they are this buffer's alone. In Procs the same two letters are
    // the sort keys they have always been, which is what a per-buffer
    // overlay is for - and why `no_two_default_bindings_collide` asks its
    // question per buffer.
    assert_eq!(
        keymap.resolve(Buffer::Procs, Key::char('a')),
        Some(Action::SortBy(Sort::Name))
    );
    assert_eq!(
        keymap.resolve(Buffer::Procs, Key::char('c')),
        Some(Action::SortBy(Sort::Cpu))
    );
    assert_eq!(keymap.resolve(Buffer::Procs, Key::char('d')), None);
}

/// And nowhere else - a unit verb in the IO buffer would be a key with
/// nothing to act on.
#[test]
fn a_unit_verb_does_not_resolve_in_a_buffer_with_no_units() {
    let keymap = Keymap::for_registry(Registry::new(BufferGates {
        declarative: true,
        packages: false,
    }));
    assert_eq!(keymap.resolve(Buffer::Io, Key::char('r')), None);
}

/// Rebinding a movement key moves the protection with it, since the
/// protected set is derived from what is bound.
#[test]
fn protection_follows_a_rebound_key() {
    let (keymap, _) = Keymap::with_overrides_for(
        Registry::default(),
        &[("move_down".to_string(), "z".to_string())],
    );
    assert!(
        keymap.is_protected(Key::char('z')),
        "z now moves, so no overlay may claim it"
    );
    assert!(
        !keymap.is_protected(Key::new(KeyCode::Down)),
        "and the arrow no longer does"
    );
}

/// An override onto a key some other action already holds is the one way
/// a user can shadow themselves. It is allowed - it is their keymap - but
/// the later binding must win rather than the two silently fighting.
#[test]
fn an_override_onto_an_occupied_key_takes_it() {
    let (keymap, problems) = Keymap::with_overrides_for(
        Registry::default(),
        &[("help".to_string(), "g".to_string())],
    );
    assert_eq!(
        keymap.resolve(Buffer::Status, Key::char('g')),
        Some(Action::Help),
        "the explicit choice wins"
    );
    // And the displaced default is unbound rather than silently shadowed,
    // with the operator told which key they have just lost.
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("refresh"), "{problems:?}");
    assert!(
        !keymap
            .base_action(Key::char('g'))
            .is_some_and(|a| a == Action::Refresh)
    );
}

/// The collision check spans scopes on purpose: a base binding is
/// protected, so it beats a buffer's overlay, and moving `next_section`
/// onto `]` would make the Procs nice key unreachable rather than merely
/// shadowed in one buffer.
#[test]
fn an_override_that_shadows_a_buffers_own_key_says_so() {
    let (_, problems) = Keymap::with_overrides_for(
        Registry::default(),
        &[("next_section".to_string(), "]".to_string())],
    );
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("nice_down"), "{problems:?}");
}

/// `Key` carried `alt` and not `ctrl`, so `convert` in the binary dropped
/// the modifier entirely and `ctrl-k` arrived as a bare `k` - which in the
/// Procs buffer opens the SIGTERM confirmation. Found in the audit that
/// produced the view-scoping change, deferred there because that change
/// needed no Ctrl binding, and fixed here.
#[test]
fn ctrl_is_not_the_same_key_as_the_bare_letter() {
    let keymap = Keymap::default();
    let bare = keymap.resolve(Buffer::Procs, Key::char('k'));
    assert_eq!(
        bare,
        Some(Action::Kill(masys_domain::service::Signal::Term)),
        "the bare letter still signals"
    );

    let held = keymap.resolve(Buffer::Procs, Key::ctrl(KeyCode::Char('k')));
    assert_eq!(held, None, "ctrl-k is a different key and binds to nothing");
}

/// The three modifier states are distinct keys, so a config can bind them
/// separately without one shadowing another.
#[test]
fn alt_and_ctrl_are_different_keys() {
    assert_ne!(Key::ctrl(KeyCode::Char('r')), Key::alt(KeyCode::Char('r')));
    assert_ne!(Key::ctrl(KeyCode::Char('r')), Key::char('r'));
}

/// A config file can name it, and the help can print it back.
#[test]
fn ctrl_round_trips_through_its_spelling() {
    let key = Key::parse("ctrl+r").expect("a parseable chord");
    assert_eq!(key, Key::ctrl(KeyCode::Char('r')));
    assert_eq!(key.spelling(), "ctrl+r");
    assert_eq!(
        Key::parse("Ctrl+r"),
        Some(Key::ctrl(KeyCode::Char('r'))),
        "case does not matter for the modifier"
    );
}

/// The footer, the `?` help and the digit itself are three answers to one
/// question - which buffers does this host have - and they have to be the
/// same answer.
///
/// The registry's own tests cannot catch a disagreement here: they ask the
/// registry, and the registry is right. What drifts is everything that
/// prints or resolves a digit *without* asking it, which is how a Debian
/// host came to advertise `[5] nix` in a footer where `5` did nothing.
#[test]
fn the_footer_and_the_help_list_exactly_the_buffers_the_digits_reach() {
    for (registry, keymap) in hosts() {
        let declarative = registry.contains(Buffer::Nix);
        let expected: Vec<String> = registry
            .specs()
            .iter()
            .filter_map(|spec| spec.key)
            .map(|key| key.to_string())
            .collect();
        let digits = |chords: Vec<String>| -> Vec<String> {
            chords
                .into_iter()
                .filter(|chord| chord.chars().all(|c| c.is_ascii_digit()))
                .collect()
        };

        let footer = digits(
            keymap
                .hints(Buffer::Status)
                .into_iter()
                .map(|hint| hint.chord)
                .collect(),
        );
        assert_eq!(
            footer, expected,
            "the footer offers a view this host does not have (declarative: {declarative})"
        );

        let help = digits(
            keymap
                .describe(Buffer::Status)
                .into_iter()
                .flat_map(|group| group.bindings)
                .map(|b| b.chord)
                .collect(),
        );
        assert_eq!(
            help, expected,
            "the ? help lists a view this host does not have (declarative: {declarative})"
        );

        let opens: Vec<String> = ('1'..='9')
            .filter(|d| {
                matches!(
                    keymap.resolve(Buffer::Status, Key::char(*d)),
                    Some(Action::Open(_))
                )
            })
            .map(|d| d.to_string())
            .collect();
        assert_eq!(
            opens, expected,
            "a digit opens a view this host does not have (declarative: {declarative})"
        );
    }
}

/// A host with no Nix buffer has no Nix keys anywhere in the keymap, not
/// merely no way to reach the buffer they sit in.
///
/// `Registry` is the one owner of which buffers a host has, and an overlay
/// for a view it does not have is a second answer to that question kept
/// somewhere else. It had no user-visible effect - nothing can put the
/// cursor in an unregistered buffer - which is exactly why it is worth
/// removing rather than leaving: the same shape, one buffer over, is how
/// a Debian host came to advertise `[5] nix` in a footer where `5` did
/// nothing.
#[test]
fn a_host_with_no_nix_buffer_carries_no_nix_keys_at_all() {
    for (registry, keymap) in hosts() {
        let declarative = registry.contains(Buffer::Nix);
        let nix_keys: Vec<String> = keymap
            .actions(Buffer::Nix)
            .into_iter()
            .filter(|binding| {
                matches!(
                    keymap.action_for(Buffer::Nix, &binding.chord),
                    Some(Action::Nix(_))
                )
            })
            .map(|binding| binding.chord)
            .collect();

        // Two verbs are direct now - `switch` and home-manager `switch`,
        // the only two live regardless of the row under the cursor and
        // reached for often enough to skip a popup entirely, magit's
        // `s`/`u` shape. Every other operation is a row behind one of the
        // five family menus, not a top-level key - the claim worth
        // keeping is the one about a host *without* the view at all,
        // which is what this test is named for.
        if declarative {
            assert_eq!(
                nix_keys,
                vec!["s".to_string(), "h".to_string()],
                "the direct Nix verbs: {nix_keys:?}"
            );
            assert_eq!(
                keymap.action_for(Buffer::Nix, "e"),
                Some(Action::Transient),
                "and unit rows still reach their popup through e"
            );
        } else {
            assert!(
                nix_keys.is_empty(),
                "a host with no Nix buffer built {nix_keys:?} into a Nix overlay"
            );
        }
    }
}

/// `action_for`'s doc says it answers "which action a chord runs". The
/// footer's dimming pass takes it at its word, asking it about every chord
/// `actions` offers - so if the claim is true it must answer for all of
/// them, and agree with `resolve`, which is what the key actually runs.
/// `chord_for` has no buffer parameter, so an answer it takes from a
/// per-buffer overlay is an answer to a question nobody asked - and the
/// overlays are a `HashMap`, so *which* overlay it reaches is randomised
/// per process. It must therefore answer from the base map alone, where
/// there is exactly one binding per action and no buffer to be wrong about.
#[test]
fn chord_for_answers_from_the_base_map_and_never_from_an_overlay() {
    for (_, keymap) in hosts() {
        for (name, action) in masys_app::keymap::ACTIONS {
            // `None` is legitimate - an action nothing binds, or one an
            // override displaced. The claim is only about what it does
            // answer: that chord must be a base binding for this action.
            let Some(chord) = keymap.chord_for(*action) else {
                continue;
            };
            let key = Key::parse(&chord).expect("a chord the footer prints parses");
            assert_eq!(
                keymap.base_action(key),
                Some(*action),
                "{name} is offered at {chord}, which the base map does not bind to it"
            );
        }
    }
}

#[test]
fn action_for_answers_for_every_chord_the_footer_offers() {
    for (_, keymap) in hosts() {
        for buffer in Buffer::ALL {
            for binding in keymap.actions(*buffer) {
                let key = Key::parse(&binding.chord).expect("a chord the footer offers parses");
                assert_eq!(
                    keymap.action_for(*buffer, &binding.chord),
                    keymap.resolve(*buffer, key),
                    "{buffer:?} offers {} and dims it by asking action_for",
                    binding.chord
                );
            }
        }
    }
}

/// And a config file that names one is still not an error. A single
/// config shared between a NixOS box and a Debian one is not wrong for
/// mentioning the Nix buffer; it is describing a view this host does not
/// have, which is why the override is dropped silently rather than
/// reported.
#[test]
fn rebinding_a_nix_key_on_a_host_without_the_buffer_is_dropped_not_refused() {
    let registry = Registry::new(BufferGates::NONE);
    let (keymap, problems) =
        Keymap::with_overrides_for(registry, &[("nix_clean".to_string(), "C".to_string())]);

    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(keymap.resolve(Buffer::Nix, Key::char('C')), None);
}
