//! The contract between the application layer and whatever draws it.
//!
//! `masys-app` builds a [`View`]; `masys-render` renders one. Neither
//! depends on the other - they meet here. Everything below is plain
//! owned or borrowed data: no ratatui types anywhere.
//!
//! **Where the line falls.** The sentence that used to be here stopped
//! being true on 2026-09-06, when the buffer catalogue and then
//! `Presentation` moved in. It read "no decisions - where a choice is
//! presentational (which glyph a `Finding` gets, how `Overview` lays out
//! across its three lines), the renderer makes it", and both of its
//! examples are now wrong: [`presentation`] decides a finding's glyph,
//! and the overview is one row per part rather than three lines. The
//! rule those examples were reaching for is narrower than "no
//! decisions", and it is this:
//!
//! * **Judgements stay below.** Nothing here compares a figure against a
//!   threshold or works out how bad a reading is. `masys_domain::triage`
//!   holds the rules and the `Thresholds` they read, and a `Severity`
//!   arrives here already decided.
//! * **Style stays above.** No colour, no width, no span, no glyph
//!   *styling* - `masys-render` turns a `Severity` into a colour and pads
//!   a column to a measured width, and it is the only thing that may.
//! * **What a row is stays here.** Which section a finding files under,
//!   what its label column holds, which of the four glyphs heads it, what
//!   text follows, where `.` goes from it. These are facts about the row,
//!   answerable without a terminal, and answering them in one place is
//!   what [`presentation`] exists for.

pub mod buffer;
pub mod format;
pub mod jump;
pub mod node;
pub mod presentation;

use masys_domain::finding::Severity;
use masys_domain::platform::PendingReboot;
use masys_domain::rate::Throughput;
use masys_domain::sample::{Machine, SystemState};

pub use node::{Node, SectionKind};

/// A value, and what masys concluded about it.
///
/// The pair exists so that a judgement travels *with* the figure it is
/// about. The session makes it - masys-app owns the `Thresholds` and runs
/// the triage pass - and the renderer only maps it to a colour, which is
/// the split this crate's own doc states above: no decisions here, and
/// none in what the renderer is handed either.
///
/// **Only wrap what has a rule.** A reading with no threshold is a bare
/// value on `Overview`, not a `Reading` carrying `Severity::Normal` -
/// uptime, unit count and the machine's identity are facts, and there is
/// no answer to how long a host should have been up. Making that
/// structural rather than conventional is the point of the type: a bare
/// `u64` has nowhere to put a verdict nobody is entitled to.
///
/// Built field by field wherever one is needed, with no constructor for
/// any particular severity - and deliberately none for `Normal`, which
/// is the one a caller reaches for when it has not thought about what it
/// measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading<T> {
    pub value: T,
    pub severity: Severity,
}

/// What the renderer measured while drawing.
///
/// Handed back rather than consumed: nothing observes it yet, and
/// `App::dispatch` says so where it moves a page by a fixed `PAGE`
/// instead. Sizing a page from `list_height` is not the one-line change
/// it looks like - the cursor is a node index and the list is items, one
/// per line of an open detail block, so *n* cursor steps is not *n*
/// rendered rows. `build_items` returns the mapping that would fix it.
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
/// One line of the System section.
///
/// The overview is drawn as several lines, and each is a row the cursor
/// can rest on. It was a single `Node` holding the whole block until
/// 2026-09-05, which made the section one seven-line item: the cursor
/// could stop there but the highlight covered all of it, because a list
/// item is the unit of selection.
///
/// Named for what each line is *about* rather than numbered, because the
/// lines that appear depend on what the host answered - a machine with no
/// PSI draws no `Pressure` line at all - so a position means nothing
/// stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewPart {
    /// Distro and kernel: what the host is.
    Identity,
    /// CPU model and core count.
    Hardware,
    /// System state, unit count, load, uptime.
    Status,
    /// The three genuine percentages - cpu, memory, zram - and their bars.
    Resources,
    /// What the machine is moving, over network and disk.
    Throughput,
    /// The PSI figures the colours above come from, plus free swap.
    Pressure,
    /// The standing warnings: clock, SMART, a pending reboot.
    Health,
}

impl OverviewPart {
    /// Every part, in the order they are drawn.
    ///
    /// The single source of what parts exist, so a part added to the enum
    /// reaches [`Overview::parts`] and the renderer without a second list
    /// to keep in step - the same reason `Buffer::ALL` exists.
    pub const ALL: &'static [OverviewPart] = &[
        OverviewPart::Identity,
        OverviewPart::Hardware,
        OverviewPart::Status,
        OverviewPart::Resources,
        OverviewPart::Throughput,
        OverviewPart::Pressure,
        OverviewPart::Health,
    ];
}

