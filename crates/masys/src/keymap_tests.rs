//! Tests for `config_from`, the pure half of `load_config`.
//!
//! `App::with_declarative` asserts that the keymap's registry and the
//! declarative service agree, so every one of `load_config`'s five
//! real-world scenarios - no config file, an unreadable file,
//! unparseable TOML, no `[keys]` table, and a valid one - must hand back
//! a keymap built against the `Registry` it was given, never a fresh
//! default. A keymap built against the wrong registry means the Nix buffer
//! silently does not exist on a host that has one, which is what these
//! tests exist to catch.
//!
//! Four tests below, not five: `load_config`'s `.ok()` folds "no file"
//! and "a file that exists but could not be read" into the same `text:
//! None` before `config_from` ever sees either, so from here they are one
//! input, not two, and a second test asserting the same call with the
//! same arguments would pin nothing a first did not already.

use masys_app::Key;
use masys_app::buffer::{Buffer, BufferGates, Registry};
use masys_app::keymap::Action;
use masys_domain::finding::Thresholds;
use masys_render::theme::Theme;
use ratatui::style::Color;

use super::{Config, config_from};

/// The config `config_from` builds for `text` on a host that has the Nix
/// buffer - which is every test below that is not about the registry.
fn config(text: &str) -> Config {
    config_from(
        Registry::new(BufferGates {
            declarative: true,
            packages: false,
        }),
        "",
        Some(text),
    )
}

/// The thresholds `config_from` builds for `text`, and what it refused.
fn thresholds_from(text: &str) -> (Thresholds, Vec<String>) {
    let Config {
        thresholds,
        problems,
        ..
    } = config(text);
    (thresholds, problems)
}

/// The palette `config_from` builds for `text`, and what it refused.
fn theme_from(text: &str) -> (Theme, Vec<String>) {
    let Config {
        theme, problems, ..
    } = config(text);
    (theme, problems)
}

/// Whether the keymap `config_from` builds for `registry` and `text` has
/// the Nix buffer - the one fact every path below has to get right.
fn has_nix(registry: &Registry, text: Option<&str>) -> bool {
    config_from(registry.clone(), "", text)
        .keymap
        .registry()
        .contains(Buffer::Nix)
}

/// Path 1: no config file at all, and a config file that exists but
/// could not be read - permissions, a symlink to nowhere, and so on.
/// `load_config`'s `.ok()` folds both into the same `text: None` before
/// `config_from` is called, so they are one path here, not two, and
/// pinned by one test.
#[test]
fn no_config_file_keeps_the_given_registry() {
    assert!(has_nix(
        &Registry::new(BufferGates {
            declarative: true,
            packages: false
        }),
        None
    ));
    assert!(!has_nix(&Registry::new(BufferGates::NONE), None));
}

/// Path 2: the file exists and is not valid TOML. Reported, not fatal -
/// and still built against the given registry rather than a default.
#[test]
fn unparseable_toml_keeps_the_given_registry_and_reports_the_problem() {
    let Config {
        keymap, problems, ..
    } = config_from(
        Registry::new(BufferGates {
            declarative: true,
            packages: false,
        }),
        "/home/user/.config/masys/config.toml",
        Some("not [ valid toml"),
    );
    assert!(keymap.registry().contains(Buffer::Nix));
    assert_eq!(
        problems.len(),
        1,
        "one problem, naming the bad file: {problems:?}"
    );
    assert!(
        problems[0].contains("config.toml"),
        "the problem should name the file: {problems:?}"
    );

    let keymap = config_from(
        Registry::new(BufferGates::NONE),
        "/home/user/.config/masys/config.toml",
        Some("not [ valid toml"),
    )
    .keymap;
    assert!(!keymap.registry().contains(Buffer::Nix));
}

