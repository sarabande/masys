//! Finding the row a jump names.
//!
//! Two things in masys move the cursor to a row somebody named rather
//! than to a row somebody pointed at: `esc` returning from a drill-down,
//! and the Status buffer's finding jump. Both ask the same question -
//! *which row is the thing I named* - and the answer has to be the same
//! one, so it is asked in exactly one place.
//!
//! It lived inside `App::go_back` as a two-arm match while returning from
//! `l` was the only caller. A second copy for filesystems and processes
//! is the shape `buffer.rs` records going wrong before: two owners of one
//! question, disagreeing where it is worst.

use masys_view::Node;

pub use masys_view::jump::{Jump, RowTarget};

/// Where `target` is in `rows`, or `None` if it is not there.
///
/// **`None` is a real answer and the important one.** The rows are
/// rebuilt between a jump being decided and the cursor being moved - a
/// tick lands, a unit stops, a filesystem unmounts, a process exits - so
/// the thing named may be gone by the time this is asked. A caller that
/// got a default index back would put the cursor on an unrelated row and
/// present it as the row that was asked for, which is this tree's oldest
/// defect wearing a cursor.
///
/// Matches by kind as well as by name. A mount point and a unit name
/// cannot collide today; answering "is there any row whose text is this"
/// rather than "is there a filesystem row for this mount" is how they
/// would the first time one did.
pub fn row_named(rows: &[Node], target: &RowTarget) -> Option<usize> {
    // **On the target, so that the outer match carries no wildcard** -
    // for the reason `RowTarget` gives.
    //
    // The inner wildcards are a different thing and are safe: they range
    // over row *kinds*, where "this is not a filesystem row" is the
    // honest answer and a new `Node` variant genuinely does not match a
    // mount point. Matching on the pair put both kinds in one match,
    // where the outer one silently absorbed two arms written out above
    // it.
    rows.iter().position(|row| match target {
        // A timer is a unit here: `l` on a timer row has to come back to
        // that timer, and a target that knew only about `Node::Unit`
        // would silently fail to return there.
        RowTarget::Unit(name) => match row {
            Node::Unit { unit, .. } => unit.name == *name,
            Node::Timer { name: timer, .. } => timer == name,
            _ => false,
        },
        RowTarget::Filesystem(mount) => {
            matches!(row, Node::Filesystem { filesystem, .. } if filesystem.mount_point == *mount)
        }
        RowTarget::Process(pid) => matches!(row, Node::Proc { proc, .. } if proc.pid == *pid),
        // Not a *named* row, so this function has nothing to say about
        // it: the answer is an ordering and a position, which only the
        // session can resolve because only it knows which rows the cursor
        // may rest on. `jump_to_finding` handles it, and this returning
        // `None` is what keeps the two documented properties of this
        // function - matches by kind, `None` when absent - true.
        RowTarget::TopOfProcs(_) => false,
        // Nor this one, for a different reason: the row does not exist
        // yet. The Log buffer has to be opened on the unit before it has
        // any rows at all, which only the session can do, and once it
        // has the destination is the buffer rather than a row in it.
        RowTarget::UnitLog(_) => false,
    })
}
