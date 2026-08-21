//! The contract between the application layer and whatever draws it.
//!
//! `masys-app` builds a [`View`]; `masys-render` renders one. Neither
//! depends on the other - they meet here. Everything below is plain
//! owned or borrowed data: no ratatui types, and no decisions - where a
//! choice is presentational (which glyph a `Finding` gets, how `Overview`
//! lays out across its three lines), the renderer makes it.

pub mod node;

use masys_domain::platform::PendingReboot;
use masys_domain::sample::{Machine, SystemState};

pub use node::{Node, SectionKind};

/// What the renderer measured while drawing, handed back so the session
/// can size its paging.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    pub list_height: u16,
    pub modal_height: u16,
    pub modal_content: u16,
}

/// The status buffer's `System` section, reduced to what triage needs to
/// answer "did the checks actually run". `system_state`, `unit_count`, and
/// `clock_synced` come straight from a sample; the rest are `Option`
/// because `masys_domain::sample::Snapshot` doesn't carry load average,
/// uptime, memory, zram, swap, or SMART yet - `None` renders as "not yet
/// measured" (the same convention `masys_domain::rate` uses), not as a
/// fabricated zero. Whether these eventually land on `Snapshot` itself is
/// masys-systemd's call once it exists to report them.
#[derive(Debug, Clone, PartialEq)]
pub struct Overview {
    /// What the host is, as opposed to what it is doing. `None` when the
    /// adapter could not identify it - the System section then simply
    /// starts at the state line.
    pub machine: Option<Machine>,
    pub system_state: SystemState,
    pub unit_count: u32,
    pub load_1: Option<f32>,
    pub load_5: Option<f32>,
    pub load_15: Option<f32>,
    pub uptime_secs: Option<u64>,
    pub mem_used_bytes: Option<u64>,
    pub mem_total_bytes: Option<u64>,
    /// `None` on a host with no zram swap configured, same as when it's
    /// simply not measured yet - masys-systemd's later plan is what makes
    /// those distinguishable, if it turns out to matter.
    pub zram_percent: Option<f32>,
    pub swap_free_bytes: Option<u64>,
    pub clock_synced: bool,
    /// `None` when the platform can't answer (see
    /// `masys_domain::service::PlatformService`) rather than when it's
    /// merely unchecked - those need to render differently.
    pub smart_ok: Option<bool>,
    pub pending_reboot: Option<PendingReboot>,
}

/// Which buffer the rows came from, and what heads it.
/// How the Procs buffer is ordered. Lives here rather than in masys-app
/// because the header displays it, and masys-render cannot name masys-app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcSort {
    Cpu,
    Memory,
    Name,
}

impl ProcSort {
    pub fn label(self) -> &'static str {
        match self {
            ProcSort::Cpu => "cpu",
            ProcSort::Memory => "memory",
            ProcSort::Name => "name",
        }
    }

    /// The direction this column starts in when you first sort by it.
    ///
    /// Name reads a-z; the two measurements read worst-first, which is
    /// the whole reason to sort by them. Pressing the same key again
    /// reverses it - htop and top both work that way, and so does every
    /// file manager's column header.
    pub fn default_descending(self) -> bool {
        !matches!(self, ProcSort::Name)
    }
}

pub enum Header<'a> {
    Status {
        hostname: &'a str,
        timestamp: &'a str,
    },
    /// The Procs buffer. Carries the active sort *and its direction* so
    /// the header can mark both - without that, pressing a sort key on an
    /// idle machine looks like it did nothing, because the order barely
    /// changes, and reversing it looks like nothing at all.
    Procs {
        sort: ProcSort,
        descending: bool,
    },
    /// The Log buffer, which is always about one unit and has an order to
    /// announce. Carries its direction for the same reason `Procs` does,
    /// and with more force: journal entries are often seconds apart, so a
    /// reversal that is not stated looks like a key that did nothing.
    ///
    /// The unit is here rather than in a section header because every row
    /// in the view belongs to it - and the renderer needs it anyway, to
    /// decide which rows have to name a unit of their own.
    Log {
        unit: &'a str,
        newest_first: bool,
    },
    /// Buffers with no header content of their own beyond a name.
    Titled(&'static str),
}

/// The bottom line: the footer hint row and the outcome of the last
/// command, merged into the one row a single-pane TUI has to spare.
pub enum StatusLine<'a> {
    /// Nothing to report, so the row shows what the keys do instead - the
    /// design's footer hint, e.g. `[r]estart [l]ogs [b] buffers`.
    Hints,
    /// A command is in flight.
    Busy,
    Message(&'a str),
    /// An action's stderr, held until the next command - a refresh must
    /// not silently erase it (see the design's Error handling table).
    Error(&'a str),
}

/// Everything needed to draw one frame.
pub struct View<'a> {
    pub header: Header<'a>,
    /// The open buffer's rows, already flattened and folded.
    pub rows: &'a [Node],
    /// Index into `rows`, or `None` on an empty buffer.
    pub selected: Option<usize>,
    pub status: StatusLine<'a>,
    /// The popup drawn over the buffer, if one is open.
    pub modal: Option<ModalView<'a>>,
    /// The text rows are being narrowed by, if any. Shown at the top of
    /// the buffer rather than in a popup: a centred modal covers the very
    /// rows it is filtering, and the filter has to stay visible while
    /// they move under it.
    pub filter: Option<&'a str>,
    /// Whether the filter is being typed right now, so the renderer can
    /// draw a cursor and the session can tell an empty filter from no
    /// filter.
    pub typing: bool,
    /// How many rows the filter matched, or `None` when nothing is being
    /// filtered on.
    ///
    /// Smaller than the row count: a section header kept because
    /// something under it survived is structure, not a result. Shown
    /// beside the query as it is typed, because the one thing a live
    /// filter cannot otherwise tell you is whether the next character
    /// took you from "nearly there" to "nothing at all".
    pub filter_matches: Option<usize>,
    /// Whether the tick has stopped because a process row is open.
    ///
    /// Rendered rather than merely obeyed: a view that has quietly frozen
    /// is indistinguishable from a machine that has, and the second is a
    /// far more alarming thing to conclude.
    pub auto_refresh_paused: bool,
    /// What the footer offers: buffer jumps and the global verbs, built
    /// by the session because only it knows which buffers exist and what
    /// the current keymap binds. The renderer used to hardcode this
    /// string, which meant the footer could not tell the truth after a
    /// keymap change.
    pub hints: &'a [KeyBinding],
    /// The open buffer's own keys. Kept apart from `hints` because the
    /// two together overflow 80 columns, and a truncated hint row drops
    /// whatever is rightmost - which would be the buffer's own actions.
    pub actions: &'a [KeyBinding],
}