/// Path 3: valid TOML with no `[keys]` table - nothing to override, but
/// still the given registry.
#[test]
fn no_keys_table_keeps_the_given_registry() {
    assert!(has_nix(
        &Registry::new(BufferGates {
            declarative: true,
            packages: false
        }),
        Some("")
    ));
    assert!(has_nix(
        &Registry::new(BufferGates {
            declarative: true,
            packages: false
        }),
        Some("[other]\nfoo = 1\n")
    ));
    assert!(!has_nix(&Registry::new(BufferGates::NONE), Some("")));
}

/// Path 4: a valid `[keys]` table, applied on top of the given registry.
#[test]
fn a_valid_keys_table_keeps_the_given_registry() {
    assert!(has_nix(
        &Registry::new(BufferGates {
            declarative: true,
            packages: false
        }),
        Some("[keys]\nunit_restart = \"r\"\n")
    ));
    assert!(!has_nix(
        &Registry::new(BufferGates::NONE),
        Some("[keys]\nunit_restart = \"r\"\n")
    ));
}

/// A `buffer_nix` override rebinds the digit rather than being dropped -
/// the one override a NixOS operator is most likely to reach for, since
/// `5` is otherwise the only way to the buffer. `resolve` rather than
/// `Registry::by_key`: the registry's digit is the catalogue's fixed
/// one, and it is `Keymap::resolve` - what `app.handle_key` actually
/// calls - that has to move with the override.
#[test]
fn a_buffer_nix_override_rebinds_rather_than_drops() {
    let Config {
        keymap, problems, ..
    } = config("[keys]\nbuffer_nix = \"6\"\n");
    assert!(problems.is_empty(), "unexpected problems: {problems:?}");
    assert_eq!(
        keymap.resolve(Buffer::Status, Key::char('6')),
        Some(Action::Open(Buffer::Nix))
    );
    assert_ne!(
        keymap.resolve(Buffer::Status, Key::char('5')),
        Some(Action::Open(Buffer::Nix))
    );
}

/// No `[thresholds]` table is the ordinary case, and it evaluates against
/// exactly what the design names.
#[test]
fn no_thresholds_table_leaves_every_rule_at_its_default() {
    for text in ["", "[keys]\nunit_restart = \"r\"\n"] {
        let (thresholds, problems) = thresholds_from(text);
        assert_eq!(thresholds, Thresholds::default(), "for {text:?}");
        assert!(problems.is_empty(), "for {text:?}: {problems:?}");
    }
}

/// Every threshold the design names can be moved, and each is keyed by the
/// field it sets - one name for one number, with no mapping table between
/// what an operator writes and what triage reads.
#[test]
fn every_threshold_can_be_set_from_the_file() {
    let (thresholds, problems) = thresholds_from(
        "[thresholds]\n\
         flapping_restart_count = 7\n\
         flapping_window_ms = 900000\n\
         psi_some_avg60_percent = 35.5\n\
         psi_full_avg60_percent = 12\n\
         disk_used_percent = 95\n\
         inode_used_percent = 99.5\n\
         journal_window_ms = 300000\n\
         journal_rows_per_section = 3\n\
         thermal_throttled_percent = 25\n",
    );
    assert!(problems.is_empty(), "{problems:?}");

    // Destructured rather than read field by field, and without `..`.
    // This test is named `every_threshold`, and a hand-written list of
    // assertions cannot keep that promise: a threshold added to the
    // struct and forgotten here leaves the name true-sounding and false.
    // `journal_window_ms` and `journal_rows_per_section` were exactly
    // that - added to `Thresholds`, absent from the loader, and so
    // *rejected* by its catch-all, which made setting one an error at
    // startup. The compiler enforces the promise now.
    let Thresholds {
        flapping_restart_count,
        flapping_window_ms,
        psi_some_avg60_percent,
        psi_full_avg60_percent,
        disk_used_percent,
        inode_used_percent,
        journal_window_ms,
        journal_rows_per_section,
        thermal_throttled_percent,
    } = thresholds;
    assert_eq!(flapping_restart_count, 7);
    assert_eq!(flapping_window_ms, 900_000);
    assert_eq!(psi_some_avg60_percent, 35.5);
    // Written as a whole number, which is what somebody types. TOML makes
    // that an integer, and insisting on `12.0` would be the config file
    // having opinions about arithmetic.
    assert_eq!(psi_full_avg60_percent, 12.0);
    assert_eq!(disk_used_percent, 95.0);
    assert_eq!(inode_used_percent, 99.5);
    assert_eq!(journal_window_ms, 300_000);
    assert_eq!(journal_rows_per_section, 3);
    assert_eq!(thermal_throttled_percent, 25.0);
}

