use masys_app::buffer::Buffer;
use masys_app::key::{Key, KeyCode};
use masys_app::keymap::{Action, Keymap, Sort};

/// No `hjkl` yet: which letters mean movement is unsettled, and binding
/// them would both prejudge that and spend four letters from the
/// namespace reserved for contextual actions. Arrows prejudge nothing.
#[test]
fn the_default_keymap_moves_with_arrows_and_binds_no_movement_letters() {
    let keymap = Keymap::default();
    for letter in ['h', 'j', 'k', 'l'] {
        let action = keymap.resolve(Buffer::Status, Key::char(letter));
        assert!(
            !matches!(action, Some(Action::MoveDown) | Some(Action::MoveUp) | Some(Action::PageDown) | Some(Action::PageUp)),
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
                let base = keymap.base_action(key).expect("a protected key is bound in the base map");
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
    for key in [Key::new(KeyCode::Down), Key::new(KeyCode::Up), Key::char('q'), Key::char('?'), Key::char('g')] {
        assert!(arrows.is_protected(key), "{key:?} should be protected");
    }
    // A letter bound only by an overlay is not protected, so the buffer
    // that owns it keeps it.
    assert!(!arrows.is_protected(Key::char('m')), "m is the Procs memory sort, not global navigation");
}

/// The rule this change is built on: a key is global when it means the
/// same thing in every view. So every global must resolve identically in
/// all of them - if one view disagrees, it is not a global, it is a view
/// key wearing a global's clothes.
#[test]
fn every_global_means_the_same_thing_in_every_view() {
    let keymap = Keymap::default();
    for key in globals() {
        let meaning = keymap.base_action(key).unwrap_or_else(|| panic!("{key:?} is listed global but bound to nothing"));
        for buffer in Buffer::ALL {
            assert_eq!(keymap.resolve(*buffer, key), Some(meaning), "{key:?} means something else in {buffer:?}");
        }
    }
}

/// `/` and `tab` were global in behaviour but absent from the protected
/// set, so an overlay could quietly take either. Under the rule they are
/// globals like any other and no view may claim them.
#[test]
fn the_protected_set_is_exactly_the_globals() {
    let keymap = Keymap::default();
    for key in globals() {
        assert!(keymap.is_protected(key), "{key:?} is global and must be protected");
    }
    for key in [Key::char('r'), Key::char('c'), Key::char('m'), Key::char('l')] {
        assert!(!keymap.is_protected(key), "{key:?} belongs to a view, not to everyone");
    }
}

/// Views move to digits so the five letters they used to hold - `s t u j
/// d` - go back to the views themselves. Those five were never navigation
/// *within* a view; they were a menu, and a menu is what digits are for.
#[test]
fn a_digit_opens_its_view_and_no_bare_letter_does() {
    let keymap = Keymap::default();
    for (index, spec) in masys_app::buffer::BUFFERS.iter().enumerate() {
        let _ = index;
        let Some(digit) = spec.key else { continue };
        assert_eq!(keymap.resolve(Buffer::Status, Key::char(digit)), Some(Action::Open(spec.buffer)), "{digit} opens {:?}", spec.buffer);
    }
    for letter in ['s', 't', 'u', 'j', 'd'] {
        assert!(
            !matches!(keymap.resolve(Buffer::Status, Key::char(letter)), Some(Action::Open(_))),
            "`{letter}` still opens a view; it belongs to the views now"
        );
    }
}

/// Section jumping is the clearest case the rule exists for: a letter,
/// and global anyway, because it is the same act everywhere.
#[test]
fn section_jumping_stays_global_because_it_means_one_thing() {
    let keymap = Keymap::default();
    for buffer in Buffer::ALL {
        assert_eq!(keymap.resolve(*buffer, Key::char('n')), Some(Action::NextSection), "{buffer:?}");
        assert_eq!(keymap.resolve(*buffer, Key::char('p')), Some(Action::PrevSection), "{buffer:?}");
    }
}

/// The globals, as the design doc lists them.
fn globals() -> Vec<Key> {
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
    // The digits actually in the ring - the log view has none.
    keys.extend(masys_app::buffer::BUFFERS.iter().filter_map(|spec| spec.key).map(Key::char));
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
        assert_eq!(keymap.resolve(Buffer::Procs, key), None, "{key:?} still binds something in procs");
    }
    for name in ["collapse_all", "expand_all"] {
        assert!(masys_app::keymap::ACTIONS.iter().all(|(known, _)| *known != name), "`{name}` is still configurable");
    }
}

/// Every key an overlay might plausibly try to claim.
fn every_key() -> Vec<Key> {
    let mut keys: Vec<Key> = ('a'..='z').chain('A'..='Z').map(Key::char).collect();
    keys.extend([Key::char('?'), Key::new(KeyCode::Down), Key::new(KeyCode::Up), Key::new(KeyCode::Tab)]);
    keys
}

/// "Each view has its own actions" - and the help says which are which.
#[test]
fn each_buffer_has_its_own_bindings_and_they_are_described() {
    let keymap = Keymap::default();
    assert_eq!(keymap.resolve(Buffer::Procs, Key::char('c')), Some(Action::SortBy(Sort::Cpu)));
    assert_eq!(keymap.resolve(Buffer::Procs, Key::char('m')), Some(Action::SortBy(Sort::Memory)));
    // `a` for alphabetical: `n` is next-section now.
    assert_eq!(keymap.resolve(Buffer::Procs, Key::char('a')), Some(Action::SortBy(Sort::Name)));
    assert_eq!(keymap.resolve(Buffer::Status, Key::char('c')), None, "a Procs key does not leak into Status");

    let headings: Vec<String> = keymap.describe(Buffer::Procs).into_iter().map(|g| g.heading).collect();
    assert_eq!(headings, vec!["Movement", "Global", "Buffers", "Procs only"]);
}

/// Status is triage and has no verbs of its own, so the help shows it the
/// global keys and nothing else - no empty "Status only" heading, the
/// same rule every buffer follows for a section with nothing in it.
#[test]
fn a_buffer_with_no_actions_of_its_own_gets_no_section() {
    let keymap = Keymap::default();
    let headings: Vec<String> = keymap.describe(Buffer::Status).into_iter().map(|g| g.heading).collect();
    assert_eq!(headings, vec!["Movement", "Global", "Buffers"]);
    assert!(keymap.actions(Buffer::Status).iter().all(|b| b.label == "filter"), "only the global filter");
}

/// `T` was the top-processes toggle and is now bound to nothing, in any
/// buffer - the rankings it hid are gone with it.
#[test]
fn the_top_processes_toggle_is_gone() {
    let keymap = Keymap::default();
    for buffer in Buffer::ALL {
        assert_eq!(keymap.resolve(*buffer, Key::char('T')), None, "{buffer:?} still binds T");
    }
    assert!(masys_app::keymap::ACTIONS.iter().all(|(name, _)| *name != "toggle_top_processes"), "still configurable");
}

/// The footer lists every view, with the one you are in marked.
///
/// It used to omit the current view, which made the row shorter but also
/// made it move: the same key sat in a different place depending on where
/// you were, so the footer could not be read by position. Listing all of
/// them keeps the row fixed and turns it into a map - and the marked
/// entry is the only thing that changes as you move.
#[test]
fn the_footer_lists_every_view_and_marks_the_current_one() {
    for buffer in Buffer::ALL {
        let hints = Keymap::default().hints(*buffer);
        let views: Vec<&masys_view::KeyBinding> = hints.iter().filter(|h| h.chord.chars().all(|c| c.is_ascii_digit())).collect();
        let in_ring = masys_app::buffer::BUFFERS.iter().filter(|s| s.key.is_some()).count();
        assert_eq!(views.len(), in_ring, "every view in the ring is listed in {buffer:?}");

        // The log view is a drill-down and not in the ring, so standing in
        // it marks nothing - which is honest: no digit would take you back.
        let active: Vec<&str> = views.iter().filter(|h| h.active).map(|h| h.label.as_str()).collect();
        let expected = usize::from(*buffer != Buffer::Log);
        assert_eq!(active.len(), expected, "in {buffer:?}: {active:?}");
        if expected == 1 {
            assert_eq!(active[0], buffer.title().to_lowercase(), "and it is the one you are in");
        }
    }
}

/// The global verbs are not views and are never marked.
#[test]
fn the_global_verbs_are_never_marked_current() {
    let hints = Keymap::default().hints(Buffer::Status);
    for verb in ["g", "?", "q"] {
        let hint = hints.iter().find(|h| h.chord == verb).unwrap_or_else(|| panic!("{verb} missing"));
        assert!(!hint.active, "{verb} is marked as a view");
    }
}

/// View switching is on digits, so every bare letter stays available for
/// whatever the row under the cursor supports.
#[test]
fn a_key_that_names_no_view_resolves_to_nothing() {
    let keymap = Keymap::default();
    assert_eq!(keymap.resolve_buffer(Key::char('2')), Some(Buffer::Procs));
    assert_eq!(keymap.resolve_buffer(Key::char('1')), Some(Buffer::Status));
    assert_eq!(keymap.resolve_buffer(Key::char('z')), None);
    // And the letters that used to: they belong to the views now.
    assert_eq!(keymap.resolve_buffer(Key::char('t')), None);
    assert_eq!(keymap.resolve_buffer(Key::char('s')), None);
}

/// `b` was the buffer prefix and is now free. It must mean nothing
/// rather than lingering as a key that silently swallows the next press.
#[test]
fn b_is_unbound_now_that_the_prefix_is_gone() {
    let keymap = Keymap::default();
    assert_eq!(keymap.resolve(Buffer::Status, Key::char('b')), None);
    assert!(!keymap.is_protected(Key::char('b')), "an overlay may claim it");
}

#[test]
fn an_unbound_key_resolves_to_nothing() {
    assert_eq!(Keymap::default().resolve(Buffer::Status, Key::char('Z')), None);
}

/// The footer is the only place most keys are ever read from, so a
/// footer that prints a table rather than the live keymap advertises
/// keys that no longer do anything. Rebinding is the whole feature.
#[test]
fn the_footer_names_the_key_that_is_actually_bound() {
    // `G` rather than `R`, which reload now holds: a global taking a
    // letter a view already owns is a real conflict, and the keymap
    // reports it rather than swallowing it - which is a different test's
    // subject, not this one's.
    let (keymap, problems) = Keymap::with_overrides(&[("refresh".to_string(), "G".to_string())]);
    assert!(problems.is_empty(), "{problems:?}");

    let hints = keymap.hints(Buffer::Status);
    let refresh = hints.iter().find(|h| h.label == "refresh").expect("a refresh hint");
    assert_eq!(refresh.chord, "G");
    assert!(!hints.iter().any(|h| h.chord == "g"), "and nothing still offers the old key: {hints:?}");
}

/// The same rule for a view jump, which reaches the footer through the
/// registry rather than through the keymap and so had its own copy of
/// the key.
#[test]
fn the_footer_names_a_rebound_view_jump() {
    let (keymap, problems) = Keymap::with_overrides(&[("view_io".to_string(), "L".to_string())]);
    assert!(problems.is_empty(), "{problems:?}");

    let hints = keymap.hints(Buffer::Status);
    let io = hints.iter().find(|h| h.label == "io").expect("an io hint");
    assert_eq!(io.chord, "L");
}

/// And the help, which must agree with the footer - two places that
/// disagree about a key are worse than one that is merely wrong.
#[test]
fn the_help_names_the_key_that_is_actually_bound() {
    let (keymap, _) = Keymap::with_overrides(&[("view_io".to_string(), "L".to_string())]);
    let chords: Vec<String> = keymap.describe(Buffer::Status).into_iter().flat_map(|g| g.bindings).map(|b| b.chord).collect();
    assert!(chords.contains(&"L".to_string()), "{chords:?}");
    assert!(!chords.contains(&"4".to_string()), "the old digit is gone: {chords:?}");
}

/// The `?` popup is the only discovery mechanism masys has. A key that is
/// bound but unlisted is a key nobody finds - which is what happened to
/// `/`, `n` and `p`, all of them added after the groups were written.
#[test]
fn the_help_lists_every_key_the_base_map_binds() {
    let keymap = Keymap::default();
    let listed: Vec<String> = keymap.describe(Buffer::Status).into_iter().flat_map(|g| g.bindings).map(|b| b.chord).collect();

    for (name, action) in masys_app::keymap::ACTIONS {
        // Per-buffer actions live in the open buffer's own group and are
        // covered by the overlay test below.
        if masys_app::keymap::Keymap::default().base_action(key_for(*action)).is_none() {
            continue;
        }
        assert!(
            listed.contains(&key_for(*action).spelling()),
            "`{name}` is bound to `{}` but the help never lists it",
            key_for(*action).spelling()
        );
    }
}

/// The default binding for an action, as the table spells it.
fn key_for(action: Action) -> Key {
    let name = action.name().expect("every action has a config name");
    let spelling = masys_app::keymap::DEFAULT_BINDINGS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, k)| *k)
        .unwrap_or_else(|| panic!("`{name}` has no default binding"));
    Key::parse(spelling).expect("a parseable default")
}

