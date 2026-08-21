//! What a transient popup *is*, before anything decides how it looks.
//!
//! A buffer supplies a [`TransientDef`]: a plain value built by a free
//! function from whatever the popup depends on. Not a trait - there is no
//! buffer object here for one to attach to. Every buffer in this crate is
//! a free function over the one `App`, and choosing which implementation
//! to call would still be a `match` on `Buffer`, so a trait would pay its
//! cost and keep the match arm it was meant to remove. `crate::buffer`
//! makes the same argument for the registry.
//!
//! The `Action` rides on the row. `masys_view::ActionRow` cannot carry
//! one - it crosses to masys-render, which knows nothing about actions -
//! so a chord has to be turned back into a verb somewhere. Left beside
//! the group list rather than on it, that mapping becomes a second owner
//! of "which chords exist here", and the two disagree exactly where it is
//! worst: `crate::buffer` records the version of that mistake where a
//! Debian host's footer advertised `[5] nix` for a digit that did
//! nothing. So [`DefRow`] holds the verb, and the `Vec<ActionGroup>` the
//! renderer sees is a *projection* of the definition.

use masys_view::{ActionGroup, ActionRow, ModalView, SwitchRow};

use crate::keymap::Action;

/// A transient, as the buffer that owns it defines it.
///
/// Built fresh each time the popup opens rather than held and updated,
/// because every interesting thing about it is contextual: which
/// generation the cursor is on, whether the unit is declared in Nix.
/// A definition that outlived the state it was read from would offer
/// actions against a row that has since moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransientDef {
    pub title: String,
    pub switches: Vec<SwitchRow>,
    pub groups: Vec<DefGroup>,
}

/// One bucket of rows, with the group-level annotation the ownership
/// guard and the store policy both use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefGroup {
    pub heading: String,
    pub note: Option<String>,
    pub rows: Vec<DefRow>,
}

/// One row: what it looks like, and what it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefRow {
    pub chord: char,
    pub label: &'static str,
    pub note: Option<String>,
    /// Whether the row is drawn dim. Dim is a mark, never an omission:
    /// the row keeps its position and its chord still runs, which is why
    /// this does not gate [`TransientDef::action`].
    pub dimmed: bool,
    pub action: Action,
}

impl TransientDef {
    /// The picture of this definition, for the renderer.
    ///
    /// Borrows rather than consuming: a transient stays open across a
    /// switch toggle, so it is drawn many times and answered once.
    pub fn view(&self) -> ModalView<'_> {
        ModalView::Transient {
            title: self.title.clone(),
            switches: self.switches.clone(),
            groups: self
                .groups
                .iter()
                .map(|group| ActionGroup {
                    heading: group.heading.clone(),
                    note: group.note.clone(),
                    rows: group
                        .rows
                        .iter()
                        .map(|row| ActionRow { chord: row.chord, label: row.label, note: row.note.clone(), dimmed: row.dimmed })
                        .collect(),
                })
                .collect(),
        }
    }

    /// What this chord runs, if anything.
    ///
    /// A dimmed row answers like any other. Dimming says the effect will
    /// not persist, not that the key is dead - the design is explicit
    /// that such a row is "listed in position and still runnable behind a
    /// confirmation", and a lookup that skipped it would quietly make the
    /// mark mean something else.
    pub fn action(&self, chord: char) -> Option<Action> {
        self.groups.iter().flat_map(|group| &group.rows).find(|row| row.chord == chord).map(|row| row.action)
    }

    /// Flips the switch this chord names, and says whether it found one.
    ///
    /// A switch chord is written the way magit writes one - `-v` - and is
    /// pressed as its last character. Action rows are looked up first, so
    /// a switch sharing a letter with an action is unreachable; nothing
    /// defines switches yet, and the transient that first does owes its
    /// switches letters its actions do not use.
    pub fn toggle(&mut self, chord: char) -> bool {
        match self.switches.iter_mut().find(|switch| switch.chord.ends_with(chord) && switch.supported) {
            Some(switch) => {
                switch.on = !switch.on;
                true
            }
            None => false,
        }
    }
}