/// One bad value costs that threshold and nothing else - the rule `[keys]`
/// already follows. A typo should cost you the customisation, not the
/// check.
#[test]
fn a_refused_value_leaves_that_one_rule_at_its_default() {
    let (thresholds, problems) =
        thresholds_from("[thresholds]\ndisk_used_percent = 150\ninode_used_percent = 99\n");
    assert_eq!(
        thresholds.disk_used_percent,
        Thresholds::default().disk_used_percent,
        "the refused one keeps its default"
    );
    assert_eq!(
        thresholds.inode_used_percent, 99.0,
        "its neighbour is still applied"
    );
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("disk_used_percent"),
        "the problem names the key: {problems:?}"
    );
}

/// What masys will not take, and why each one matters.
///
/// A percentage outside 0 to 100 is refused rather than clamped: 150
/// clamped to 100 is a check that never fires, reported as a check that
/// was accepted. A count of zero would make every unit on the host a
/// finding the moment the file was saved.
#[test]
fn values_that_would_disable_a_check_are_refused_rather_than_taken() {
    for line in [
        "disk_used_percent = 150",
        "disk_used_percent = -1",
        "psi_some_avg60_percent = 101",
        "disk_used_percent = \"most of it\"",
        "flapping_restart_count = 0",
        "flapping_window_ms = 0",
        "flapping_restart_count = 2.5",
    ] {
        let (thresholds, problems) = thresholds_from(&format!("[thresholds]\n{line}\n"));
        assert_eq!(
            thresholds,
            Thresholds::default(),
            "`{line}` must change nothing"
        );
        assert_eq!(problems.len(), 1, "`{line}` must say why: {problems:?}");
    }
}

/// A misspelled key is reported rather than ignored.
///
/// Silently doing nothing would leave an operator believing they had
/// raised a threshold they had not - which they would discover the next
/// time the check they thought they had relaxed woke them up.
#[test]
fn an_unknown_threshold_is_reported() {
    let (thresholds, problems) = thresholds_from("[thresholds]\ndisk_used_percentage = 95\n");
    assert_eq!(thresholds, Thresholds::default());
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("disk_used_percentage"), "{problems:?}");
}

/// A file masys could not parse has said nothing about either table, and
/// both fall back together.
#[test]
fn unparseable_toml_leaves_the_thresholds_alone_too() {
    let (thresholds, problems) = thresholds_from("not [ valid toml");
    assert_eq!(thresholds, Thresholds::default());
    assert_eq!(problems.len(), 1, "one problem for one file: {problems:?}");
}

/// No `[theme]` table leaves the palette exactly as it is - including on
/// a file that has plenty else to say. A config file is a host with
/// something to say, not a feature being switched on.
#[test]
fn no_theme_table_leaves_every_colour_as_it_is() {
    for text in [
        "",
        "[keys]\nunit_restart = \"r\"\n",
        "[thresholds]\ndisk_used_percent = 90\n",
    ] {
        let (theme, problems) = theme_from(text);
        assert_eq!(theme, Theme::default(), "for {text:?}");
        assert!(problems.is_empty(), "for {text:?}: {problems:?}");
    }
}