/// A buffer missing from the registry compiles fine and is simply
/// unreachable - no key opens it, and its title falls back to something
/// shared, so its cursor collides with every other unregistered buffer's.
/// That is exactly the bug this test exists to catch, having already
/// happened once.
#[test]
fn every_buffer_is_registered_and_reachable() {
    let keymap = Keymap::default();
    for buffer in Buffer::ALL {
        let spec = masys_app::buffer::BUFFERS
            .iter()
            .find(|spec| spec.buffer == *buffer)
            .unwrap_or_else(|| panic!("{buffer:?} has no BUFFERS entry, so no key can reach it"));
        // The log view has no digit: it is a drill-down, reached by `l`
        // from a unit and left by `esc`.
        let Some(key) = spec.key else { continue };
        assert_eq!(keymap.resolve_buffer(Key::char(key)), Some(*buffer), "{key} must open {buffer:?}");
    }
}

/// Two buffers on one letter would make the second unreachable, and the
/// registry is ordered so the first would silently win.
#[test]
fn no_two_buffers_share_a_key() {
    let mut keys: Vec<char> = masys_app::buffer::BUFFERS.iter().filter_map(|spec| spec.key).collect();
    let before = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), before, "duplicate buffer keys: {keys:?}");
}

/// Titles are the identity for per-buffer cursor state, so a duplicate
/// would make two buffers share one cursor.
#[test]
fn no_two_buffers_share_a_title() {
    let mut titles: Vec<&str> = masys_app::buffer::BUFFERS.iter().map(|spec| spec.title).collect();
    let before = titles.len();
    titles.sort_unstable();
    titles.dedup();
    assert_eq!(titles.len(), before, "duplicate buffer titles: {titles:?}");
}

