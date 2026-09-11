//! Where a jump goes: a buffer, and the row in it.
//!
//! Two things in masys move the cursor to a row somebody *named* rather
//! than to a row somebody pointed at: `esc` returning from a drill-down,
//! and the Status buffer's finding jump. Both ask the same question -
//! which row is the thing I named - so it is asked in exactly one place.
//!
//! The *naming* is here, beside the rows being named. Resolving a name to
//! an index in a particular buffer's rows is `masys_app::jump`, because
//! only the session knows which rows exist right now.

use crate::ProcSort;
use crate::buffer::Buffer;

/// A row, named in terms a lookup can match.
///
/// Deliberately not "a string and a `Node` kind": the *identity* of a row
/// differs by kind, and flattening that would be an invitation to match
/// on whatever text happens to be to hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowTarget {
    /// A unit, by name - and a timer counts, because a timer is a unit.
    /// `go_back` has always matched both: `l` on a timer row has to come
    /// back to that timer, and a target that knew only about
    /// `Node::Unit` would quietly fail to return there.
    Unit(String),
    /// A filesystem, by mount point.
    Filesystem(String),
    /// One unit's journal - a destination that has to be *opened*
    /// rather than moved to.
    ///
    /// The odd one out here, and deliberately named so a reader sees
    /// that. Every other target is a row that already exists once the
    /// buffer is rebuilt, found by `row_named`. The Log buffer holds
    /// whichever unit was last asked for, so arriving at it means
    /// fetching that unit's entries first - which is what `l` does, and
    /// this reaches it by the same path rather than a second one.
    UnitLog(String),
    /// A process, by pid rather than by name. Two processes share a
    /// `comm` whenever a service runs a pool of workers, so a name would
    /// find whichever came first, which is nobody's answer.
    Process(u32),
    /// The busiest row, once Procs is put in this order.
    ///
    /// What a pressure finding wants: it names no row, so the answer is
    /// "whatever is top once the buffer is in the right order". A pid
    /// resolved at jump time would be a *reading* dressed as an identity,
    /// and it would be stale by the time the cursor moved - the sample
    /// that names the top consumer is the one before the tick that
    /// rebuilds the rows.
    ///
    /// Carries the order rather than leaving it a separate field on
    /// [`Jump`], where it was `Some` for exactly this variant and `None`
    /// for every other - a pair that could be combined wrongly and had no
    /// meaning if it was.
    ///
    /// `ProcSort` rather than masys-app's `Sort`, which is this type
    /// under an alias - `keymap.rs` has re-exported it from here since
    /// the Procs header needed to print which way it was sorted.
    TopOfProcs(ProcSort),
}

/// Where a finding takes you: which buffer, and which row in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jump {
    pub buffer: Buffer,
    pub row: RowTarget,
}