/// masys-systemd's call once it exists to report them.
#[derive(Debug, Clone, PartialEq)]
pub struct Overview {
    /// What the host is, as opposed to what it is doing. `None` when the
    /// adapter could not identify it - the System section then simply
    /// starts at the state line.
    pub machine: Option<Machine>,
    pub system_state: SystemState,
    /// How many units this host has, or `None` when nobody has managed
    /// to count them.
    ///
    /// `None` only where the read failed and there is no earlier answer
    /// to stand in - on any later tick the last good list is still
    /// there, stale but counted, with an `Unreadable` row saying so. A
    /// bare `0` would be the glossary's *fabricated*: a number that
    /// looks like a reading and came from a read that did not work.
    pub unit_count: Option<u32>,
    pub load_1: Option<f32>,
    pub load_5: Option<f32>,
    pub load_15: Option<f32>,
    pub uptime_secs: Option<u64>,
    /// What the machine is moving, over the network and to storage.
    ///
    /// Bare, with no severity, and deliberately: there is no threshold
    /// for "too much traffic" and there should not be one. A host
    /// saturating its NIC during a backup is working, not failing, and a
    /// colour on it would be the kind of false alarm that teaches an
    /// operator to stop looking.
    ///
    /// `None` until two samples exist - throughput is a derivative, and
    /// a host sampled once has none. The IO buffer derives these and
    /// hands them over rather than the session summing its own, so the
    /// figure here and the rows there cannot disagree.
    pub net_throughput: Option<Throughput>,
    pub disk_throughput: Option<Throughput>,
    /// What share of the last interval the CPU spent doing something.
    ///
    /// `None` until two samples exist, because utilisation is a
    /// derivative - the same rule `masys_domain::rate` states for every
    /// other rate, and the reason a host one tick old shows no figure
    /// rather than an idle machine.
    ///
    /// Judged by CPU pressure, not by the percentage. A machine at 100%
    /// with nothing queued behind it is a machine being used; PSI is what
    /// says the work is not getting done.
    pub cpu_percent: Option<Reading<f32>>,
    /// The PSI figures the colours above are derived from, so the reading
    /// behind a coloured segment is visible rather than only its verdict.
    ///
    /// `some_avg60` for each resource - the figure
    /// `Thresholds::psi_some_avg60_percent` compares, and the one that
    /// decides the common case. The `full` line that raises a segment to
    /// urgent is on the `FindingKind::Pressure` below, where the detail
    /// belongs; the header is for scanning.
    ///
    /// `None` on a kernel that reports no PSI at all, which drops the
    /// whole line rather than printing zeroes.
    pub cpu_pressure: Option<Reading<f32>>,
    pub io_pressure: Option<Reading<f32>>,
    pub memory_pressure: Option<Reading<f32>>,
    /// How much memory is in use, and how much trouble that is.
    ///
    /// The severity does not come from this figure. A high memory-used
    /// percentage is normal on Linux, because the kernel fills free RAM
    /// with page cache, so a threshold on it would be a false alarm by
    /// design - the verdict comes from PSI, which is the reading that
    /// says memory is actually hurting. `Severity::Unknown` on a kernel
    /// that reports no PSI at all.
    pub mem_used_bytes: Option<Reading<u64>>,
    /// Bare: total memory is what the machine has, not something it is
    /// doing, and there is no threshold for owning too much RAM.
    pub mem_total_bytes: Option<u64>,
    /// `None` on a host with no zram swap configured, same as when it's
    /// simply not measured yet - masys-systemd's later plan is what makes
    /// those distinguishable, if it turns out to matter.
    pub zram_percent: Option<f32>,
    pub swap_free_bytes: Option<u64>,
    /// Whether the clock is disciplined, and what it means that it is
    /// not. Judged rather than bare: an unsynchronised clock silently
    /// breaks TLS validation, kubernetes auth and journal ordering, and
    /// it produces a `FindingKind::ClockUnsynchronized` saying so - the
    /// header carried the same fact in the colour it drew an uptime in.
    pub clock_synced: Reading<bool>,
    /// `None` when the platform can't answer (see
    /// `masys_domain::service::PlatformService`) rather than when it's
    /// merely unchecked - those need to render differently.
    ///
    /// Three states, and all three are different claims: `None` is "not
    /// read", which draws no segment at all; `Some(false)` is a disk
    /// reporting that it is failing; `Some(true)` is one that says it is
    /// fine. Nothing may collapse the first into either of the others -
    /// an unread disk is the one masys knows least about, and the last
    /// thing it should look like is a healthy one.
    pub smart_ok: Option<Reading<bool>>,
    /// A reboot the running kernel or initrd needs. `None` when none is,
    /// so the segment is absent rather than saying so.
    ///
    /// Carries a severity for the same reason the two above do: whether
    /// a pending reboot is worth noticing is a judgement, and if the
    /// renderer made it the renderer would be deciding what matters.
    pub pending_reboot: Option<Reading<PendingReboot>>,
}

