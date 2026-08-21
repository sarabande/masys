//! The systemd view: every unit systemd knows, grouped by what kind of
//! thing it is.
//!
//! Grouped by *type* rather than by state. State grouping answered "what
//! is wrong and what is running", which sounds right and turns out to be
//! the status buffer's question, not this one's - and it collapsed 366
//! units into three sections, two of which (`Active`, `Inactive`) said
//! nothing about what the units in them were. A type is what you look a
//! unit up by: sockets are one subject, mounts another, and a section per
//! subject is what makes 366 rows navigable.
//!
//! Failure has not stopped mattering; it stopped being a *section*. A
//! failed unit sorts to the top of its own type and keeps its `x` glyph,
//! and the status view still leads with failures, which is its job.

use masys_domain::platform::Ownership;
use masys_domain::unit::{ActiveState, Unit, UnitDetail, UnitKind};
use masys_view::{Node, SectionKind};
use std::collections::HashSet;

use crate::keymap::{Action, UnitVerb};
use crate::transient::{DefGroup, DefRow, TransientDef};

/// What the session knows about the open units.
///
/// A trait rather than a concrete map so this module stays free of the
/// session's own type, and so a test can build rows without one.
pub trait Expanded {
    fn is_open(&self, unit: &str) -> bool;
    fn detail(&self, unit: &str) -> Option<UnitDetail>;
}

/// The plain "nothing is open" answer, which is also what every test that
/// is not about expansion wants.
impl Expanded for HashSet<String> {
    fn is_open(&self, unit: &str) -> bool {
        self.contains(unit)
    }
    fn detail(&self, _unit: &str) -> Option<UnitDetail> {
        None
    }
}

/// Worst first within a section, so a broken unit is the first thing under
/// its heading rather than somewhere in an alphabetical list of 138.
///
/// Ordering by state and then by name is stable between ticks in the way
/// the buffer needs: a unit only moves when it actually changes state,
/// where ordering by anything live - a rate, a timestamp - would shuffle
/// rows under the cursor every two seconds.
///
/// `pub` so the Nix view's own section of units sorts by the same rule.
/// Two orderings for "a list of units" would be two habits to learn.
pub fn severity(state: ActiveState) -> u8 {
    match state {
        ActiveState::Failed => 0,
        ActiveState::Activating => 1,
        ActiveState::Deactivating => 2,
        ActiveState::Reloading => 3,
        ActiveState::Active => 4,
        ActiveState::Inactive => 5,
    }
}

/// One section per unit type present, in `UnitKind::ORDER`.
///
/// Empty types are hidden, the same rule every other buffer follows: a
/// heading that is usually empty stops being read. On a host with no swap
/// there is no `Swap` heading to wonder about.
pub fn build_unit_rows<E: Expanded>(
    units: &[Unit],
    collapsed: &HashSet<String>,
    expanded: &E,
    open_slices: &HashSet<String>,
    now_ms: u64,
) -> Vec<Node> {
    let failed: HashSet<&str> = units.iter().filter(|u| u.active_state == ActiveState::Failed).map(|u| u.name.as_str()).collect();
    let mut rows = Vec::new();

    for kind in UnitKind::ORDER {
        let mut members: Vec<&Unit> = units.iter().filter(|unit| unit.kind == *kind).collect();
        if members.is_empty() {
            continue;
        }
        members.sort_by(|a, b| severity(a.active_state).cmp(&severity(b.active_state)).then_with(|| a.name.cmp(&b.name)));

        let title = kind.plural();
        rows.push(Node::SectionHeader { title: title.to_string(), kind: SectionKind::Units, count: Some(members.len() as u32) });

        // Through `fold_key` rather than straight past it, even though
        // it hands back the title for this kind: the key a builder looks
        // up and the key `App::cycle_section` inserts have to come from
        // one place, which is the property the Nix view's folds lost by
        // deriving them separately.
        if !collapsed.contains(&SectionKind::Units.fold_key(title)) {
            match kind {
                // A timer's own state answers the wrong question - it is
                // `active` while the job it runs is broken - so its row
                // carries the schedule and the health of what it
                // activates instead.
                UnitKind::Timer => rows.extend(timer_rows(&members, units, &failed, open_slices, expanded, now_ms)),
                // Slices are systemd's containment hierarchy, so they
                // nest rather than list: a slice holds child slices and
                // the units assigned to it, several levels deep.
                UnitKind::Slice => {
                    let roots: Vec<&&Unit> =
                        members.iter().filter(|slice| !members.iter().any(|other| Some(&other.name) == slice.slice.as_ref())).collect();
                    for root in roots {
                        rows.extend(slice_tree(root, units, open_slices, expanded, now_ms, 0));
                    }
                }
                _ => {
                    for unit in &members {
                        rows.extend(triggered_rows(unit, units, open_slices, expanded, now_ms, 0));
                    }
                }
            }
        }
        rows.push(Node::Spacer);
    }

    if matches!(rows.last(), Some(Node::Spacer)) {
        rows.pop();
    }
    rows
}

