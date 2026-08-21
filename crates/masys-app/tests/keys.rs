use masys_app::key::{Key, KeyCode};

#[test]
fn a_bare_letter_parses_as_typed() {
    assert_eq!(Key::parse("n"), Some(Key::char('n')));
    // `K` and `k` are different keys and masys binds both, so case
    // matters for letters.
    assert_eq!(Key::parse("K"), Some(Key::char('K')));
    assert_ne!(Key::parse("K"), Key::parse("k"));
}

#[test]
fn named_keys_parse_case_insensitively() {
    for (text, code) in [
        ("tab", KeyCode::Tab),
        ("Tab", KeyCode::Tab),
        ("enter", KeyCode::Enter),
        ("return", KeyCode::Enter),
        ("esc", KeyCode::Esc),
        ("bksp", KeyCode::Backspace),
        ("pgdn", KeyCode::PageDown),
        ("pagedown", KeyCode::PageDown),
        ("home", KeyCode::Home),
        ("space", KeyCode::Char(' ')),
    ] {
        assert_eq!(Key::parse(text), Some(Key::new(code)), "{text}");
    }
}

#[test]
fn alt_is_a_prefix() {
    assert_eq!(Key::parse("alt+n"), Some(Key::alt(KeyCode::Char('n'))));
    assert_eq!(Key::parse("alt+down"), Some(Key::alt(KeyCode::Down)));
}

/// And so is ctrl, which `Key` could not represent at all until the
/// modifier turned out to be silently dropped - making `ctrl-k` arrive as
/// a bare `k`, straight into the SIGTERM confirmation.
#[test]
fn ctrl_is_a_prefix_too() {
    assert_eq!(Key::parse("ctrl+n"), Some(Key::ctrl(KeyCode::Char('n'))));
    assert_eq!(Key::parse("c-n"), Some(Key::ctrl(KeyCode::Char('n'))), "emacs spelling, which magit users will try");
    assert_eq!(Key::parse("ctrl+alt+delete".trim_end_matches("delete")), None, "an empty base is still not a key");
}

/// A typo in a config file should leave the default binding alone and say
/// so, not silently bind something else.
#[test]
fn an_unrecognised_key_is_rejected_rather_than_guessed() {
    assert_eq!(Key::parse("hyper+n"), None, "a modifier masys does not model is not a near-miss to guess at");
    assert_eq!(Key::parse("nope"), None);
    assert_eq!(Key::parse(""), None);
}

/// Every key masys can bind must survive a round trip through its config
/// spelling, or a written-out keymap would not parse back.
#[test]
fn every_spelling_parses_back_to_the_same_key() {
    let keys = [
        Key::char('n'),
        Key::char('K'),
        Key::char(' '),
        Key::new(KeyCode::Tab),
        Key::new(KeyCode::Enter),
        Key::new(KeyCode::Esc),
        Key::new(KeyCode::Backspace),
        Key::new(KeyCode::Up),
        Key::new(KeyCode::Down),
        Key::new(KeyCode::PageUp),
        Key::new(KeyCode::PageDown),
        Key::new(KeyCode::Home),
        Key::new(KeyCode::End),
        Key::alt(KeyCode::Char('n')),
    ];
    for key in keys {
        assert_eq!(Key::parse(&key.spelling()), Some(key), "{:?} spells {:?}", key, key.spelling());
    }
}

#[test]
fn surrounding_whitespace_is_ignored() {
    assert_eq!(Key::parse("  tab  "), Some(Key::new(KeyCode::Tab)));
}
