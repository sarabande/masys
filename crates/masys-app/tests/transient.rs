//! The definition itself, without a session around it. `tests/actions.rs`
//! drives the state machine; this pins what a definition *is*.

use masys_app::keymap::{Action, UnitVerb};
use masys_app::systemd_buffer::{Persistence, ownership_caveat, unit_transient};
use masys_app::transient::{DefGroup, DefRow, TransientDef};
use masys_domain::platform::Ownership;
use masys_view::{ModalView, SwitchRow};

fn def() -> TransientDef {
    TransientDef {
        title: "Unit . sshd.service".to_string(),
        switches: vec![
            SwitchRow {
                group: "Arguments",
                chord: "-v",
                label: "verbose",
                supported: true,
                on: false,
            },
            SwitchRow {
                group: "Arguments",
                chord: "-i",
                label: "impure",
                supported: false,
                on: false,
            },
        ],
        groups: vec![DefGroup {
            heading: "Persistence".to_string(),
            note: Some("! declared in nix".to_string()),
            rows: vec![
                DefRow {
                    chord: 'e',
                    label: "enable",
                    note: None,
                    dimmed: true,
                    action: Action::Unit(UnitVerb::Enable),
                },
                DefRow {
                    chord: 'l',
                    label: "logs",
                    note: None,
                    dimmed: false,
                    action: Action::Logs,
                },
            ],
        }],
    }
}

/// The picture the renderer gets carries everything except the verb -
/// which is the whole point of the split. `masys_view::ActionRow` has no
/// field for an `Action` and masys-render has never heard of one.
#[test]
fn the_view_projects_every_field_but_the_action() {
    let ModalView::Transient {
        title,
        switches,
        groups,
    } = def().view()
    else {
        panic!("a transient")
    };

    assert_eq!(title, "Unit . sshd.service");
    assert_eq!(switches.len(), 2);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].heading, "Persistence");
    assert_eq!(groups[0].note.as_deref(), Some("! declared in nix"));
    assert_eq!(
        groups[0]
            .rows
            .iter()
            .map(|row| (row.chord, row.label, row.dimmed))
            .collect::<Vec<_>>(),
        vec![('e', "enable", true), ('l', "logs", false),]
    );
}

/// A dimmed row answers its chord like any other. Dimming says the effect
/// will not persist, not that the key is dead - the design is explicit
/// that such a row stays "listed in position and still runnable behind a
/// confirmation".
#[test]
fn a_dimmed_rows_chord_still_resolves() {
    assert_eq!(def().action('e'), Some(Action::Unit(UnitVerb::Enable)));
    assert_eq!(def().action('l'), Some(Action::Logs));
    assert_eq!(
        def().action('z'),
        None,
        "and a chord nothing binds resolves to nothing"
    );
}

/// A switch flips in place, and an unsupported one does not - it is
/// listed so the renderer can dim it, not so it can be turned on.
#[test]
fn a_supported_switch_toggles_and_an_unsupported_one_does_not() {
    let mut def = def();

    assert!(
        def.toggle('v'),
        "the switch is found by its chord's last character"
    );
    assert!(def.switches[0].on);
    assert!(def.toggle('v'), "and toggles back");
    assert!(!def.switches[0].on);

    assert!(!def.toggle('i'), "an unsupported switch reports no match");
    assert!(!def.switches[1].on, "and stays off");
    assert!(!def.toggle('q'), "a chord no switch names finds nothing");
}

/// The unit popup, built from what the platform actually said. A read
/// that *failed* is not a host that answered "nothing else owns this":
/// the rows are marked and say so, rather than promising a persistence
/// nobody checked.
#[test]
fn a_failed_ownership_read_marks_the_rows_rather_than_claiming_they_persist() {
    let def = unit_transient("sshd.service", None);
    let persistence = def
        .groups
        .iter()
        .find(|group| group.heading == "Persistence")
        .expect("a Persistence group");

    assert_eq!(
        persistence.note.as_deref(),
        Some("! could not read who owns this unit")
    );
    assert!(
        persistence.rows.iter().all(|row| row.dimmed),
        "{persistence:?}"
    );
    assert!(
        persistence
            .rows
            .iter()
            .all(|row| row.note.as_deref() == Some("persistence unknown")),
        "{persistence:?}"
    );

    // And it is not the same picture an imperative host draws, which is
    // what "these are different answers" means in practice.
    let imperative = unit_transient("sshd.service", Some(&Ownership::Imperative));
    assert_ne!(def.groups, imperative.groups);
}

/// Every chord in a transient must be distinct, or one row is a row no
/// key reaches. Asked of the popup that exists rather than of a rule
/// written down, so the next one to be built is checked too.
#[test]
fn no_two_rows_in_the_unit_popup_share_a_chord() {
    let def = unit_transient("sshd.service", Some(&Ownership::Imperative));
    let chords: Vec<char> = def
        .groups
        .iter()
        .flat_map(|group| &group.rows)
        .map(|row| row.chord)
        .collect();

    let mut seen = chords.clone();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen.len(),
        chords.len(),
        "a chord is used twice: {chords:?}"
    );
}

/// The footer's mark and the confirmation's caveat come from one reading,
/// and must agree about every state of it.
///
/// #12 was two readers of `unit_ownership` disagreeing on one host: the
/// popup said "persistence unknown" while the footer drew `D`, `M` and `U`
/// live. `Persistence` fixed those two; `ownership_caveat` is the third,
/// and this is what stops it drifting the same way.
///
/// The claim is a biconditional on purpose. A caveat with no mark is a
/// sentence about a key the footer says is fine; a mark with no caveat is
/// a dimmed key the confirmation never explains. Both are the same defect
/// wearing different clothes.
#[test]
fn a_caveat_appears_exactly_where_the_footer_marks() {
    let declarative = Ownership::Declarative {
        source: "/etc/nixos/configuration.nix".to_string(),
        note: "a rebuild reverts this".to_string(),
        reverted_by: "nixos-rebuild switch".to_string(),
    };
    let err = masys_domain::error::MasysError::Platform("no os-release on this host".to_string());

    let cases: [(&str, Result<&Ownership, &masys_domain::error::MasysError>); 3] = [
        ("a unit the configuration owns", Ok(&declarative)),
        ("a unit nothing else owns", Ok(&Ownership::Imperative)),
        ("a read that failed", Err(&err)),
    ];

    for (host, ownership) in cases {
        let marked = Persistence::of(ownership.ok()).marked();
        let caveat = ownership_caveat(ownership);
        assert_eq!(
            caveat.is_some(),
            marked,
            "on {host}, the confirmation and the footer disagree: caveat {caveat:?}, marked {marked}"
        );
    }
}

/// And the caveat says *why* where the popup only has room to say *that*.
///
/// The one thing this third caller adds: a failed read names the error,
/// where `unit_transient` has a row's width and says "persistence
/// unknown". Both are honest; only one fits in a popup.
#[test]
fn a_failed_read_names_its_error_in_the_confirmation() {
    let err = masys_domain::error::MasysError::Platform("no os-release on this host".to_string());
    let caveat = ownership_caveat(Err(&err)).expect("a failed read is always worth a caveat");

    assert!(
        caveat.contains("ownership unknown"),
        "the caveat must say the read failed: {caveat:?}"
    );
    assert!(
        caveat.contains("no os-release on this host"),
        "and must carry the reason, which is the whole point of taking the `Result`: {caveat:?}"
    );
}