/// The `.timer` units and what they run.
///
/// Soonest first: the next thing to happen is the useful ordering, and a
/// timer with no next elapse sorts last because it will not happen. This
/// is the one section not ordered worst-first, because a timer's own state
/// is not where its trouble shows - `activated_failed` is.
fn timer_rows<E: Expanded>(
    timers: &[&Unit],
    units: &[Unit],
    failed: &HashSet<&str>,
    open_subtrees: &HashSet<String>,
    expanded: &E,
    now_ms: u64,
) -> Vec<Node> {
    let mut timers: Vec<&&Unit> = timers.iter().collect();
    timers.sort_by_key(|u| (u.timer.as_ref().and_then(|t| t.next_ms).unwrap_or(u64::MAX), u.name.clone()));

    let mut rows = Vec::new();
    for unit in timers {
        let Some(timer) = unit.timer.as_ref() else { continue };
        let open = open_subtrees.contains(&unit.name);
        rows.push(Node::Timer {
            name: unit.name.clone(),
            activates: timer.activates.clone(),
            next_in_ms: timer.next_ms.map(|at| at.saturating_sub(now_ms)),
            last_ago_ms: timer.last_ms.map(|at| now_ms.saturating_sub(at)),
            children: open && !unit.triggers.is_empty(),
            activated_failed: failed.contains(timer.activates.as_str()),
        });
        if open {
            rows.extend(trigger_children(unit, units, open_subtrees, expanded, now_ms, 1));
        }
    }
    rows
}

/// A unit and, beneath it, whatever it starts.
///
/// systemd's *causal* hierarchy, where the slice tree is its containment
/// one: a timer runs a service, a path runs the unit watching it, an
/// activation socket runs the service behind it. It answers "what does
/// this actually do", which for a timer or a socket is the only
/// interesting question about it - the unit itself does nothing but wait.
///
/// Closed by default, like the slice tree and for the same reason: the
/// unit it starts is already listed under its own type.
fn triggered_rows<E: Expanded>(
    unit: &Unit,
    units: &[Unit],
    open_subtrees: &HashSet<String>,
    expanded: &E,
    now_ms: u64,
    depth: u32,
) -> Vec<Node> {
    let open = open_subtrees.contains(&unit.name);
    let mut rows = unit_rows(unit, expanded, now_ms, depth, open && !unit.triggers.is_empty());
    if open {
        rows.extend(trigger_children(unit, units, open_subtrees, expanded, now_ms, depth + 1));
    }
    rows
}

/// The units named by `triggers`, as rows, in the order the unit names
/// them. A name systemd reports but the poll did not see is skipped
/// rather than drawn as a row with nothing behind it.
fn trigger_children<E: Expanded>(
    unit: &Unit,
    units: &[Unit],
    open_subtrees: &HashSet<String>,
    expanded: &E,
    now_ms: u64,
    depth: u32,
) -> Vec<Node> {
    unit.triggers
        .iter()
        .filter_map(|name| units.iter().find(|candidate| &candidate.name == name))
        .flat_map(|child| triggered_rows(child, units, open_subtrees, expanded, now_ms, depth))
        .collect()
}