/// Every colour masys draws with can be named, and each is keyed by the
/// field it sets - the rule `[thresholds]` follows, for the same reason.
///
/// The four spellings here are all the ones the README documents: a
/// plain name, a hyphenated one, a palette index and `#rrggbb`. A host
/// whose complaint is that `LightRed` is illegible is a host that has
/// redefined its palette, and the last two are how it says which entry
/// it actually means.
#[test]
fn every_colour_can_be_set_from_the_file() {
    let (theme, problems) = theme_from(
        "[theme]\n\
         section_header = \"blue\"\n\
         severity_dead = \"light-magenta\"\n\
         severity_warning = \"208\"\n\
         severity_urgent = \"#ff8800\"\n\
         info = \"gray\"\n\
         status_error = \"lightred\"\n",
    );
    assert!(problems.is_empty(), "{problems:?}");

    // Destructured rather than read field by field, and without `..`,
    // for `every_threshold_can_be_set_from_the_file`'s reason: this test
    // is named `every_colour`, and a hand-written list of assertions
    // cannot keep that promise. A seventh field added to `Theme` and
    // forgotten here would leave the name true-sounding and false, and
    // an operator with no way to move the colour it paints.
    let Theme {
        section_header,
        severity_dead,
        severity_warning,
        severity_urgent,
        info,
        status_error,
    } = theme;
    assert_eq!(section_header, Color::Blue);
    assert_eq!(severity_dead, Color::LightMagenta);
    assert_eq!(severity_warning, Color::Indexed(208));
    assert_eq!(severity_urgent, Color::Rgb(0xff, 0x88, 0x00));
    assert_eq!(info, Color::Gray);
    assert_eq!(status_error, Color::LightRed);
}

/// A table that names one colour moves that one. The other five are not
/// a palette this host chose, and blanking them to some default-for-a
/// -custom-theme would be masys deciding it knows better than the file.
#[test]
fn a_partial_theme_leaves_the_rest_of_the_palette() {
    let (theme, problems) = theme_from("[theme]\ninfo = \"white\"\n");
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(theme.info, Color::White);
    assert_eq!(
        theme,
        Theme {
            info: Color::White,
            ..Theme::default()
        }
    );
}

/// A colour masys cannot read costs that one slot and nothing else, and
/// the problem names it back - `theme.info: must be a colour` would
/// leave an operator re-reading the line they just typed.
#[test]
fn a_colour_masys_cannot_read_is_reported_and_leaves_that_slot_alone() {
    let (theme, problems) = theme_from("[theme]\ninfo = \"burgundy\"\nseverity_dead = \"cyan\"\n");
    assert_eq!(
        theme.info,
        Theme::default().info,
        "the refused one keeps its default"
    );
    assert_eq!(
        theme.severity_dead,
        Color::Cyan,
        "the other one still lands"
    );
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("info"), "{problems:?}");
    assert!(problems[0].contains("burgundy"), "{problems:?}");

    // A colour has to be written as one, too.
    let (theme, problems) = theme_from("[theme]\ninfo = 3\n");
    assert_eq!(theme, Theme::default());
    assert_eq!(problems.len(), 1, "{problems:?}");
}

/// An unknown slot is reported rather than ignored, for the reason a
/// misspelled threshold is: a `severity_urgant` that silently did nothing
/// would leave an operator believing they had recoloured the one signal
/// they most need to see, and they would find out the next time it
/// mattered.
#[test]
fn an_unknown_colour_slot_is_reported() {
    let (theme, problems) = theme_from("[theme]\nseverity_urgant = \"cyan\"\n");
    assert_eq!(theme, Theme::default());
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("severity_urgant"), "{problems:?}");
}

/// A file masys could not parse has said nothing about the palette
/// either, and all three tables fall back together.
#[test]
fn unparseable_toml_leaves_the_palette_alone_too() {
    let (theme, problems) = theme_from("not [ valid toml");
    assert_eq!(theme, Theme::default());
    assert_eq!(problems.len(), 1, "one problem for one file: {problems:?}");
}