/// One key and what it does, for the footer hints and the key help.
///
/// `chord` is a `String` rather than `ActionRow`'s `char` because a
/// binding is not always one character: `b p` is a prefix and a letter,
/// and `Tab`/`PgDn` are names rather than glyphs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBinding {
    pub chord: String,
    pub label: String,
    /// Whether to draw this key dimmed.
    ///
    /// Dimmed means "this will not do what you expect here", not "this is
    /// gone": the key still works and still occupies its usual position,
    /// so muscle memory is never punished with a silently missing row.
    /// The design's rule for action rows, applied to the footer and the
    /// help - masys never hides an action outright, only marks it.
    pub dimmed: bool,
    /// Whether this key leads to where you already are.
    ///
    /// Only the view jumps ever set it, and exactly one at a time. The
    /// footer used to omit the current view instead, which made the row
    /// shorter but also made it *move* - the same key sat in a different
    /// place depending on where you were, so it could not be read by
    /// position. Listing all of them keeps the row fixed and turns it into
    /// a map; this flag is the only thing on it that changes as you move.
    pub active: bool,
}

/// One titled bucket of bindings in the key help - "Movement", "Buffers",
/// and the open buffer's own section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyGroup {
    pub heading: String,
    pub bindings: Vec<KeyBinding>,
}

/// A popup, in the shapes it can take.
pub enum ModalView<'a> {
    /// Every binding available right now, grouped. Opened by `?`.
    /// Separate from `Transient` because it is a reference list rather
    /// than a menu: nothing here is dispatchable, and its chords are
    /// strings rather than single characters.
    Keys { groups: Vec<KeyGroup> },
    /// A transient's switch rows and action columns - the unit popup's
    /// Runtime/Persistence/Inspect groups, kill's signal picker, and so
    /// on.
    ///
    /// `title` is a `String` rather than the `&'static str` this spec
    /// carried while it was unimplemented, because every transient the
    /// design draws names the thing it acts on: the unit popup's title is
    /// `Unit . restic-backup.service`. A static title could only have said
    /// `unit`, and a popup that does not name its subject is one you can
    /// open on the wrong row without noticing.
    Transient { title: String, switches: Vec<SwitchRow>, groups: Vec<ActionGroup> },
    /// A transient's open text-entry or picker sub-step (e.g. renice's
    /// typed value), which replaces the switch/action display while
    /// active.
    Input { prompt: &'static str, typed: &'a str, candidates: Option<CandidateList<'a>> },
    /// A yes/no prompt guarding a destructive or non-persistent action.
    /// The action itself is held by the session, not described here.
    Confirm { prompt: &'a str },
}

/// One toggleable switch, with its current state already resolved - the
/// renderer shouldn't have to cross-reference a separate set of enabled
/// chords to decide between `[x]` and `[ ]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchRow {
    pub group: &'static str,
    pub chord: &'static str,
    pub label: &'static str,
    /// Whether masys implements this switch at all. Unsupported ones are
    /// still listed for the renderer to dim.
    pub supported: bool,
    pub on: bool,
}

/// One bucket of action rows in a transient popup, e.g. `Runtime` or
/// `Persistence`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionGroup {
    /// A `String` for the same reason `Transient`'s title is one, and the
    /// same reason `KeyGroup`'s already was: the Nix transient's
    /// generation group names its generation (`Generation 436`), so the
    /// heading is contextual and cannot be `&'static str`.
    pub heading: String,
    /// A group-level annotation, e.g. `declared in nix` - the
    /// persistence-ownership guard's `Ownership::Declarative` note.
    /// `None` for a group with nothing to flag.
    pub note: Option<String>,
    pub rows: Vec<ActionRow>,
}

/// One action row within an `ActionGroup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRow {
    pub chord: char,
    pub label: &'static str,
    /// A trailing per-row note, e.g. `reverts on nixos-rebuild`. `None`
    /// for an action with no persistence caveat.
    pub note: Option<String>,
    /// Whether to dim this row. A dimmed row is still listed in position
    /// and still runnable behind a confirmation - masys never hides an
    /// action outright, only marks it (see the design's
    /// persistence-ownership guard).
    pub dimmed: bool,
}

/// A picker's live-filtered candidates and which one is highlighted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateList<'a> {
    pub matches: Vec<&'a str>,
    pub selected: usize,
}
