//! The definition itself, without a session around it. `tests/actions.rs`
//! drives the state machine; this pins what a definition *is*.

use masys_app::keymap::{Action, UnitVerb};
use masys_app::systemd_buffer::unit_transient;
use masys_app::transient::{DefGroup, DefRow, TransientDef};
use masys_domain::platform::Ownership;
use masys_view::{ModalView, SwitchRow};

fn def() -> TransientDef {
    TransientDef {
        title: "Unit . sshd.service".to_string(),
        switches: vec![
            SwitchRow { group: "Arguments", chord: "-v", label: "verbose", supported: true, on: false },
            SwitchRow { group: "Arguments", chord: "-i", label: "impure", supported: false, on: false },
        ],
        groups: vec![DefGroup {
            heading: "Persistence".to_string(),
            note: Some("! declared in nix".to_string()),
            rows: vec![
                DefRow { chord: 'e', label: "enable", note: None, dimmed: true, action: Action::Unit(UnitVerb::Enable) },
                DefRow { chord: 'l', label: "logs", note: None, dimmed: false, action: Action::Logs },
            ],
        }],
    }
}

/// The picture the renderer gets carries everything except the verb -
/// which is the whole point of the split. `masys_view::ActionRow` has no
/// field for an `Action` and masys-render has never heard of one.
#[test]
fn the_view_projects_every_field_but_the_action() {
    let ModalView::Transient { title, switches, groups } = def().view() else { panic!("a transient") };

    assert_eq!(title, "Unit . sshd.service");
    assert_eq!(switches.len(), 2);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].heading, "Persistence");
    assert_eq!(groups[0].note.as_deref(), Some("! declared in nix"));
    assert_eq!(
        groups[0].rows.iter().map(|row| (row.chord, row.label, row.dimmed)).collect::<Vec<_>>(),
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
    assert_eq!(def().action('z'), None, "and a chord nothing binds resolves to nothing");
}

/// A switch flips in place, and an unsupported one does not - it is
/// listed so the renderer can dim it, not so it can be turned on.
#[test]
fn a_supported_switch_toggles_and_an_unsupported_one_does_not() {
    let mut def = def();

    assert!(def.toggle('v'), "the switch is found by its chord's last character");
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
    let persistence = def.groups.iter().find(|group| group.heading == "Persistence").expect("a Persistence group");

    assert_eq!(persistence.note.as_deref(), Some("! could not read who owns this unit"));
    assert!(persistence.rows.iter().all(|row| row.dimmed), "{persistence:?}");
    assert!(persistence.rows.iter().all(|row| row.note.as_deref() == Some("persistence unknown")), "{persistence:?}");

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
    let chords: Vec<char> = def.groups.iter().flat_map(|group| &group.rows).map(|row| row.chord).collect();

    let mut seen = chords.clone();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), chords.len(), "a chord is used twice: {chords:?}");
}