/// Single-letter jumps - the only way to switch buffers.
#[test]
fn a_bare_letter_opens_its_buffer() {
    let keymap = Keymap::default();
    for spec in masys_app::buffer::BUFFERS {
        let Some(key) = spec.key else { continue };
        assert_eq!(keymap.resolve(Buffer::Status, Key::char(key)), Some(Action::Open(spec.buffer)), "{key} opens {:?}", spec.buffer);
        assert_eq!(keymap.resolve_buffer(Key::char(key)), Some(spec.buffer));
    }
}

/// Navigation letters are global, so no buffer's overlay may quietly
/// repurpose one - the same rule that protects `q` and `g`.
#[test]
fn buffer_letters_are_protected_from_overlays() {
    let keymap = Keymap::default();
    for spec in masys_app::buffer::BUFFERS {
        let Some(key) = spec.key else { continue };
        assert!(keymap.is_protected(Key::char(key)), "{key} must be protected");
        for buffer in Buffer::ALL {
            assert_eq!(keymap.resolve(*buffer, Key::char(key)), Some(Action::Open(spec.buffer)), "{buffer:?} must not shadow {key}");
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
                    !masys_app::buffer::BUFFERS.iter().any(|spec| spec.chord().as_deref() == Some(binding.chord.as_str())),
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
                    let (Some(c), None) = (chars.next(), chars.next()) else { continue };
                    let action = keymap.resolve(*buffer, Key::char(c));
                    assert!(
                        !matches!(action, Some(Action::MoveDown) | Some(Action::MoveUp) | Some(Action::PageDown) | Some(Action::PageUp)),
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
    let keymap = Keymap::default();
    for spec in masys_app::buffer::BUFFERS {
        let bound = masys_app::keymap::DEFAULT_BINDINGS
            .iter()
            .find(|(name, _)| Action::from_name(name) == Some(Action::Open(spec.buffer)))
            .map(|(_, key)| *key);
        match spec.key {
            Some(key) => {
                let bound = bound.unwrap_or_else(|| panic!("{:?} has no default binding", spec.buffer));
                assert_eq!(bound, key.to_string(), "{:?}: registry says {key}", spec.buffer);
                assert_eq!(keymap.resolve_buffer(Key::char(key)), Some(spec.buffer));
            }
            // A view outside the ring must have no binding either, or a
            // key would reach it and the registry would not say which.
            None => assert_eq!(bound, None, "{:?} is not in the ring but is bound to {bound:?}", spec.buffer),
        }
    }
}

/// Every action in the table must have a default binding, or it is an
/// action nothing can reach.
#[test]
fn every_named_action_has_a_default_binding() {
    for (name, _) in masys_app::keymap::ACTIONS {
        assert!(
            masys_app::keymap::DEFAULT_BINDINGS.iter().any(|(bound, _)| bound == name),
            "`{name}` is configurable but has no default binding"
        );
    }
}

/// And no two actions may default to the same key in the same scope,
/// which would make one of them unreachable.
#[test]
fn no_two_default_bindings_collide() {
    let keymap = Keymap::default();
    for buffer in Buffer::ALL {
        let mut seen: Vec<(String, &str)> = Vec::new();
        for (name, key) in masys_app::keymap::DEFAULT_BINDINGS {
            let Some(action) = Action::from_name(name) else { continue };
            // Only what actually resolves in this buffer can collide here.
            if keymap.resolve(*buffer, Key::parse(key).expect("a parseable default")) != Some(action) {
                continue;
            }
            if let Some((_, other)) = seen.iter().find(|(k, _)| k == key) {
                panic!("{buffer:?}: `{key}` is both `{other}` and `{name}`");
            }
            seen.push(((*key).to_string(), name));
        }
    }
}

/// The point of configurability: any action can be moved to any key.
#[test]
fn an_override_rebinds_by_action_name() {
    let (keymap, problems) = Keymap::with_overrides(&[("next_section".to_string(), "z".to_string())]);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(keymap.resolve(Buffer::Status, Key::char('z')), Some(Action::NextSection));
    assert_eq!(keymap.resolve(Buffer::Status, Key::char('n')), None, "the default is vacated, not kept alongside");
}

/// A typo should cost you the customisation, not the key: the default
/// stays and the problem is reported, rather than the action becoming
/// unreachable.
#[test]
fn a_bad_key_leaves_the_default_in_place_and_says_so() {
    // `hyper+n` rather than `ctrl+n`, which this used to name: ctrl was
    // unparseable only because `Key` could not represent it, and that was
    // the bug, not the example.
    let (keymap, problems) = Keymap::with_overrides(&[("next_section".to_string(), "hyper+n".to_string())]);
    assert_eq!(keymap.resolve(Buffer::Status, Key::char('n')), Some(Action::NextSection), "still bound");
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("hyper+n"), "{problems:?}");
}

/// A misspelled action name is reported rather than silently ignored -
/// otherwise a config that does nothing looks exactly like one that works.
#[test]
fn an_unknown_action_name_is_reported() {
    let (_, problems) = Keymap::with_overrides(&[("restart_everything".to_string(), "z".to_string())]);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("restart_everything"), "{problems:?}");
}

/// Per-buffer actions are configurable too, and stay in their buffer.
#[test]
fn a_per_buffer_action_can_be_rebound() {
    let (keymap, problems) = Keymap::with_overrides(&[("unit_restart".to_string(), "z".to_string())]);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(keymap.resolve(Buffer::Systemd, Key::char('z')), Some(Action::Unit(masys_app::keymap::UnitVerb::Restart)));
    assert_eq!(keymap.resolve(Buffer::Procs, Key::char('z')), None, "still only in its own buffer");
}

/// Rebinding a movement key moves the protection with it, since the
/// protected set is derived from what is bound.
#[test]
fn protection_follows_a_rebound_key() {
    let (keymap, _) = Keymap::with_overrides(&[("move_down".to_string(), "z".to_string())]);
    assert!(keymap.is_protected(Key::char('z')), "z now moves, so no overlay may claim it");
    assert!(!keymap.is_protected(Key::new(KeyCode::Down)), "and the arrow no longer does");
}

/// An override onto a key some other action already holds is the one way
/// a user can shadow themselves. It is allowed - it is their keymap - but
/// the later binding must win rather than the two silently fighting.
#[test]
fn an_override_onto_an_occupied_key_takes_it() {
    let (keymap, problems) = Keymap::with_overrides(&[("help".to_string(), "g".to_string())]);
    assert_eq!(keymap.resolve(Buffer::Status, Key::char('g')), Some(Action::Help), "the explicit choice wins");
    // And the displaced default is unbound rather than silently shadowed,
    // with the operator told which key they have just lost.
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("refresh"), "{problems:?}");
    assert!(!keymap.base_action(Key::char('g')).is_some_and(|a| a == Action::Refresh));
}

/// The collision check spans scopes on purpose: a base binding is
/// protected, so it beats a buffer's overlay, and moving `next_section`
/// onto `]` would make the Procs nice key unreachable rather than merely
/// shadowed in one buffer.
#[test]
fn an_override_that_shadows_a_buffers_own_key_says_so() {
    let (_, problems) = Keymap::with_overrides(&[("next_section".to_string(), "]".to_string())]);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("nice_down"), "{problems:?}");
}

/// `Key` carried `alt` and not `ctrl`, so `convert` in the binary dropped
/// the modifier entirely and `ctrl-k` arrived as a bare `k` - which in the
/// Procs view opens the SIGTERM confirmation. Found in the audit that
/// produced the view-scoping change, deferred there because that change
/// needed no Ctrl binding, and fixed here.
#[test]
fn ctrl_is_not_the_same_key_as_the_bare_letter() {
    let keymap = Keymap::default();
    let bare = keymap.resolve(Buffer::Procs, Key::char('k'));
    assert_eq!(bare, Some(Action::Kill(masys_domain::service::Signal::Term)), "the bare letter still signals");

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
    assert_eq!(Key::parse("Ctrl+r"), Some(Key::ctrl(KeyCode::Char('r'))), "case does not matter for the modifier");
}