impl Overview {
    /// The parts that have something to say, in the order they are drawn.
    ///
    /// **The one decision about how many rows the System section has.**
    /// The renderer draws exactly the parts named here and applies no
    /// emptiness filter of its own - it used to build all seven and drop
    /// the empty ones, and leaving that in place would have made the row
    /// count answerable in two crates that could disagree. Here the
    /// question is asked once, of the data, which is the only place that
    /// knows whether the host answered.
    ///
    /// `Status` and `Health` are always present: the first opens with the
    /// system state and a unit count that prints `- units` when nobody
    /// took one, and the second always says whether the clock is synced.
    /// Neither can be empty, so neither is conditional.
    pub fn parts(&self) -> Vec<OverviewPart> {
        OverviewPart::ALL
            .iter()
            .copied()
            .filter(|part| self.has(*part))
            .collect()
    }

    /// Whether one part has anything to draw.
    ///
    /// Exhaustive, so a new `OverviewPart` cannot be added without
    /// deciding when it appears.
    fn has(&self, part: OverviewPart) -> bool {
        let machine = self.machine.as_ref();
        match part {
            OverviewPart::Identity => {
                machine.is_some_and(|m| !m.distro.is_empty() || !m.kernel.is_empty())
            }
            OverviewPart::Hardware => {
                machine.is_some_and(|m| !m.cpu_model.is_empty() || m.cpu_cores > 0)
            }
            OverviewPart::Status => true,
            OverviewPart::Resources => {
                self.cpu_percent.is_some()
                    || (self.mem_used_bytes.is_some() && self.mem_total_bytes.is_some())
                    || self.zram_percent.is_some()
            }
            OverviewPart::Throughput => {
                self.net_throughput.is_some() || self.disk_throughput.is_some()
            }
            // Free swap shares this line, which is why it is named for
            // the line rather than for PSI - see the renderer.
            OverviewPart::Pressure => {
                self.cpu_pressure.is_some()
                    || self.io_pressure.is_some()
                    || self.memory_pressure.is_some()
                    || self.swap_free_bytes.is_some()
            }
            OverviewPart::Health => true,
        }
    }
}

/// Which buffer the rows came from, and what heads it.
/// How the Procs buffer is ordered. Lives here rather than in masys-app
/// because the header displays it, and masys-render cannot name masys-app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcSort {
    Cpu,
    Memory,
    /// Combined read and write rate. One column rather than two: the
    /// question this answers is "what is hitting the disk", and asking an
    /// operator to know the direction before they can look would be
    /// asking them for the answer.
    Io,
    Name,
}

impl ProcSort {
    pub fn label(self) -> &'static str {
        match self {
            ProcSort::Cpu => "cpu",
            ProcSort::Memory => "memory",
            ProcSort::Io => "io",
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
    Procs { sort: ProcSort, descending: bool },
    /// The Log buffer, which is always about one unit and has an order to
    /// announce. Carries its direction for the same reason `Procs` does,
    /// and with more force: journal entries are often seconds apart, so a
    /// reversal that is not stated looks like a key that did nothing.
    ///
    /// The unit is here rather than in a section header because every row
    /// in the buffer belongs to it - and the renderer needs it anyway, to
    /// decide which rows have to name a unit of their own.
    Log { unit: &'a str, newest_first: bool },
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
    /// Only the buffer jumps ever set it, and exactly one at a time. The
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
    /// Runtime/Persistence/Inspect groups, and the Nix family menus.
    ///
    /// Both examples are ones that exist, which is the rule and not an
    /// accident: an example names something a reader can go and open.
    /// This cited "kill's signal picker" until 2026-08-30 - a transient
    /// masys has designed and not built - so a reader met a live example
    /// and a phantom in one sentence with nothing telling them apart.
    ///
    /// That unbuilt picker is also why `Signal` carries `Hup`, `Int`,
    /// `Usr1` and `Usr2` that `ACTIONS` never binds, and why
    /// `SystemService::ionice` has no production caller: designed surface
    /// awaiting the Process buffer's transient, not leftovers.
    ///
    /// `title` is a `String` rather than the `&'static str` this spec
    /// carried while it was unimplemented, because every transient the
    /// design draws names the thing it acts on: the unit popup's title is
    /// `Unit . restic-backup.service`. A static title could only have said
    /// `unit`, and a popup that does not name its subject is one you can
    /// open on the wrong row without noticing.
    Transient {
        title: String,
        switches: Vec<SwitchRow>,
        groups: Vec<ActionGroup>,
    },
    /// A transient's open text-entry sub-step - a retention, a generation
    /// spec, a search query - which replaces the switch/action display
    /// while active.
    ///
    /// Text entry and nothing else. This carried a `candidates` picker
    /// until 2026-08-28, on the design's argument that it "is already the
    /// shape `nix search` needs". Search was built the other way: it is in
    /// `only_reads`, so it suspends and prints to the terminal, and no
    /// result ever comes back to a popup. The other two prompts - an
    /// option name and a generation spec - are free text as well, and
    /// `answer_input` binds no movement key on purpose, so a list could
    /// not have been walked even once filled.
    Input {
        prompt: &'static str,
        typed: &'a str,
    },
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
