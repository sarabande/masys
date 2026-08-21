//! Tests for `keymap_from`, the pure half of `load_keymap`.
//!
//! `App::with_declarative` asserts that the keymap's registry and the
//! declarative service agree, so every one of `load_keymap`'s five
//! real-world scenarios - no config file, an unreadable file,
//! unparseable TOML, no `[keys]` table, and a valid one - must hand back
//! a keymap built against the `Registry` it was given, never a fresh
//! default. A keymap built against the wrong registry means the Nix view
//! silently does not exist on a host that has one, which is what these
//! tests exist to catch.
//!
//! Four tests below, not five: `load_keymap`'s `.ok()` folds "no file"
//! and "a file that exists but could not be read" into the same `text:
//! None` before `keymap_from` ever sees either, so from here they are one
//! input, not two, and a second test asserting the same call with the
//! same arguments would pin nothing a first did not already.

use masys_app::Key;
use masys_app::buffer::{Buffer, Registry};
use masys_app::keymap::Action;

use super::keymap_from;

/// Whether the keymap `keymap_from` builds for `registry` and `text` has
/// the Nix view - the one fact every path below has to get right.
fn has_nix(registry: &Registry, text: Option<&str>) -> bool {
    let (keymap, _problems) = keymap_from(registry.clone(), "", text);
    keymap.registry().contains(Buffer::Nix)
}

/// Path 1: no config file at all, and a config file that exists but
/// could not be read - permissions, a symlink to nowhere, and so on.
/// `load_keymap`'s `.ok()` folds both into the same `text: None` before
/// `keymap_from` is called, so they are one path here, not two, and
/// pinned by one test.
#[test]
fn no_config_file_keeps_the_given_registry() {
    assert!(has_nix(&Registry::new(true), None));
    assert!(!has_nix(&Registry::new(false), None));
}

/// Path 2: the file exists and is not valid TOML. Reported, not fatal -
/// and still built against the given registry rather than a default.
#[test]
fn unparseable_toml_keeps_the_given_registry_and_reports_the_problem() {
    let (keymap, problems) = keymap_from(Registry::new(true), "/home/user/.config/masys/config.toml", Some("not [ valid toml"));
    assert!(keymap.registry().contains(Buffer::Nix));
    assert_eq!(problems.len(), 1, "one problem, naming the bad file: {problems:?}");
    assert!(problems[0].contains("config.toml"), "the problem should name the file: {problems:?}");

    let (keymap, _) = keymap_from(Registry::new(false), "/home/user/.config/masys/config.toml", Some("not [ valid toml"));
    assert!(!keymap.registry().contains(Buffer::Nix));
}

/// Path 3: valid TOML with no `[keys]` table - nothing to override, but
/// still the given registry.
#[test]
fn no_keys_table_keeps_the_given_registry() {
    assert!(has_nix(&Registry::new(true), Some("")));
    assert!(has_nix(&Registry::new(true), Some("[other]\nfoo = 1\n")));
    assert!(!has_nix(&Registry::new(false), Some("")));
}

/// Path 4: a valid `[keys]` table, applied on top of the given registry.
#[test]
fn a_valid_keys_table_keeps_the_given_registry() {
    assert!(has_nix(&Registry::new(true), Some("[keys]\nunit_restart = \"r\"\n")));
    assert!(!has_nix(&Registry::new(false), Some("[keys]\nunit_restart = \"r\"\n")));
}

/// A `view_nix` override rebinds the digit rather than being dropped -
/// the one override a NixOS operator is most likely to reach for, since
/// `5` is otherwise the only way to the view. `resolve` rather than
/// `Registry::by_key`: the registry's digit is the catalogue's fixed
/// one, and it is `Keymap::resolve` - what `app.handle_key` actually
/// calls - that has to move with the override.
#[test]
fn a_view_nix_override_rebinds_rather_than_drops() {
    let (keymap, problems) = keymap_from(Registry::new(true), "", Some("[keys]\nview_nix = \"6\"\n"));
    assert!(problems.is_empty(), "unexpected problems: {problems:?}");
    assert_eq!(keymap.resolve(Buffer::Status, Key::char('6')), Some(Action::Open(Buffer::Nix)));
    assert_ne!(keymap.resolve(Buffer::Status, Key::char('5')), Some(Action::Open(Buffer::Nix)));
}