/// One slice and everything under it, recursively.
///
/// Closed by default, and that is not a default so much as a necessity:
/// `system.slice` owns 130 services on this host, all of them already
/// listed under Services, and opening every slice would list them a second
/// time the moment the view was drawn. A tree earns its indentation only
/// when you ask it to.
///
/// Child slices before member units, so the shape of the hierarchy is
/// visible before its contents.
fn slice_tree<E: Expanded>(
    slice: &Unit,
    units: &[Unit],
    open_slices: &HashSet<String>,
    expanded: &E,
    now_ms: u64,
    depth: u32,
) -> Vec<Node> {
    let mine = |unit: &&Unit| unit.slice.as_deref() == Some(slice.name.as_str());
    let children: Vec<&Unit> = units.iter().filter(mine).collect();
    let open = open_slices.contains(&slice.name);

    let mut rows = unit_rows(slice, expanded, now_ms, depth, !children.is_empty() && open);
    if !open {
        return rows;
    }

    let (mut nested, mut leaves): (Vec<&Unit>, Vec<&Unit>) = children.into_iter().partition(|u| u.kind == UnitKind::Slice);
    nested.sort_by(|a, b| a.name.cmp(&b.name));
    leaves.sort_by(|a, b| severity(a.active_state).cmp(&severity(b.active_state)).then_with(|| a.name.cmp(&b.name)));

    for child in nested {
        rows.extend(slice_tree(child, units, open_slices, expanded, now_ms, depth + 1));
    }
    for leaf in leaves {
        rows.extend(unit_rows(leaf, expanded, now_ms, depth + 1, false));
    }
    rows
}

/// One unit's row, and its detail row when the unit is open.
pub fn unit_rows<E: Expanded>(unit: &Unit, expanded: &E, now_ms: u64, depth: u32, children: bool) -> Vec<Node> {
    let open = expanded.is_open(&unit.name);
    let mut rows = vec![Node::Unit {
        // 0 is systemd's "no transition recorded", not 1970.
        age_ms: (unit.since_ms > 0).then(|| now_ms.saturating_sub(unit.since_ms)),
        expanded: open,
        depth,
        children,
        unit: unit.clone(),
    }];
    if open {
        rows.push(Node::UnitDetail { unit: unit.clone(), detail: expanded.detail(&unit.name).map(Box::new) });
    }
    rows
}

/// The unit popup the design mocked up and nothing had ever built.
///
/// Runtime, Persistence and Inspect, with the ownership guard marking the
/// three verbs that write under `/etc/systemd/system`. A marked row is
/// **still listed and still runnable** - the mark says the effect will
/// not last, not that the key is dead.
///
/// `ownership` is `None` when the read *failed*, which is deliberately
/// not the same as `Some(Imperative)`. A platform that could not answer
/// must not be reported as one that answered "nothing else owns this":
/// the popup marks the rows and says it could not tell, rather than
/// promising a persistence it never checked.
///
/// The chords are the popup's own, from the design's mockup, and they are
/// not the top-level bindings - `s` starts here where `S` starts outside.
/// That is what a transient is: its letters are chosen to read together
/// in one small list, the way magit's are.
pub fn unit_transient(unit: &str, ownership: Option<&Ownership>) -> TransientDef {
    let declarative = match ownership {
        Some(Ownership::Declarative { .. }) | None => true,
        Some(Ownership::Imperative) => false,
    };
    let row_note = match ownership {
        Some(Ownership::Declarative { reverted_by, .. }) => Some(format!("reverts on {reverted_by}")),
        None => Some("persistence unknown".to_string()),
        Some(Ownership::Imperative) => None,
    };
    // The `!` is written here rather than by the renderer, because only
    // this function knows the annotation is a warning - the Nix
    // transient's `policy: weekly, keep 14d` is not.
    let group_note = match ownership {
        Some(Ownership::Declarative { note, .. }) => Some(format!("! {note}")),
        None => Some("! could not read who owns this unit".to_string()),
        Some(Ownership::Imperative) => None,
    };

    let persistence = [('e', "enable", UnitVerb::Enable), ('d', "disable", UnitVerb::Disable), ('m', "mask", UnitVerb::Mask)]
        .into_iter()
        .map(|(chord, label, verb)| DefRow { chord, label, note: row_note.clone(), dimmed: declarative, action: Action::Unit(verb) })
        .collect();

    TransientDef {
        title: format!("Unit . {unit}"),
        switches: Vec::new(),
        groups: vec![
            DefGroup { heading: "Runtime".to_string(), note: None, rows: runtime_rows() },
            DefGroup { heading: "Persistence".to_string(), note: group_note, rows: persistence },
            DefGroup {
                heading: "Inspect".to_string(),
                note: None,
                rows: vec![DefRow { chord: 'l', label: "logs", note: None, dimmed: false, action: Action::Logs }],
            },
        ],
    }
}

fn runtime_rows() -> Vec<DefRow> {
    [('s', "start", UnitVerb::Start), ('S', "stop", UnitVerb::Stop), ('r', "restart", UnitVerb::Restart), ('R', "reload", UnitVerb::Reload)]
        .into_iter()
        .map(|(chord, label, verb)| DefRow { chord, label, note: None, dimmed: false, action: Action::Unit(verb) })
        .collect()
}
