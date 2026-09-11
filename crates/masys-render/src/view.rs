//! Pure text/line construction: turns domain-shaped data (`Finding`,
//! `Overview`) into styled ratatui `Line`s. No `Frame`, no layout -
//! `render.rs` decides where these go, not what they say.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use masys_domain::declarative::{Generation, Input, kernel_version};
use masys_domain::finding::Severity;
use masys_domain::journal::{Entry, Priority};
use masys_domain::platform::Package;
use masys_domain::proc_detail::{Fd, FdTarget, ProcDetail};
use masys_domain::rate::ProcRate;
use masys_domain::rate::{DiskRate, NetRate};
use masys_domain::sample::SystemState;
use masys_domain::sample::{Disk, Filesystem, Interface, Proc, ProcState};
use masys_domain::unit::{ActiveState, Unit, UnitDetail};
use masys_view::presentation::Presentation;
use masys_view::{Node, Overview, OverviewPart, ProcSort};

// Moved to masys-view so a finding's `Presentation` can build its own
// tail. Re-exported because the mockup's units read as a renderer fact
// and every caller already spells them `masys_render::view::`.
pub use masys_view::format::{human_bytes, human_duration};

use crate::theme::Theme;

/// `12_000_000_000 -> "11G"`. Binary units (1024-based, matching `df -h`
/// under the hood), zero decimals once the value reaches double digits in
/// its unit - matching the mockup's `12G`/`61M`, not `11.18G`.
/// Breaks a Unix millisecond timestamp into local calendar fields.
///
/// Via `localtime_r` rather than pulling in `chrono` or `time` for two
/// labels, and deliberately not UTC: an operator reading a journal line
/// stamped six hours off will distrust every other number on the screen.
/// `localtime_r` goes through the C library, so it follows the same zone
/// systemd and the journal itself use.
fn local_fields(ms: u64) -> Option<libc::tm> {
    let secs = (ms / 1000) as libc::time_t;
    // SAFETY: `tm` is zeroed and correctly sized, and `localtime_r`
    // writes into it rather than returning a shared static the way
    // `localtime` does. A null return means the timestamp is
    // unrepresentable, which is the one case not handled by reading it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        None
    } else {
        Some(tm)
    }
}

/// `HH:MM:SS` in local time - the journal's own column.
pub fn local_time(ms: u64) -> String {
    match local_fields(ms) {
        Some(tm) => format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec),
        None => "--:--:--".to_string(),
    }
}

/// `YYYY-MM-DD HH:MM` in local time - the status header's clock.
pub fn local_datetime(ms: u64) -> String {
    match local_fields(ms) {
        Some(tm) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min
        ),
        None => String::new(),
    }
}

/// `YYYY-MM-DD` in local time, for a row where the hour says nothing - an
/// input's upstream commit is a day, not a minute.
///
/// `-` rather than an empty column for a timestamp that cannot be
/// represented, the same shrug the rest of this module prints for a
/// reading it does not have.
fn local_date(ms: u64) -> String {
    match local_fields(ms) {
        Some(tm) => format!(
            "{:04}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday
        ),
        None => "-".to_string(),
    }
}

/// One finding row, its label padded to a width the caller measured
/// across the section.
///
/// Takes the row rather than the finding: everything drawn here except
/// the colour comes from `Presentation`, and the colour comes from
/// `Severity`. This crate makes no decision about a finding any more -
/// it turns six answers into spans.
fn finding_line_padded(row: &FindingRow, theme: &Theme, label_width: usize) -> Line<'static> {
    let p = &row.presentation;
    // A label-less finding is never padded - its text is the tail whole,
    // and padding an empty label would just indent it out of line.
    let text = match p.label() {
        // Padded by hand rather than with `{label:<label_width$}`: the
        // format width specifier counts chars, which is the very thing
        // `label_width` is measured in display columns to avoid.
        Some(label) => {
            let padding = " ".repeat(label_width.saturating_sub(label.width()));
            format!("{label}{padding}{}", p.tail())
        }
        None => p.tail().to_string(),
    };
    Line::from(vec![
        Span::styled(
            format!("{} ", p.glyph()),
            severity_style(Some(row.severity), theme),
        ),
        Span::raw(text),
    ])
}

/// One finding, as the two things drawing it needs.
///
/// A pair rather than a `&Node`, because `finding_lines` is handed a
/// *section* and a slice of nodes cannot be narrowed to one variant. The
/// severity lives on the `Finding` and everything else on its
/// `Presentation`; both are already on the node, and `build_items` takes
/// them off it.
#[derive(Debug, Clone)]
pub struct FindingRow {
    pub severity: Severity,
    pub presentation: Presentation,
}

/// One section's findings, with their labels padded to a common width so
/// the columns after them line up.
///
/// The mockup aligns *within* a section, which a single-finding function
/// cannot do because it cannot see its siblings - so pass one section's
/// worth: a wider slice over-pads the narrow section, a narrower one
/// gives up alignment altogether.
///
/// **The only way in, and it was not.** A `finding_line` sat here taking
/// one row and passing a width of `0`, and nothing but tests ever called
/// it - nineteen of them, aimed at a path `build_items` never takes,
/// with the padding argument that is the most likely thing to be wrong
/// hidden behind a constant. A single-row slice through here gives the
/// same line, because the width is the maximum over the slice and the
/// maximum of one label is its own width.
///
/// The glyph maps onto severity tiers rather than onto the fourteen
/// `FindingKind` variants - `x` (dead), `~`/`^` (unstable or under
/// pressure, not yet dead), `!` (urgent) - matching how the mockup
/// actually reads. Both it and the colour arrive decided: `Presentation`
/// chose the first and `Severity` the second, and this crate chooses
/// neither.
pub fn finding_lines(rows: &[FindingRow], theme: &Theme) -> Vec<Line<'static>> {
    let label_width = rows
        .iter()
        .filter_map(|row| row.presentation.label())
        // Terminal columns, not bytes and not chars: a CJK mount point is
        // one char but two columns wide, and a combining mark is one char
        // but zero columns, so any other measure skews the tail column.
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0);
    rows.iter()
        .map(|row| finding_line_padded(row, theme, label_width))
        .collect()
}

/// One row of the process table: the process, the rate pass's answer for
/// it, the cumulative CPU seconds the app derived, and how long it has
/// been up.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcRow {
    pub proc: Proc,
    pub rate: Option<ProcRate>,
    pub cpu_seconds: u64,
    /// Wall-clock age, or `None` when the start time is unknown. Derived
    /// in the app layer against the tick's `now_ms`, because nothing in
    /// this module is allowed a clock.
    pub age_ms: Option<u64>,
}

/// The kernel caps `/proc/[pid]/comm` at 16 bytes including its NUL, so
/// no process name can exceed this and the column never needs truncating.
/// It is a *floor* rather than the width: `ProcColumns` grows it to
/// whatever the terminal has spare.
const NAME_WIDTH: usize = 15;
const PID_WIDTH: usize = 7;
const CPU_WIDTH: usize = 7;
const MEM_WIDTH: usize = 8;
const IO_WIDTH: usize = 8;
const UP_WIDTH: usize = 8;
const NICE_WIDTH: usize = 4;
const OOM_WIDTH: usize = 5;
const THR_WIDTH: usize = 5;
const STATE_WIDTH: usize = 3;
const TIME_WIDTH: usize = 10;

/// The table at its narrowest: pid and name at their floors, plus the six
/// columns that are never dropped.
const BASE_WIDTH: usize = PID_WIDTH
    + 2
    + NAME_WIDTH
    + CPU_WIDTH
    + MEM_WIDTH
    + IO_WIDTH
    + THR_WIDTH
    + STATE_WIDTH
    + TIME_WIDTH;

/// The optional columns, in the order they switch on. Left to right, so a
/// column only ever appears to the right of the ones already there and
/// the table never rearranges itself as the terminal grows - it only
/// extends.
///
/// The first entry buys the *second* IO column: below it, read and write
/// are summed into one `IO/s`.
const EXTRA_WIDTHS: [usize; 4] = [IO_WIDTH, UP_WIDTH, NICE_WIDTH, OOM_WIDTH];

/// Which columns the terminal is wide enough for, and how much of the
/// slack the name column absorbs.
///
/// The Procs table used to be a fixed 65 columns wide however large the
/// terminal was, which left a third of a modern window blank and
/// truncated cgroup names that had room to spare. This is the one place
/// the width is turned into a layout, so the header and the rows read it
/// from the same answer and cannot drift.
///
/// Extras are added rather than the table being scaled: the numeric
/// columns have fixed maximum widths, so widening them buys nothing but
/// whitespace. Everything left over goes to the name, which is the only
/// column with more to say than it has room for - and that also pins the
/// numbers to the right edge, where they line up with the footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcColumns {
    pub name_width: usize,
    /// Read and write as their own columns rather than one summed `IO/s`.
    pub split_io: bool,
    pub uptime: bool,
    pub nice: bool,
    pub oom: bool,
}

impl Default for ProcColumns {
    /// The narrow layout - what the table was before it could see a
    /// width. Callers that have no terminal to measure (tests, and the
    /// `proc_line`/`proc_group_line` single-row helpers) get this.
    fn default() -> Self {
        Self::fit(BASE_WIDTH)
    }
}

impl ProcColumns {
    /// The widest layout that fits in `width` columns.
    ///
    /// Each extra switches on only when the table can still pay for it,
    /// so a narrow terminal drops the rightmost extras instead of
    /// truncating - which silently eats `TIME` and `S`, the failure the
    /// footer's action row already had to be taught to avoid.
    pub fn fit(width: usize) -> Self {
        // `take_while`, not `filter`: a column that does not fit stops
        // the ones after it too, so the extras are always a *prefix* of
        // `EXTRA_WIDTHS`. Skipping one and taking the next would let a
        // terminal one column wider insert a column in the middle and
        // shove everything right of it sideways - the same shuffling
        // under the cursor that the Procs buffer sorts stably to avoid.
        let shown = EXTRA_WIDTHS
            .iter()
            .scan(BASE_WIDTH, |used, &cost| {
                *used += cost;
                Some(*used)
            })
            .take_while(|&total| total <= width)
            .count();
        let used = BASE_WIDTH + EXTRA_WIDTHS[..shown].iter().sum::<usize>();
        Self {
            name_width: NAME_WIDTH + width.saturating_sub(used),
            split_io: shown > 0,
            uptime: shown > 1,
            nice: shown > 2,
            oom: shown > 3,
        }
    }

    /// The columns between IO and THR, as blank space - what a group row
    /// puts there, since nice, oom_score and a start time are facts about
    /// one process and do not sum over a cgroup.
    fn per_process_gap(&self) -> String {
        let width = usize::from(self.uptime) * UP_WIDTH
            + usize::from(self.nice) * NICE_WIDTH
            + usize::from(self.oom) * OOM_WIDTH;
        " ".repeat(width)
    }
}

/// The column headings, laid out from the same `ProcColumns` the rows
/// are, so the headings cannot drift out of alignment with what is under
/// them - which they will, silently, if each writes its own spacing.
///
/// The active sort column is marked `v` or `^` for descending or
/// ascending - which is also the only feedback that pressing the same
/// sort key again did anything, on a machine where reversing the order
/// barely moves the rows. ASCII rather than an arrow glyph, for the same reason the
/// fold markers are: a monitoring tool has to be readable over a serial
/// console with no unicode. The marker also fits inside each column's
/// existing width, so turning it on shifts nothing.
pub fn proc_columns_header(sort: ProcSort, descending: bool, columns: &ProcColumns) -> String {
    let mark = |column: ProcSort, heading: &str| -> String {
        if column == sort {
            format!("{heading}{}", if descending { "v" } else { "^" })
        } else {
            heading.to_string()
        }
    };
    let name_width = columns.name_width;
    // Split into read and write, both halves carry the marker: the sort
    // is over their total, so marking one would name a column that is not
    // the key. `WRITE/sv` fills its eight exactly and so meets `READ/sv`
    // with no gap - widening the pair to buy that space back would shift
    // every column after it, on every host, to pay for a state one of
    // four sorts is in.
    let io = if columns.split_io {
        format!(
            "{:>IO_WIDTH$}{:>IO_WIDTH$}",
            mark(ProcSort::Io, "READ/s"),
            mark(ProcSort::Io, "WRITE/s")
        )
    } else {
        format!("{:>IO_WIDTH$}", mark(ProcSort::Io, "IO/s"))
    };
    let heading = |on: bool, width: usize, text: &str| {
        if on {
            format!("{text:>width$}")
        } else {
            String::new()
        }
    };
    let up = heading(columns.uptime, UP_WIDTH, "UP");
    let ni = heading(columns.nice, NICE_WIDTH, "NI");
    let oom = heading(columns.oom, OOM_WIDTH, "OOM");
    format!(
        "{:>PID_WIDTH$}  {:<name_width$}{:>CPU_WIDTH$}{:>MEM_WIDTH$}{io}{up}{ni}{oom}{:>THR_WIDTH$}{:>STATE_WIDTH$}{:>TIME_WIDTH$}",
        "PID",
        mark(ProcSort::Name, "NAME"),
        mark(ProcSort::Cpu, "%CPU"),
        mark(ProcSort::Memory, "MEM"),
        "THR",
        "S",
        "TIME"
    )
}

/// `1324 -> "22:04"`, `4462 -> "1:14:22"` - `top`'s `TIME+` column.
///
/// Hours appear only when there are hours, but they are never dropped: a
/// process up for two days showing `04:11` would be actively misleading,
/// which is the failure mode a fixed `MM:SS` has.
pub fn human_cpu_time(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn state_letter(state: ProcState) -> &'static str {
    match state {
        ProcState::Running => "R",
        ProcState::Sleeping => "S",
        ProcState::UninterruptibleWait => "D",
        ProcState::Zombie => "Z",
        ProcState::Stopped => "T",
    }
}

/// A per-second byte figure: blank at zero, so a column of `0B` does not
/// bury the rows that are actually moving data.
fn flow(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1.0 {
        human_bytes(bytes_per_sec as u64)
    } else {
        String::new()
    }
}

/// A signed per-process number that is blank at its default, so only the
/// rows someone has deliberately changed - or the kernel has flagged -
/// stand out. `0` is the value almost every process has, and a column of
/// zeroes is a column no one reads.
fn notable(value: i32) -> String {
    if value != 0 {
        value.to_string()
    } else {
        String::new()
    }
}

/// One process row.
///
/// `rate` is `None` until two samples exist for this pid, and its CPU and
/// IO columns render as `-`. The design states the rule for the Procs
/// buffer and it holds here too: `-` means not yet measured, blank means
/// measured and genuinely zero. Collapsing the two would make a process
/// masys has never managed to time look like one doing nothing.
fn proc_line_padded(row: &ProcRow, theme: &Theme, columns: &ProcColumns) -> Line<'static> {
    let ProcRow {
        proc,
        rate,
        cpu_seconds,
        age_ms,
    } = row;
    // Padded by hand rather than with `{name:<name_width$}`: the format
    // width specifier counts chars, which is the very thing `name_width`
    // is measured in display columns to avoid.
    let padding = " ".repeat(columns.name_width.saturating_sub(proc.comm.width()));
    let cpu = match rate {
        Some(rate) => format!("{:>width$.1}%", rate.cpu_percent, width = CPU_WIDTH - 1),
        None => format!("{:>CPU_WIDTH$}", "-"),
    };
    // Blank at zero, `-` when unmeasured: an unprivileged masys cannot
    // read most processes' io at all, and showing that as `0B` would
    // claim they are idle.
    let io = match (rate, columns.split_io) {
        (Some(rate), true) => {
            format!(
                "{:>IO_WIDTH$}{:>IO_WIDTH$}",
                flow(rate.io_read_bytes_per_sec),
                flow(rate.io_write_bytes_per_sec)
            )
        }
        (Some(rate), false) => format!(
            "{:>IO_WIDTH$}",
            flow(rate.io_read_bytes_per_sec + rate.io_write_bytes_per_sec)
        ),
        (None, true) => format!("{:>IO_WIDTH$}{:>IO_WIDTH$}", "-", "-"),
        (None, false) => format!("{:>IO_WIDTH$}", "-"),
    };
    // Wall-clock age, not CPU time: a process up for a week having used
    // four seconds of CPU is a healthy daemon, and one up for forty
    // seconds is a service that just restarted. `TIME` cannot tell them
    // apart.
    let up = if columns.uptime {
        format!(
            "{:>UP_WIDTH$}",
            age_ms
                .map(human_duration)
                .unwrap_or_else(|| "-".to_string())
        )
    } else {
        String::new()
    };
    let ni = if columns.nice {
        format!("{:>NICE_WIDTH$}", notable(proc.nice))
    } else {
        String::new()
    };
    let oom = if columns.oom {
        format!("{:>OOM_WIDTH$}", notable(proc.oom_score))
    } else {
        String::new()
    };
    let mem = if proc.rss_bytes > 0 {
        human_bytes(proc.rss_bytes)
    } else {
        String::new()
    };
    Line::from(vec![
        Span::styled(
            format!("{:>PID_WIDTH$}  ", proc.pid),
            Style::default().fg(theme.info),
        ),
        Span::raw(format!(
            "{}{padding}{cpu}{mem:>MEM_WIDTH$}{io}{up}{ni}{oom}{:>THR_WIDTH$}{:>STATE_WIDTH$}{:>TIME_WIDTH$}",
            proc.comm,
            proc.threads,
            state_letter(proc.state),
            human_cpu_time(*cpu_seconds)
        )),
    ])
}

/// One ranking's rows, in the layout the terminal has room for.
///
/// The name column is widened to `columns.name_width` - which the caller
/// derived from the terminal - or to the widest name in the batch,
/// whichever is larger, so the numeric columns land in the same place for
/// every row. The floor is never per-batch: the status buffer renders two
/// rankings as separate batches that sit directly above one another, and
/// per-batch widths would put the same column in two different places on
/// one screen.
pub fn proc_lines(rows: &[ProcRow], theme: &Theme, columns: &ProcColumns) -> Vec<Line<'static>> {
    let widest = rows
        .iter()
        .map(|row| row.proc.comm.width())
        .max()
        .unwrap_or(0);
    let columns = ProcColumns {
        name_width: columns.name_width.max(widest).max(NAME_WIDTH),
        ..*columns
    };
    rows.iter()
        .map(|row| proc_line_padded(row, theme, &columns))
        .collect()
}

/// One cgroup, as the Procs buffer's group row: fold marker, name, and
/// the aggregates for everything under it.
///
/// The fold marker is `v`/`>` rather than a box-drawing glyph, matching
/// the design's mockup and magit's own outline - it survives a terminal
/// with no unicode and a font with no arrows, which a monitoring tool
/// running over a serial console has to.
///
/// Memory and IO blank at zero for the same reason they do on a process
/// row: kernel threads have neither, and a column of `0B` down the whole
/// list buries the groups that do.
///
/// `UP`, `NI` and `OOM` are left blank rather than aggregated. They are
/// facts about a single process - a cgroup has no nice value, and the
/// oldest member's age is not the group's age - and inventing a number
/// for them would make the group row lie in the one place it is meant to
/// summarise.
pub fn proc_group_line(node: &Node, theme: &Theme, columns: &ProcColumns) -> Line<'static> {
    let Node::ProcGroup {
        name,
        expanded,
        cpu_percent,
        mem_bytes,
        read_bytes_per_sec,
        write_bytes_per_sec,
        proc_count,
        ..
    } = node
    else {
        return Line::default();
    };
    let marker = if *expanded { "v" } else { ">" };
    // The leaf segment, not the whole path. A cgroup path is long and
    // front-loaded with the parts every row shares - truncating
    // `/user.slice/user-1000.slice/user@1000.service/app.slice/...` to
    // the column width yields `/user.slice/user-1000.`, which identifies
    // nothing. The full path stays the fold key; only the display shrinks.
    let leaf = name
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(name);
    let io = if columns.split_io {
        format!(
            "{:>IO_WIDTH$}{:>IO_WIDTH$}",
            flow(*read_bytes_per_sec),
            flow(*write_bytes_per_sec)
        )
    } else {
        format!(
            "{:>IO_WIDTH$}",
            flow(read_bytes_per_sec + write_bytes_per_sec)
        )
    };
    let mem = if *mem_bytes > 0 {
        human_bytes(*mem_bytes)
    } else {
        String::new()
    };
    let gap = columns.per_process_gap();
    Line::from(vec![
        Span::styled(
            format!("{marker} "),
            Style::default().fg(theme.section_header),
        ),
        Span::styled(
            // The name spans the PID and NAME columns; the rest line up
            // with the process rows beneath, and PROCS sits where THR
            // does because both answer "how many".
            format!(
                "{leaf:<width$.width$}{cpu_percent:>cpu$.1}%{mem:>MEM_WIDTH$}{io}{gap}{proc_count:>THR_WIDTH$}",
                width = PID_WIDTH + columns.name_width,
                cpu = CPU_WIDTH - 1
            ),
            Style::default()
                .fg(theme.section_header)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

/// A `[####------]` bar. The design's gauge widget, in the one place a
/// percentage is the whole point.
///
/// Filled cells round *down*, so a filesystem at 99.6% shows one empty
/// cell rather than a full bar: "nearly full" and "full" are different
/// operational states, and a bar that saturates early stops
/// distinguishing them exactly when it matters.
pub fn gauge(percent: f32, width: usize) -> String {
    let clamped = percent.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * width as f32).floor() as usize;
    let filled = filled.min(width);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(width - filled))
}

fn unit_glyph(state: ActiveState, theme: &Theme) -> (&'static str, Color) {
    match state {
        ActiveState::Failed => ("x", theme.severity_dead),
        // Mid-transition: not wrong, but not settled either, and a unit
        // stuck activating is a real fault that looks fine in a snapshot.
        ActiveState::Activating | ActiveState::Deactivating | ActiveState::Reloading => {
            ("~", theme.severity_warning)
        }
        ActiveState::Active | ActiveState::Inactive => (".", theme.info),
    }
}

/// The colour a whole unit row carries, or `None` for a healthy one.
///
/// On 366 rows a two-column glyph is a small signal in a wall of text.
/// Colouring the row is what lets a failure be found without reading it.
///
/// Active is deliberately *uncoloured* rather than given a colour of its
/// own: it is the overwhelming majority, and a colour on the common case
/// is a colour that means nothing. Inactive recedes instead, so the eye
/// passes over the units that are merely switched off and lands on the
/// ones that are wrong.
fn unit_row_style(state: ActiveState, theme: &Theme) -> Style {
    match state {
        ActiveState::Failed => Style::default().fg(theme.severity_dead),
        ActiveState::Activating | ActiveState::Deactivating | ActiveState::Reloading => {
            Style::default().fg(theme.severity_warning)
        }
        ActiveState::Inactive => Style::default().fg(theme.info),
        ActiveState::Active => Style::default(),
    }
}

/// One unit row: name, what it is doing, whether it is enabled, and how
/// long it has been that way.
///
/// `age_ms` arrives already computed, for the same reason
/// `masys_domain::triage` converts a finding's timestamp before the view
/// sees it: a renderer that reads the clock cannot be tested against a
/// fixed frame.
pub fn unit_line_at(row: &UnitRow, theme: &Theme, name_width: usize) -> Line<'static> {
    unit_line_indented(
        &row.unit,
        row.age_ms,
        row.expanded,
        row.children,
        row.depth,
        theme,
        name_width,
    )
}

#[allow(clippy::too_many_arguments)]
fn unit_line_indented(
    unit: &Unit,
    age_ms: Option<u64>,
    expanded: bool,
    children: bool,
    depth: u32,
    theme: &Theme,
    name_width: usize,
) -> Line<'static> {
    let (glyph, color) = unit_glyph(unit.active_state, theme);
    let state = match (unit.active_state, unit.exit_code) {
        (ActiveState::Failed, Some(code)) => format!("failed (exit {code})"),
        _ => unit.sub_state.clone(),
    };
    // `enabled` only earns a column when true: on a normal host most
    // units are static or generated, and a column of `disabled` down the
    // screen says nothing about any of them.
    let enabled = if unit.enabled { "enabled" } else { "" };
    // Blank rather than a number when systemd recorded no transition:
    // dating those from the epoch rendered `-.mount` as `20684d 14h`.
    let age = age_ms.map(human_duration).unwrap_or_default();
    // The fold marker gets a column of its own, so an open unit reads as
    // open and a closed one does not shift to say so. `v` and a blank
    // rather than `v`/`>`: every unit can open, so a marker on the closed
    // ones would be noise down all 366 rows. ASCII, like the fold markers
    // in the procs buffer, because a monitoring tool has to survive a serial
    // console with no unicode.
    // `v` when something is showing beneath this row - its detail, or the
    // subtree of a slice. `>` when a slice has a subtree that is shut, and
    // blank for a row with nothing under it: every unit can open its
    // detail, so a marker on all of them would be noise down 366 rows.
    let marker = if expanded || children {
        "v"
    } else if unit.kind == masys_domain::unit::UnitKind::Slice {
        ">"
    } else {
        " "
    };
    let indent = " ".repeat(indent_width(depth));
    let name = format!("{indent}{}", unit.name);
    Line::from(vec![
        Span::styled(format!("{glyph} {marker} "), Style::default().fg(color)),
        // Truncated by the format specifier's precision, which counts
        // chars rather than columns - acceptable here because systemd
        // unit names are ASCII by construction, unlike the hostnames and
        // mount points elsewhere in this module.
        Span::styled(
            format!("{name:<name_width$.name_width$}  {state:<20}{enabled:<9}{age:>8}"),
            unit_row_style(unit.active_state, theme),
        ),
    ])
}

/// A whole section's units, names padded to a common width.
/// Widest unit-name column. Unit names are unbounded - systemd escapes
/// device and mount paths into them, and this host has several over 130
/// characters - so padding to the longest in a 177-unit section pushes
/// the state column off the screen for all 176 others.
const UNIT_NAME_WIDTH: usize = 36;

/// One unit as the view needs it, including where it sits in a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitRow {
    pub unit: Unit,
    pub age_ms: Option<u64>,
    pub expanded: bool,
    pub depth: u32,
    pub children: bool,
}

/// A section's units, names padded to a common width.
///
/// The width is measured *including* each row's indentation, so a nested
/// tree keeps one straight column for the state rather than stepping it
/// right along with the names.
pub fn unit_lines(rows: &[UnitRow], theme: &Theme) -> Vec<Line<'static>> {
    let name_width = rows
        .iter()
        .map(|row| row.unit.name.width() + indent_width(row.depth))
        .max()
        .unwrap_or(0)
        .min(UNIT_NAME_WIDTH);
    rows.iter()
        .map(|row| unit_line_at(row, theme, name_width))
        .collect()
}

/// Two columns per level, matching how the procs buffer nests.
fn indent_width(depth: u32) -> usize {
    depth as usize * 2
}

/// `96103000ns -> "96ms"`, `4200000000ns -> "4.2s"` - systemd accounts CPU
/// in nanoseconds and `systemctl status` prints it scaled.
fn human_cpu_nsec(nsec: u64) -> String {
    match nsec {
        n if n >= 60_000_000_000 => {
            format!("{}m {}s", n / 60_000_000_000, (n / 1_000_000_000) % 60)
        }
        n if n >= 1_000_000_000 => format!("{:.1}s", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{}ms", n / 1_000_000),
        n => format!("{}us", n / 1_000),
    }
}

/// A unit's detail, drawn under its row when it is open.
///
/// The content `systemctl status` leads with, which is the set
/// `systemctl-tui` surfaces too - description, where it was loaded from,
/// and what it is running. Drawn in this tool's own idiom: a label column
/// on the left, so the eye finds `cgroup` without reading the line.
///
/// No log. `l` opens that in full, scrollable and searchable, and a
/// handful of lines duplicated here meant two places to read the same
/// thing and neither of them the good one.
///
/// A field the unit does not have is omitted rather than printed empty: a
/// `.target` has no main pid and a `.device` no memory accounting, and a
/// column of `-` down the block says nothing about any of them.
pub fn unit_detail_lines(
    unit: &Unit,
    detail: Option<&UnitDetail>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut facts: Vec<(&str, String)> = Vec::new();
    let detail = detail.cloned().unwrap_or_default();
    if let Some(description) = &detail.description {
        facts.push(("", description.clone()));
    }

    // Where it came from and whether it is on at boot - `systemctl
    // status`'s "Loaded:" line, which is one fact about the unit file.
    let boot = if unit.enabled {
        "enabled"
    } else {
        "not enabled"
    };
    let loaded = match &detail.fragment_path {
        Some(path) => format!("{path}  .  {boot}"),
        None => format!("no unit file  .  {boot}"),
    };
    facts.push(("loaded", loaded));

    // What it is currently running. Each part is skipped when the unit
    // type has no such thing.
    let mut running: Vec<String> = Vec::new();
    if let Some(pid) = detail.main_pid {
        running.push(format!("pid {pid}"));
    }
    if let Some(tasks) = detail.tasks {
        let limit = detail
            .tasks_max
            .map(|max| format!(" of {max}"))
            .unwrap_or_default();
        running.push(format!(
            "{tasks} {}{limit}",
            if tasks == 1 { "task" } else { "tasks" }
        ));
    }
    if let Some(bytes) = detail.memory_bytes {
        // The peak beside the current figure: a unit sitting at 4M having
        // peaked at 900M is a different unit from one that never moved.
        let peak = detail
            .memory_peak_bytes
            .filter(|peak| *peak > bytes)
            .map(|peak| format!(" (peak {})", human_bytes(peak)));
        running.push(format!(
            "{}{}",
            human_bytes(bytes),
            peak.unwrap_or_default()
        ));
    }
    if let Some(nsec) = detail.cpu_nsec {
        running.push(format!("cpu {}", human_cpu_nsec(nsec)));
    }
    if !running.is_empty() {
        facts.push(("running", running.join("  .  ")));
    }

    // Why it last stopped. `success` is the overwhelming default and says
    // nothing, so it earns no line; anything else is the headline fact
    // about a unit that is not running.
    if let Some(result) = detail.result.as_deref().filter(|r| *r != "success") {
        facts.push(("result", result.to_string()));
    }
    if let Some(code) = unit.exit_code {
        facts.push(("exit", format!("{code}")));
    }
    // Storage and network, when systemd is accounting them. Absent rather
    // than zero when it is not - "not measured" and "did nothing" are
    // different answers, the rule this whole tree follows.
    if let (Some(read), Some(written)) = (detail.io_read_bytes, detail.io_write_bytes)
        && read + written > 0
    {
        facts.push((
            "io",
            format!(
                "{} read  .  {} written",
                human_bytes(read),
                human_bytes(written)
            ),
        ));
    }
    if let (Some(rx), Some(tx)) = (detail.ip_ingress_bytes, detail.ip_egress_bytes)
        && rx + tx > 0
    {
        facts.push((
            "ip",
            format!("{} in  .  {} out", human_bytes(rx), human_bytes(tx)),
        ));
    }
    if !detail.documentation.is_empty() {
        facts.push(("docs", detail.documentation.join("  ")));
    }
    if let Some(cgroup) = detail.cgroup.as_ref().or(unit.cgroup.as_ref()) {
        facts.push(("cgroup", cgroup.clone()));
    }
    // The raw material for flapping, as the count triage works from rather
    // than a list of timestamps nobody reads.
    if !unit.restart_timestamps_ms.is_empty() {
        facts.push((
            "restarts",
            format!("{} in the last hour", unit.restart_timestamps_ms.len()),
        ));
    }
    if unit.since_ms > 0 {
        facts.push(("since", local_datetime(unit.since_ms)));
    }
    if let Some(timer) = &unit.timer {
        facts.push(("activates", timer.activates.clone()));
    }

    // Bounded for the same reason the process block is, though this one
    // is short enough that it never reaches the limit in practice.
    fit_block(facts)
        .into_iter()
        .map(|(label, value)| {
            // Only the labels that carry bad news are coloured. Colouring
            // every value would make the block a wall of colour, which is
            // the same as colouring none of it.
            let style = match label {
                "result" | "exit" => Style::default().fg(theme.severity_dead),
                _ => Style::default(),
            };
            Line::from(vec![
                Span::styled(format!("    {label:<10}"), Style::default().fg(theme.info)),
                Span::styled(value, style),
            ])
        })
        .collect()
}

/// One open descriptor, as it appears in the detail block.
///
/// A socket reads as its address rather than its inode: `socket:[38271]`
/// names a number, `tcp 0.0.0.0:5432 LISTEN` names the reason the process
/// is running.
fn fd_target_text(target: &FdTarget) -> String {
    match target {
        FdTarget::Path(path) => path.clone(),
        FdTarget::Tcp { local, peer, state } => match peer {
            Some(peer) => format!("tcp {local} -> {peer} {state}"),
            None => format!("tcp {local} {state}"),
        },
        FdTarget::Udp { local } => format!("udp {local}"),
        FdTarget::Unix { path, .. } => match path {
            Some(path) => format!("unix {path}"),
            // Almost every unix socket on a host is one of these, and
            // naming them all `unix` would be a wall of identical rows.
            None => "unix (unnamed)".to_string(),
        },
        FdTarget::Pipe => "pipe".to_string(),
        FdTarget::Other(raw) => raw.clone(),
    }
}

/// The three descriptors the shell hands every process, by the names
/// people actually use for them.
fn standard_stream(number: u32) -> &'static str {
    match number {
        0 => "stdin",
        1 => "stdout",
        _ => "stderr",
    }
}

/// The longest a detail block gets.
///
/// A readability limit, not a rendering one - the block is drawn one item
/// per line, so any length would *draw*. The limit is about what can be
/// reached: the cursor cannot rest inside a block, and moving it past one
/// jumps to the next real row, so lines below the fold of the frame can
/// be seen but never scrolled to. A block that claims to list four
/// hundred environment variables while thirty of them are reachable is
/// less honest than one that lists thirty and says so.
const DETAIL_MAX_LINES: usize = 24;

/// Trims a detail block to `DETAIL_MAX_LINES`, replacing what is cut with
/// a line saying how much.
///
/// The tail goes first, which is why the blocks below are ordered with
/// the identifying facts at the top and the long lists at the bottom: the
/// cut lands on the environment before the descriptors, and on the
/// descriptors before the command line.
///
/// The count is never silent - the same rule the footer's action row and
/// the key help were both taught after losing content the same way.
fn fit_block(mut facts: Vec<(&'static str, String)>) -> Vec<(&'static str, String)> {
    if facts.len() <= DETAIL_MAX_LINES {
        return facts;
    }
    // One line of the budget is spent saying what the rest of it cost.
    let kept = DETAIL_MAX_LINES - 1;
    let dropped = facts.len() - kept;
    facts.truncate(kept);
    facts.push(("", format!("... {dropped} more lines not shown")));
    facts
}

/// How many descriptors past the standard three and the listening sockets
/// the block lists before summarising.
///
/// A process can hold thousands, and a detail block that scrolls the
/// buffer off the screen has stopped being a detail block. The count line
/// above always states the true total, so what is dropped is visible
/// rather than silent - the rule the footer's action row is built on.
const FD_SAMPLE: usize = 6;

/// Everything an open process row shows beneath itself.
///
/// `detail` is `None` when the read failed - for an unprivileged masys
/// looking at another user's process, the common case. The block still
/// draws what `Proc` alone carries, because a process is still worth
/// looking at when only half of it is readable.
///
/// Environment values are shown verbatim, secrets included. This reads
/// `/proc/[pid]/environ`, which the kernel already restricts to the
/// process's own uid or `CAP_SYS_PTRACE`, so the block can only show an
/// operator what `cat` would show them anyway - and redacting it would
/// hide the value exactly when someone is debugging why a service has the
/// wrong one.
pub fn proc_detail_lines(
    proc: &Proc,
    detail: Option<&ProcDetail>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut facts: Vec<(&str, String)> = Vec::new();
    let detail = detail.cloned().unwrap_or_default();

    // The headline. `Proc::comm` is capped at 15 bytes by the kernel, so
    // this is the first line that says which `python3` this is.
    if let Some(cmdline) = &detail.cmdline {
        facts.push(("cmdline", cmdline.clone()));
    }
    // Separate from `cmdline`, which reports whatever `argv[0]` was set
    // to and can be a lie - on NixOS it also names the store path, and
    // therefore the generation.
    if let Some(exe) = &detail.exe {
        facts.push(("exe", exe.clone()));
    }
    if let Some(cwd) = &detail.cwd {
        facts.push(("cwd", cwd.clone()));
    }
    if let Some(cgroup) = &proc.cgroup {
        facts.push(("cgroup", cgroup.clone()));
    }

    // Resident beside mapped beside swapped: a process that has reserved
    // a lot and one that is using a lot look identical in RSS alone, and
    // one being squeezed shows only in swap.
    let mut memory = vec![format!("{} rss", human_bytes(proc.rss_bytes))];
    if let Some(virt) = detail.virt_bytes {
        memory.push(format!("{} virt", human_bytes(virt)));
    }
    if let Some(swap) = detail.swap_bytes.filter(|swap| *swap > 0) {
        memory.push(format!("{} swap", human_bytes(swap)));
    }
    facts.push(("memory", memory.join("  .  ")));

    // Cumulative totals, not the per-second rates the table already
    // carries. Absent rather than zero when unreadable, the rule the
    // whole tree follows.
    if proc.io_read_bytes + proc.io_write_bytes > 0 {
        facts.push((
            "io",
            format!(
                "{} read  .  {} written",
                human_bytes(proc.io_read_bytes),
                human_bytes(proc.io_write_bytes)
            ),
        ));
    }

    let mut process = vec![
        format!("{} threads", proc.threads),
        format!("nice {}", proc.nice),
        format!("oom {}", proc.oom_score),
    ];
    if let Some(ppid) = detail.ppid {
        process.insert(0, format!("parent {ppid}"));
    }
    facts.push(("process", process.join("  .  ")));
    if proc.started_at_ms > 0 {
        facts.push(("started", local_datetime(proc.started_at_ms)));
    }

    // Descriptors. The counts first, because "four hundred open files"
    // and "four hundred open sockets" are different problems, then the
    // ones worth naming: the three standard streams and anything
    // listening, which is a service's whole reason for running.
    if !detail.fds.is_empty() {
        let counts = detail.fd_counts();
        let mut summary = vec![format!("{} open", counts.total())];
        for (count, label) in [
            (counts.files, "files"),
            (counts.sockets, "sockets"),
            (counts.pipes, "pipes"),
            (counts.other, "other"),
        ] {
            if count > 0 {
                summary.push(format!("{count} {label}"));
            }
        }
        facts.push(("fds", summary.join("  .  ")));

        let listening: Vec<&Fd> = detail.listening().collect();
        for fd in detail.fds.iter().filter(|fd| fd.is_standard()) {
            facts.push((
                "",
                format!(
                    "{:<8}{}",
                    standard_stream(fd.number),
                    fd_target_text(&fd.target)
                ),
            ));
        }
        for fd in listening.iter().take(FD_SAMPLE) {
            facts.push((
                "",
                format!("{:<8}{}", fd.number, fd_target_text(&fd.target)),
            ));
        }
        // Never a silent truncation: the count line above states the
        // true total, and this says what is not shown.
        if listening.len() > FD_SAMPLE {
            facts.push((
                "",
                format!("... and {} more listening", listening.len() - FD_SAMPLE),
            ));
        }
    }

    // Last, and deliberately: it is the longest block here, and putting
    // it above the descriptors would push them off a short screen.
    if !detail.env.is_empty() {
        facts.push(("environ", format!("{} vars", detail.env.len())));
        for (key, value) in &detail.env {
            facts.push(("", format!("{key}={value}")));
        }
    }

    // Bounded, and never silently: a block long enough to run off the
    // frame has lines nobody can scroll to, and claiming to list them
    // would be worse than saying how many there are.
    fit_block(facts)
        .into_iter()
        .map(|(label, value)| {
            Line::from(vec![
                Span::styled(format!("    {label:<10}"), Style::default().fg(theme.info)),
                Span::raw(value),
            ])
        })
        .collect()
}

pub fn filesystem_line(
    fs: &Filesystem,
    expanded: bool,
    theme: &Theme,
    mount_width: usize,
) -> Line<'static> {
    // A read-only filesystem is the urgent case this buffer exists for -
    // the kernel does it after an I/O error and nothing announces it.
    // An open one takes the marker every foldable row in masys draws,
    // which outranks the quiet glyph: it is the affordance that says the
    // row has something inside it.
    let (glyph, color) = match (fs.read_only, expanded) {
        (true, _) => ("!", theme.severity_urgent),
        (false, true) => ("v", theme.info),
        (false, false) => (">", theme.info),
    };
    let padding = " ".repeat(mount_width.saturating_sub(fs.mount_point.width()));
    let inodes = if fs.inode_used_percent >= 1.0 {
        format!("{:.0}% inodes", fs.inode_used_percent)
    } else {
        String::new()
    };
    let flag = if fs.read_only { "  read-only" } else { "" };
    Line::from(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        Span::raw(format!(
            "{}{padding}  {}{:>5.0}%{:>9} free  {inodes:<11}{flag}",
            fs.mount_point,
            gauge(fs.used_percent, 10),
            fs.used_percent,
            human_bytes(fs.free_bytes)
        )),
    ])
}

pub fn filesystem_lines(filesystems: &[(Filesystem, bool)], theme: &Theme) -> Vec<Line<'static>> {
    let mount_width = filesystems
        .iter()
        .map(|(f, _)| f.mount_point.width())
        .max()
        .unwrap_or(0);
    filesystems
        .iter()
        .map(|(fs, expanded)| filesystem_line(fs, *expanded, theme, mount_width))
        .collect()
}

fn priority_style(priority: Priority, theme: &Theme) -> (&'static str, Color) {
    match priority {
        Priority::Emergency | Priority::Alert | Priority::Critical | Priority::Error => {
            ("!", theme.severity_urgent)
        }
        Priority::Warning => ("~", theme.severity_warning),
        Priority::Notice | Priority::Info | Priority::Debug => (".", theme.info),
    }
}

/// Exactly `n` ASCII digits, and what follows them.
fn digits(text: &str, n: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    (bytes.len() >= n && bytes[..n].iter().all(|b| b.is_ascii_digit())).then(|| &text[n..])
}

/// `.123456`, if it is there. Not consumed unless a digit follows the
/// dot, so a message opening `21:17:57.` keeps its full stop.
fn fraction(text: &str) -> &str {
    match text
        .strip_prefix('.')
        .and_then(|rest| digits(rest, 1).map(|_| rest))
    {
        Some(rest) => rest.trim_start_matches(|c: char| c.is_ascii_digit()),
        None => text,
    }
}

/// `Z`, `-0400` or `+02:00`, if it is there. Anything half-formed is left
/// where it is rather than half-eaten.
fn zone(text: &str) -> &str {
    if let Some(rest) = text.strip_prefix('Z') {
        return rest;
    }
    let Some(rest) = text
        .strip_prefix(['+', '-'])
        .and_then(|rest| digits(rest, 2))
    else {
        return text;
    };
    digits(rest.strip_prefix(':').unwrap_or(rest), 2).unwrap_or(text)
}

/// What follows the sender's own timestamp, when the message opens with
/// one: `YYYY-MM-DD`, a `T` or a space, `HH:MM:SS`, and optionally
/// fractional seconds and a zone.
fn after_sender_timestamp(message: &str) -> Option<&str> {
    let rest = digits(message, 4)?.strip_prefix('-')?;
    let rest = digits(rest, 2)?.strip_prefix('-')?;
    let rest = digits(rest, 2)?.strip_prefix(['T', ' '])?;
    let rest = digits(rest, 2)?.strip_prefix(':')?;
    let rest = digits(rest, 2)?.strip_prefix(':')?;
    Some(zone(fraction(digits(rest, 2)?)))
}

/// Drops the timestamp a daemon stamped onto its own line.
///
/// journald records when it received an entry, and masys draws that in a
/// column of its own - but a sender that also timestamps its text puts a
/// second copy in the message, with a date the column deliberately omits.
/// On this host that is 12% of all journal lines: syncthing, docker and
/// anything else logging in RFC 3339.
///
/// Only a *leading* stamp is dropped. A date inside a sentence is the
/// message, and cutting it would be editing what a unit said. A line that
/// is nothing but a stamp is left whole too - a blank row says less than
/// a redundant one.
fn strip_sender_timestamp(message: &str) -> &str {
    match after_sender_timestamp(message) {
        // Whitespace rather than a space: the otel collector separates
        // its fields with tabs, and it is the single largest source of
        // RFC 3339 lines on this host.
        Some(rest) if rest.starts_with(char::is_whitespace) && !rest.trim_start().is_empty() => {
            rest.trim_start()
        }
        _ => message,
    }
}

/// One journal line: local time, the owning unit, the message.
///
/// An entry with no `_SYSTEMD_UNIT` came from the kernel, and says so
/// rather than leaving the column blank - "no unit" and "the kernel" are
/// the same fact here, and naming it is more useful than a gap.
pub fn journal_line(
    entry: &Entry,
    theme: &Theme,
    unit_width: usize,
    scope: Option<&str>,
) -> Line<'static> {
    let (glyph, color) = priority_style(entry.priority, theme);
    // A row in a buffer already scoped to one unit says nothing by naming
    // it again - but `journalctl -u foo` also returns pid 1's messages
    // *about* foo, stamped `init.scope`, and those do have something to
    // say. So the column is printed only where it differs.
    let unit = match entry_unit(entry, scope) {
        Some(unit) => unit,
        None => {
            let message = strip_sender_timestamp(&entry.message).replace(['\n', '\t'], " ");
            return Line::from(vec![
                Span::styled(format!("{glyph} "), Style::default().fg(color)),
                Span::styled(
                    format!("{}  ", local_time(entry.timestamp_ms)),
                    Style::default().fg(theme.info),
                ),
                Span::raw(message),
            ]);
        }
    };
    let padding = " ".repeat(unit_width.saturating_sub(unit.width()));
    // Messages are arbitrary and carry newlines and tabs; a row is a row.
    // A tab draws as nothing at all, which glued tab-separated fields
    // together into `...-0600infoadapter/receiver.go:41`.
    let message = strip_sender_timestamp(&entry.message).replace(['\n', '\t'], " ");
    Line::from(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        Span::styled(
            format!("{}  ", local_time(entry.timestamp_ms)),
            Style::default().fg(theme.info),
        ),
        Span::raw(format!("{unit}{padding}  {message}")),
    ])
}

/// The unit a row has to name, or `None` when the buffer already names it.
fn entry_unit(entry: &Entry, scope: Option<&str>) -> Option<String> {
    // No `_SYSTEMD_UNIT` at all means the kernel, which is worth saying:
    // "no unit" and "the kernel" are the same fact here, and naming it is
    // more useful than a gap.
    let unit = entry.unit.clone().unwrap_or_else(|| "kernel".to_string());
    (Some(unit.as_str()) != scope).then_some(unit)
}

pub fn journal_lines(entries: &[Entry], theme: &Theme, scope: Option<&str>) -> Vec<Line<'static>> {
    let unit_width = entries
        .iter()
        .filter_map(|e| entry_unit(e, scope))
        .map(|unit| unit.width())
        // A single pathological unit name should not push every message
        // off the right edge.
        .max()
        .unwrap_or(0)
        .min(28);
    entries
        .iter()
        .map(|entry| journal_line(entry, theme, unit_width, scope))
        .collect()
}

/// One block device's throughput and how busy it is.
///
/// `busy` is the fraction of wall-clock time the device had a request in
/// flight - the number `iostat` calls `%util`. It is the one figure here
/// that says whether storage is the *bottleneck* rather than merely
/// active: a disk moving 2 MB/s at 100% busy is saturated, and the same
/// 2 MB/s at 3% busy is idle.
pub fn disk_line(
    disk: &Disk,
    rate: Option<DiskRate>,
    theme: &Theme,
    name_width: usize,
) -> Line<'static> {
    let (read, write, iops, busy) = match rate {
        Some(rate) => (
            per_second(rate.read_bytes_per_sec),
            per_second(rate.write_bytes_per_sec),
            format!("{:.0} io/s", rate.reads_per_sec + rate.writes_per_sec),
            format!("{:.0}%", rate.busy_percent),
        ),
        // Not yet measured, which is not the same as idle.
        None => (
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
        ),
    };
    // Saturation is the finding here, so it gets the warning tier once a
    // device is spending most of its time busy.
    let saturated = rate.map(|r| r.busy_percent >= 80.0).unwrap_or(false);
    let (glyph, color) = if saturated {
        ("^", theme.severity_warning)
    } else {
        (".", theme.info)
    };
    Line::from(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        Span::raw(format!(
            "{:<name_width$.name_width$}{read:>11}{write:>11}{iops:>11}{busy:>8} busy",
            disk.name
        )),
    ])
}

/// One directory inside an opened filesystem.
///
/// Indented by depth, with a bar showing its share of its *parent* - so a
/// long bar always reads as "this is where the level above went", whether
/// the level above is the filesystem or another directory.
pub fn dir_entry_line(row: &DirRow, theme: &Theme, name_width: usize) -> Line<'static> {
    let DirRow {
        path,
        bytes,
        depth,
        share,
        expanded,
        has_children,
    } = row;
    // The same marker `ProcGroup` and an open unit already draw, so a row
    // that can be opened looks the same everywhere in masys.
    let marker = match (has_children, expanded) {
        (true, true) => "v ",
        (true, false) => "> ",
        (false, _) => "  ",
    };
    // The leaf, not the whole path: the parent is the row above, and
    // repeating it pushes the only new word off the right edge.
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let indent = "  ".repeat(*depth as usize);
    Line::from(vec![
        Span::raw(format!("{indent}{marker}")),
        Span::raw(format!("{name:<name_width$.name_width$}")),
        Span::styled(
            format!("{:>9}  ", human_bytes(*bytes)),
            Style::default().fg(theme.section_header),
        ),
        Span::styled(gauge(*share * 100.0, 10), Style::default().fg(theme.info)),
    ])
}

/// One directory row's content, lifted out of its `Node`.
///
/// The same shape as `ProcRow`, and for the same reason: `Node` is not
/// `Clone`, and a batch has to own what it pads to a common width.
#[derive(Debug, Clone)]
pub struct DirRow {
    pub path: std::path::PathBuf,
    pub bytes: u64,
    pub depth: u32,
    pub share: f32,
    pub expanded: bool,
    pub has_children: bool,
}

/// Every directory row, with the name column sized to the widest of them.
pub fn dir_entry_lines(section: &[DirRow], theme: &Theme) -> Vec<Line<'static>> {
    let name_width = section
        .iter()
        .map(|row| {
            row.path
                .file_name()
                .map(|n| n.to_string_lossy().width())
                .unwrap_or(1)
                + 2 * row.depth as usize
        })
        .max()
        .unwrap_or(0)
        .clamp(8, 40);
    section
        .iter()
        .map(|row| dir_entry_line(row, theme, name_width))
        .collect()
}

/// One installed package: name, then version.
///
/// The version is dimmed, for the reason every secondary column in masys
/// is: the operator scanning this buffer is looking for a *name*, and a
/// column of versions at full weight competes with the thing being looked
/// for.
/// The column is padded by display width, not by `char` count. `{:<n}`
/// counts `char`s, and `package_lines` measures the column with
/// `.width()` - so on a name carrying a wide or combining character the
/// two disagreed and the version column stepped out of line.
/// `filesystem_line` pads the same way for the same reason.
pub fn package_line(package: &Package, theme: &Theme, name_width: usize) -> Line<'static> {
    let padding = " ".repeat(name_width.saturating_sub(package.name.width()));
    Line::from(vec![
        Span::raw(format!("{}{padding}  ", package.name)),
        Span::styled(package.version.clone(), Style::default().fg(theme.info)),
    ])
}

/// A section's packages, the name column sized to the widest of them.
///
/// Measured rather than a constant, the same rule every other columnar
/// section in masys follows: a fixed width either truncates
/// `linux-firmware` or leaves a gap on a host whose longest name is
/// `git`.
pub fn package_lines(packages: &[Package], theme: &Theme) -> Vec<Line<'static>> {
    let name_width = packages
        .iter()
        .map(|package| package.name.width())
        .max()
        .unwrap_or(0);
    packages
        .iter()
        .map(|package| package_line(package, theme, name_width))
        .collect()
}

/// One interface's throughput, and what it has been dropping.
///
/// Errors and drops are cumulative since boot, so they earn a column only
/// when they are non-zero: printing `0 drop` on every row teaches the eye
/// to skip the one column here that means something is wrong. The
/// development host's NIC carries transmit drops - 37 when this was
/// written, 76 on 2026-08-30 - which is exactly the fact worth surfacing.
///
/// A range rather than a figure, because this one *counts up*: a comment
/// citing the exact value of a live counter is stale the week after it is
/// written, and the claim that matters here is "non-zero and rising", not
/// the number.
///
/// This doc sat above `package_line` between 2026-08-30 and the review
/// that found it - the same reattachment that happened to `only_reads` in
/// masys-app, from the same cause: a new function inserted between a doc
/// block and the function it describes.
pub fn interface_line(
    interface: &Interface,
    rate: Option<NetRate>,
    theme: &Theme,
    name_width: usize,
) -> Line<'static> {
    let (down, up) = match rate {
        Some(rate) => (
            per_second(rate.rx_bytes_per_sec),
            per_second(rate.tx_bytes_per_sec),
        ),
        // Not yet measured, which is not the same as idle.
        None => ("-".to_string(), "-".to_string()),
    };
    let errs = interface.rx_errs + interface.tx_errs;
    let drops = interface.rx_drop + interface.tx_drop;
    let mut trouble = String::new();
    if drops > 0 {
        trouble.push_str(&format!("   {drops} drop"));
    }
    if errs > 0 {
        trouble.push_str(&format!("   {errs} err"));
    }
    // Anything dropping or erroring is the reason this section exists, so
    // it takes the warning tier rather than the quiet one.
    let (glyph, color) = if drops > 0 || errs > 0 {
        ("^", theme.severity_warning)
    } else {
        (".", theme.info)
    };
    Line::from(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        Span::raw(format!(
            "{:<name_width$.name_width$}{down:>11} down{up:>11} up{trouble}",
            interface.name
        )),
    ])
}

/// Every interface's row, with the name column padded to the widest of
/// them - the same batching `disk_lines` does, and for the same reason:
/// the padding is a property of the section, not of a row.
pub fn interface_lines(
    section: &[(Interface, Option<NetRate>)],
    theme: &Theme,
) -> Vec<Line<'static>> {
    let name_width = section
        .iter()
        .map(|(i, _)| i.name.width())
        .max()
        .unwrap_or(0)
        .min(24);
    section
        .iter()
        .map(|(interface, rate)| interface_line(interface, *rate, theme, name_width))
        .collect()
}

/// Blank at a measured zero, so an idle device does not fill the column
/// with `0B` - the rule every other rate column here follows.
///
/// Carries its own `/s`, because this section has no column header to
/// name the units and a bare `1.9K` beside a bare `14` says nothing about
/// which is bytes and which is operations.
fn per_second(bytes: f64) -> String {
    if bytes >= 1.0 {
        format!("{}/s", human_bytes(bytes as u64))
    } else {
        String::new()
    }
}

pub fn disk_lines(rows: &[(Disk, Option<DiskRate>)], theme: &Theme) -> Vec<Line<'static>> {
    let name_width = rows
        .iter()
        .map(|(d, _)| d.name.width())
        .max()
        .unwrap_or(0)
        .max(10);
    rows.iter()
        .map(|(disk, rate)| disk_line(disk, *rate, theme, name_width))
        .collect()
}

/// One timer's row: what the app resolved, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerRow {
    pub name: String,
    pub activates: String,
    pub next_in_ms: Option<u64>,
    pub last_ago_ms: Option<u64>,
    pub children: bool,
    pub activated_failed: bool,
}

/// One timer: when it next fires, when it last did, and whether that run
/// worked.
///
/// The failure marker is about the unit the timer *activates*, not the
/// timer itself. A timer stays `active` while the job it runs is broken,
/// which is exactly how a nightly job fails for a month unnoticed.
pub fn timer_line(row: &TimerRow, theme: &Theme, name_width: usize) -> Line<'static> {
    let (glyph, color) = if row.activated_failed {
        ("x", theme.severity_dead)
    } else {
        (".", theme.info)
    };
    // Every timer runs something, so every timer can open - `>` when shut
    // rather than a blank, unlike a unit row where most cannot.
    let marker = if row.children { "v" } else { ">" };
    let next = match row.next_in_ms {
        Some(left) => format!("in {}", human_duration(left)),
        // Not a fault - a monotonic timer whose reference point has
        // passed has no next elapse either - so it says what it is.
        None => "not scheduled".to_string(),
    };
    let last = match row.last_ago_ms {
        Some(ago) => format!("{} ago", human_duration(ago)),
        None => "never run".to_string(),
    };
    // The activated unit usually differs from the timer only in suffix,
    // so it is named only when it is not the obvious one.
    let tail = match row.name.strip_suffix(".timer") {
        Some(stem) if row.activates == format!("{stem}.service") => String::new(),
        _ => format!("  {}", row.activates),
    };
    Line::from(vec![
        Span::styled(format!("{glyph} {marker} "), Style::default().fg(color)),
        Span::styled(
            format!(
                "{:<name_width$.name_width$}  {next:<18}{last:<16}{tail}",
                row.name
            ),
            // A timer whose job is broken is the case the section exists
            // for, so the row carries it rather than only the glyph.
            if row.activated_failed {
                Style::default().fg(theme.severity_dead)
            } else {
                Style::default()
            },
        ),
    ])
}

pub fn timer_lines(rows: &[TimerRow], theme: &Theme) -> Vec<Line<'static>> {
    let name_width = rows
        .iter()
        .map(|r| r.name.width())
        .max()
        .unwrap_or(0)
        .min(32);
    rows.iter()
        .map(|row| timer_line(row, theme, name_width))
        .collect()
}

pub fn section_header_line(title: &str, count: Option<u32>, theme: &Theme) -> Line<'static> {
    let text = match count {
        Some(n) => format!("{title} ({n})"),
        None => title.to_string(),
    };
    Line::from(Span::styled(
        text,
        Style::default()
            .fg(theme.section_header)
            .add_modifier(Modifier::BOLD),
    ))
}

fn system_state_word(state: SystemState) -> &'static str {
    match state {
        SystemState::Running => "running",
        SystemState::Degraded => "degraded",
        SystemState::Maintenance => "maintenance",
        SystemState::Starting => "starting",
        SystemState::Stopping => "stopping",
        SystemState::Offline => "offline",
    }
}

/// How many cells the System block's bars get.
///
/// Narrower than the ten the filesystem rows use, and the same primitive:
/// what a bar means is the fraction it fills, not how many cells it
/// spends doing it. The filesystems have a column to themselves, while
/// three bars share one line here with the figures they annotate - and at
/// ten cells a terabyte host with every segment present runs past eighty
/// columns, which is the width the design targets.
const OVERVIEW_GAUGE_WIDTH: usize = 8;

/// A throughput figure for the System block, where a measured zero is an
/// answer and must look like one.
///
/// Distinct from `per_second`, which the device and interface rows use:
/// that one returns the empty string below a byte a second, so a quiet
/// device leaves its cells blank rather than filling a table with
/// `0B/s`. The overview has one figure per direction and a sentence
/// around it, and a blank there reads as a reading that failed.
fn moving_bytes(bytes_per_sec: f64) -> String {
    format!("{}/s", human_bytes(bytes_per_sec.max(0.0) as u64))
}

/// One `.`-separated item in the System block, with whatever masys
/// concluded about it.
///
/// `severity` is `None` for a fact nothing judges - a distro name, an
/// uptime - as opposed to `Severity::Unknown`, a reading that has a rule
/// but no measurement to apply it to. The two draw identically today and
/// the type still keeps them apart, because they are different states of
/// the thing being drawn rather than of the drawing: collapsing them
/// would put the renderer in the position of having decided that a fact
/// and an unmeasured reading are the same, which is not its call.
struct Segment {
    text: String,
    severity: Option<Severity>,
}

/// A fact with no rule behind it.
fn fact(text: impl Into<String>) -> Segment {
    Segment {
        text: text.into(),
        severity: None,
    }
}

/// A reading the session has judged.
fn judged(text: impl Into<String>, severity: Severity) -> Segment {
    Segment {
        text: text.into(),
        severity: Some(severity),
    }
}

/// The colour a severity draws in - the whole of what this crate decides
/// about one, and now the only place it decides it.
///
/// It never sees a figure, which is the point: the thresholds, the PSI
/// lines and the rule that compares them all live in masys-domain, and
/// the session applies them. If this function grew a number it would
/// become a second opinion, and the colour and the finding beneath it
/// could disagree.
///
/// They did, for months, by a route this doc did not cover: the overview
/// came through here and the findings did not. `finding_tail` picked a
/// `theme.severity_*` field per variant, so one PSI reading was
/// light-red on the System line and yellow on the row detailing it. Both
/// paths land here now, which is what makes the sentence above true of
/// the whole crate rather than of one caller.
///
/// `Normal` is the terminal's own foreground rather than a colour of its
/// own - `unit_row_style` draws a healthy unit the same way, and for the
/// same reason: a colour on the common case is a colour that means
/// nothing. That leaves it visibly apart from the dim grey the facts
/// around it use, which is what keeps "measured, and fine" from looking
/// like "never measured".
fn severity_style(severity: Option<Severity>, theme: &Theme) -> Style {
    match severity {
        // Nothing to say, in the two ways there are of having nothing to
        // say: no rule exists, or the rule's input was never read.
        None | Some(Severity::Unknown) => Style::default().fg(theme.info),
        Some(Severity::Normal) => Style::default(),
        Some(Severity::Warning) => Style::default().fg(theme.severity_warning),
        Some(Severity::Urgent) => Style::default().fg(theme.severity_urgent),
        // Reached only by findings. No `Reading` is ever `Dead` - a
        // reading is a figure against a threshold, and "stopped" is not a
        // point on that scale - which is why this crate drew failed units
        // from `theme.severity_dead` directly for as long as the domain
        // had no variant for the judgement it was making.
        Some(Severity::Dead) => Style::default().fg(theme.severity_dead),
    }
}

/// One System line: `. ` and the segments, `.`-separated, each in its own
/// colour.
///
/// Spans rather than one formatted string, which is what this was until
/// severity arrived: a single span can only carry a single style, so a
/// line built by `join` could never have said that one item on it was the
/// problem.
fn overview_line(segments: Vec<Segment>, theme: &Theme) -> Line<'static> {
    let plain = Style::default().fg(theme.info);
    let mut spans = vec![Span::styled(". ", plain)];
    for (index, segment) in segments.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  .  ", plain));
        }
        spans.push(Span::styled(
            segment.text,
            severity_style(segment.severity, theme),
        ));
    }
    Line::from(spans)
}

/// One line of the System section, styled.
///
pub fn overview_part_line(overview: &Overview, part: OverviewPart, theme: &Theme) -> Line<'static> {
    overview_line(overview_segments(overview, part), theme)
}

/// The segments one part of the overview draws.
///
/// **No emptiness filter here.** This builds what it is asked for, and
/// `Overview::parts` decides what to ask for - the two used to be one
/// function that built all seven groups and dropped the empty ones,
/// which was fine while the whole overview was a single row. Now that
/// each part is a row the cursor can land on, the count belongs to one
/// crate, and a second opinion here could put a blank highlighted line
/// on the screen.
fn overview_segments(overview: &Overview, part: OverviewPart) -> Vec<Segment> {
    match part {
        // What the host *is*, ahead of what it is doing. Two parts rather
        // than one because a CPU model is long enough to crowd the distro
        // and kernel off a narrow terminal, and those two identify the
        // machine.
        OverviewPart::Identity => {
            let mut segments = Vec::new();
            if let Some(machine) = &overview.machine {
                if !machine.distro.is_empty() {
                    segments.push(fact(machine.distro.clone()));
                }
                if !machine.kernel.is_empty() {
                    segments.push(fact(format!(
                        "linux {}{}",
                        machine.kernel,
                        if machine.arch.is_empty() {
                            String::new()
                        } else {
                            format!(" {}", machine.arch)
                        }
                    )));
                }
            }
            segments
        }
        OverviewPart::Hardware => {
            let mut segments = Vec::new();
            if let Some(machine) = &overview.machine {
                if !machine.cpu_model.is_empty() {
                    segments.push(fact(machine.cpu_model.clone()));
                }
                if machine.cpu_cores > 0 {
                    segments.push(fact(format!("{} cores", machine.cpu_cores)));
                }
            }
            segments
        }
        // Load, uptime and the unit count carry no severity, and the type
        // says so: there is no threshold for how long a host should have
        // been up, and a load average is a queue length whose meaning
        // depends on the core count. PSI is what masys judges instead.
        OverviewPart::Status => {
            let mut segments = vec![
                fact(system_state_word(overview.system_state)),
                // `-` for a count nobody took, which is the convention
                // every other unmeasured reading here follows. Never `0`,
                // which is a host that answered.
                fact(match overview.unit_count {
                    Some(count) => format!("{count} units"),
                    None => "- units".to_string(),
                }),
            ];
            if let (Some(l1), Some(l5), Some(l15)) =
                (overview.load_1, overview.load_5, overview.load_15)
            {
                segments.push(fact(format!("load {l1:.2} {l5:.2} {l15:.2}")));
            }
            if let Some(uptime) = overview.uptime_secs {
                segments.push(fact(format!("up {}", human_duration(uptime * 1000))));
            }
            segments
        }
        // The only three readings on this screen that are genuinely
        // percentages - so the only three that can carry a bar. CPU
        // leads, because "is this host busy right now" is the question
        // the load averages cannot answer: they are a queue length over
        // minutes.
        OverviewPart::Resources => {
            let mut segments = Vec::new();
            if let Some(cpu) = overview.cpu_percent {
                segments.push(judged(
                    format!(
                        "cpu {:.0}% {}",
                        cpu.value,
                        gauge(cpu.value, OVERVIEW_GAUGE_WIDTH)
                    ),
                    cpu.severity,
                ));
            }
            if let (Some(used), Some(total)) = (overview.mem_used_bytes, overview.mem_total_bytes) {
                // A total of zero is not a machine with no memory, it is
                // a reading that did not work - so it gets the figures
                // and no bar, rather than a bar drawn against nothing.
                let bar = match total > 0 {
                    true => format!(
                        " {}",
                        gauge(
                            used.value as f32 / total as f32 * 100.0,
                            OVERVIEW_GAUGE_WIDTH
                        )
                    ),
                    false => String::new(),
                };
                segments.push(judged(
                    format!(
                        "mem {}/{}{bar}",
                        human_bytes(used.value),
                        human_bytes(total)
                    ),
                    used.severity,
                ));
            }
            // zram is gauged but never coloured, and the two are not in
            // tension: it is a percentage, so it has a bar, and it has no
            // threshold, so the bar is drawn in the colour of the fact it
            // is. A full zram is the *normal* steady state of a zram-swap
            // host - the design says so where it puts `zram 100%` in this
            // section rather than behind a finding, "because alerting on
            // it directly would train the operator to ignore the tool".
            // Whether it is actually hurting is what memory pressure
            // answers, one line along.
            if let Some(zram) = overview.zram_percent {
                segments.push(fact(format!(
                    "zram {zram:.0}% {}",
                    gauge(zram, OVERVIEW_GAUGE_WIDTH)
                )));
            }
            segments
        }
        // What the machine is moving. Its own line: the gauged line above
        // is at its width budget, and these are neither percentages nor
        // judged, so they have nothing to sit beside there.
        //
        // No severity on either, deliberately. There is no threshold for
        // "too much traffic" - a host saturating its NIC during a backup
        // is working - so these are `fact`s, and the type has nowhere to
        // put a verdict even if somebody later decided there should be one.
        //
        // `moving_bytes` rather than `per_second`, which the device rows
        // use: that one renders anything under a byte a second as the
        // empty string, which is right in a column of many devices and
        // wrong in a sentence. Here the segment exists precisely because
        // the machine *was* measured, so an idle one has to say `0B/s` -
        // blanks either side of `in` would read as a figure that failed
        // to arrive.
        OverviewPart::Throughput => {
            let mut segments = Vec::new();
            if let Some(net) = overview.net_throughput {
                segments.push(fact(format!(
                    "net {} in  {} out",
                    moving_bytes(net.in_bytes_per_sec),
                    moving_bytes(net.out_bytes_per_sec)
                )));
            }
            if let Some(disk) = overview.disk_throughput {
                segments.push(fact(format!(
                    "disk {} in  {} out",
                    moving_bytes(disk.in_bytes_per_sec),
                    moving_bytes(disk.out_bytes_per_sec)
                )));
            }
            segments
        }
        // The figures the colours above come from, on the line beneath
        // them, so a coloured segment can be checked rather than trusted.
        // Named for the line rather than for PSI, because a swap quantity
        // shares it.
        //
        // Each segment names its own resource and carries `psi` itself,
        // rather than the line being headed `psi` once. The headed form
        // read `. psi cpu 61%  .  io 4%  .  mem 41%  .  swap 0B free`,
        // where the swap quantity that shares the line looked like a
        // fourth PSI resource - and swap has no PSI.
        //
        // The three are absent together, since they come from one kernel
        // feature, so a host with no PSI shows no pressure figures at all
        // rather than three zeroes.
        OverviewPart::Pressure => {
            let mut segments = Vec::new();
            if let Some(cpu) = overview.cpu_pressure {
                segments.push(judged(format!("cpu psi {:.0}%", cpu.value), cpu.severity));
            }
            if let Some(io) = overview.io_pressure {
                segments.push(judged(format!("io psi {:.0}%", io.value), io.severity));
            }
            if let Some(memory) = overview.memory_pressure {
                segments.push(judged(
                    format!("mem psi {:.0}%", memory.value),
                    memory.severity,
                ));
            }
            // Swap free sits here rather than beside the memory figure it
            // belongs to, because the line above is the gauged one and
            // three bars plus a quantity do not fit eighty columns. It is
            // a quantity and not a percentage, which is the same reason it
            // has no bar - so this is where the readings that cannot be
            // gauged collect.
            if let Some(swap) = overview.swap_free_bytes {
                segments.push(fact(format!("swap {} free", human_bytes(swap))));
            }
            segments
        }
        // The standing warnings. Each is a boolean somebody already ruled
        // on, and each now says so - a failing disk used to sit in a row
        // of unremarkable facts wearing the colour of the uptime beside
        // it.
        //
        // An unread SMART status draws no segment at all, which is how it
        // stays distinguishable from both a healthy disk and a failing
        // one. That is the `None` arm below doing nothing, and it is
        // deliberate.
        OverviewPart::Health => {
            let mut segments = vec![judged(
                match overview.clock_synced.value {
                    true => "clock synced",
                    false => "clock not synced",
                },
                overview.clock_synced.severity,
            )];
            if let Some(smart) = overview.smart_ok {
                segments.push(judged(
                    match smart.value {
                        true => "smart ok",
                        false => "smart failing",
                    },
                    smart.severity,
                ));
            }
            if let Some(reboot) = &overview.pending_reboot {
                segments.push(judged(
                    format!("reboot pending: {}", reboot.value.reason),
                    reboot.severity,
                ));
            }
            segments
        }
    }
}

/// One generation's row, lifted out of its `Node` so a whole section can
/// be measured before any of it is drawn. Same shape as `UnitRow`, and
/// for the same reason: `Node` is not `Clone`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRow {
    pub generation: Generation,
    /// How long ago it was created. `None` where the profile link's mtime
    /// could not be read, which renders `-`: dating it from the epoch
    /// would print `20684d`, and a `0` would be a reading rather than an
    /// admission that there is none.
    pub age_ms: Option<u64>,
}

/// Widest a generation's version column is allowed to grow. Nothing
/// bounds a label - it is whatever the store path carries - and the
/// system profile's are already 22 columns
/// (`26.11.20260804.e72e4f2`), so an unusual one must not push the
/// kernel off the right edge for every other row.
const GENERATION_LABEL_WIDTH: usize = 24;

/// The three variable columns of a generations section, measured across
/// the whole section.
///
/// Three, where `unit_lines` measures one: the id, the role and the
/// version all vary, and the profiles differ in every one of them - the
/// system profile's ids are already three digits wide, with a 22-column
/// version label, while the home profile's are narrower and carry no
/// version at all. Neither high-water mark bounds the other, and both
/// only ever climb, which is why this is measured rather than constant -
/// the same reason the Nix buffer's columns are sized the way the systemd
/// buffer's one keypress away are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GenerationColumns {
    pub id: usize,
    pub role: usize,
    pub label: usize,
}

impl GenerationColumns {
    pub fn measure(rows: &[GenerationRow]) -> Self {
        GenerationColumns {
            id: rows
                .iter()
                .map(|row| row.generation.id.to_string().width())
                .max()
                .unwrap_or(0),
            role: rows
                .iter()
                .map(|row| generation_role(&row.generation).width())
                .max()
                .unwrap_or(0),
            label: rows
                .iter()
                .map(|row| row.generation.label.as_deref().unwrap_or_default().width())
                .max()
                .unwrap_or(0)
                .min(GENERATION_LABEL_WIDTH),
        }
    }
}

/// Which of the two questions this generation answers. Both, on a host
/// with no reboot pending, which is why they are one column and not two
/// flags.
fn generation_role(generation: &Generation) -> &'static str {
    match (generation.current, generation.booted) {
        (true, true) => "current, booted",
        (true, false) => "current",
        (false, true) => "booted",
        (false, false) => "",
    }
}

/// One generation: which it is, how old, what version, and which kernel.
///
/// The marker column carries `*` for current and the role column names
/// `booted` where it differs, because "which one am I running" and "which
/// one will I get at the next boot" are two questions and a single
/// asterisk answers only one of them.
pub fn generation_line(
    generation: &Generation,
    age_ms: Option<u64>,
    theme: &Theme,
    columns: GenerationColumns,
) -> Line<'static> {
    let GenerationColumns {
        id: id_width,
        role: role_width,
        label: label_width,
    } = columns;
    let marker = if generation.current { "*" } else { " " };
    let role = generation_role(generation);
    // The age, not the timestamp: every other row in masys that carries a
    // time says how long ago, and `age_ms` arrives precomputed because
    // nothing in this module is allowed a clock. `-` for a link whose
    // mtime could not be read - the same shrug `disk_line` prints for a
    // rate it has not measured, and not a `0` that would read as one.
    let age = age_ms
        .map(human_duration)
        .unwrap_or_else(|| "-".to_string());
    let label = generation.label.clone().unwrap_or_default();
    let kernel = generation
        .kernel
        .as_deref()
        .and_then(kernel_version)
        .unwrap_or_default();
    // A generation that booted but is no longer current is what the
    // reboot notice at the top of the buffer is about, so it wears the
    // same tier here: the list says which row the notice means.
    let role_style = if generation.booted && !generation.current {
        Style::default().fg(theme.severity_warning)
    } else {
        Style::default().fg(theme.info)
    };
    Line::from(vec![
        Span::styled(
            format!("{marker} {:>id_width$}  ", generation.id),
            Style::default().fg(theme.section_header),
        ),
        Span::styled(format!("{role:<role_width$}  "), role_style),
        Span::styled(format!("{age:>8}  "), Style::default().fg(theme.info)),
        // Truncated by the format precision, which counts chars rather
        // than columns - as in `unit_line_indented`, and sound for the
        // same reason: a store path's name is ASCII by construction.
        Span::raw(format!("{label:<label_width$.label_width$}  ")),
        Span::styled(kernel, Style::default().fg(theme.info)),
    ])
}

/// A section's generations, every column sized to the widest of them.
pub fn generation_lines(rows: &[GenerationRow], theme: &Theme) -> Vec<Line<'static>> {
    let columns = GenerationColumns::measure(rows);
    rows.iter()
        .map(|row| generation_line(&row.generation, row.age_ms, theme, columns))
        .collect()
}

/// The pending-reboot block.
///
/// Two lines, and the second is the one that matters: every other tool on
/// the host prints "reboot required", and what an operator actually needs
/// to know is whether it can wait until Friday.
///
/// Un-indented, unlike every other row in the buffer, because it stands
/// where a section header would - `NixBuffer::rows` gives it no header to
/// sit under.
pub fn reboot_lines(node: &Node, theme: &Theme) -> Vec<Line<'static>> {
    let Node::RebootPending {
        booted,
        current,
        kernel_changed,
        initrd_changed,
    } = node
    else {
        return Vec::new();
    };
    // `?` for a store path that matched no generation on the profile - a
    // booted generation can be deleted out from under the running system,
    // and inventing a number for it would be worse than admitting the gap.
    let number = |id: &Option<u64>| {
        id.map(|id| id.to_string())
            .unwrap_or_else(|| "?".to_string())
    };
    // What differs, then what that means, on two lines rather than one:
    // together they run past the right edge of an 80-column terminal,
    // and the half that would be clipped is the half worth reading.
    let differs = match (*kernel_changed, *initrd_changed) {
        (true, true) => "kernel and initrd changed",
        (true, false) => "kernel changed",
        (false, true) => "kernel unchanged, initrd changed",
        (false, false) => "kernel unchanged",
    };
    let consequence = match (*kernel_changed, *initrd_changed) {
        (true, _) => "a reboot is needed to run the current kernel",
        (false, true) => "the initrd is loaded at boot - a reboot is needed to pick it up",
        (false, false) => "activation-only - no kernel or initrd change, this can wait",
    };
    // The distinction the row exists for, in the colour as well as the
    // words: an activation-only difference is not the alarm that a kernel
    // which is not the one running is.
    let tier = if *kernel_changed || *initrd_changed {
        theme.severity_urgent
    } else {
        theme.severity_warning
    };
    vec![
        Line::from(Span::styled("! reboot pending", Style::default().fg(tier))),
        Line::from(Span::styled(
            format!(
                "    booted {} . current {} . {differs}",
                number(booted),
                number(current)
            ),
            Style::default().fg(theme.info),
        )),
        Line::from(Span::styled(
            format!("    {consequence}"),
            Style::default().fg(theme.info),
        )),
    ]
}

/// How far behind an input has to be before its row is marked. Roughly
/// half a release cycle: NixOS releases every six months and supports one
/// for about seven, so an input this old predates the current release's
/// own point updates.
///
/// Display only - it marks a row, it does not raise a finding, which is
/// why it lives here rather than in `masys_domain::finding::Thresholds`.
const STALE_INPUT_DAYS: u32 = 90;

/// Widest an input's name column grows. Flake input names are short by
/// convention but nothing enforces it.
const INPUT_NAME_WIDTH: usize = 24;

/// One pinned input: how old it is, when it was last modified upstream,
/// and where it came from.
///
/// A transitive input is dimmed rather than hidden. A 235-day-old
/// `flake-compat` inherited through another flake is not the same finding
/// as a stale `nixpkgs` - one is somebody else's decision and the other
/// is this configuration's - and the row has to say which it is.
pub fn input_line(
    input: &Input,
    age_days: Option<u32>,
    theme: &Theme,
    name_width: usize,
) -> Line<'static> {
    // Unknown age is not a stale one: a channel records no upstream
    // timestamp at all, and marking it would be a finding invented out of
    // a missing reading.
    let stale = age_days.is_some_and(|days| days >= STALE_INPUT_DAYS);
    let (glyph, color) = if stale {
        ("^", theme.severity_warning)
    } else {
        (" ", theme.info)
    };
    let age = age_days
        .map(|days| format!("{days}d"))
        .unwrap_or_else(|| "-".to_string());
    // Seconds, from the lock. `-` where the lock records none, for the
    // same reason the age column shrugs rather than printing 1970.
    let date = input
        .last_modified_secs
        .map(|secs| local_date(secs.saturating_mul(1000)))
        .unwrap_or_else(|| "-".to_string());
    let origin = input.origin.clone().unwrap_or_else(|| "-".to_string());
    let style = if input.direct {
        Style::default()
    } else {
        Style::default().fg(theme.info)
    };
    Line::from(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        // Truncated by char count, as in `unit_line_indented`: an input
        // name comes from a flake's attribute set and is ASCII by
        // construction.
        Span::styled(
            format!(
                "{:<name_width$.name_width$}  {age:>5}  {date:<10}  {origin}",
                input.name
            ),
            style,
        ),
    ])
}

/// A section's inputs, names padded to a common width.
pub fn input_lines(rows: &[(Input, Option<u32>)], theme: &Theme) -> Vec<Line<'static>> {
    let name_width = rows
        .iter()
        .map(|(input, _)| input.name.width())
        .max()
        .unwrap_or(0)
        .min(INPUT_NAME_WIDTH);
    rows.iter()
        .map(|(input, age_days)| input_line(input, *age_days, theme, name_width))
        .collect()
}

/// The store block: how full `/nix` is, and what is holding it that way.
///
/// This buffer's equivalent of the Status buffer's System section - always
/// drawn, and carrying facts rather than a complaint - so it reads like
/// `overview_lines`: `.`-joined, in the same quiet tier, and several
/// lines in one block rather than a row apiece.
pub fn nix_store_lines(node: &Node, theme: &Theme) -> Vec<Line<'static>> {
    let Node::NixStore {
        used_percent,
        free_bytes,
        system_generations,
        home_generations,
        gc_roots,
    } = node
    else {
        return Vec::new();
    };
    // `-` until `/nix` has been matched among the sampled filesystems. A
    // store at `0%` with `0B` free is a reading; this is the absence of
    // one, and the two must not print the same.
    let used = used_percent
        .map(|percent| format!("{percent:.0}%"))
        .unwrap_or_else(|| "-".to_string());
    let free = free_bytes
        .map(human_bytes)
        .unwrap_or_else(|| "-".to_string());
    // And `-` for the three counts, for the same reason and against the
    // sharper version of the same mistake: `0 gc roots` and
    // `0 system generations` are not merely uninformative, they are
    // findings - nothing is pinning the store, there is nothing to roll
    // back to - and a read that never came back has not earned either.
    let count = |value: &Option<u32>| {
        value
            .map(|count| count.to_string())
            .unwrap_or_else(|| "-".to_string())
    };
    let retained = [
        format!("{} system generations", count(system_generations)),
        format!("{} home", count(home_generations)),
        format!("{} gc roots", count(gc_roots)),
    ];
    vec![
        Line::from(Span::styled(
            format!("/nix {used}  .  {free} free"),
            Style::default().fg(theme.info),
        )),
        Line::from(Span::styled(
            retained.join("  .  "),
            Style::default().fg(theme.info),
        )),
    ]
}

/// One declared maintenance job's row, lifted out of its `Node` so the
/// section can be measured. Same shape as `TimerRow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NixPolicyRow {
    pub job: String,
    pub unit: String,
    /// Three states - see `Node::NixPolicy::retention`, which this
    /// mirrors field for field.
    pub retention: Option<Option<String>>,
    pub next_in_ms: Option<u64>,
    pub last_ago_ms: Option<u64>,
    pub enabled: bool,
}

/// What a job keeps, as a column.
///
/// Blank for a job that was read and retains nothing. That is a real
/// answer, not a missing reading: nothing is retained by optimising at
/// all, and a `nix-gc.service` with no `--delete-older-than` has genuinely
/// been given no retention. The row then states the schedule without
/// claiming a policy.
///
/// `-` is reserved for the reading masys could not take, and the two must
/// not print the same. Blank says "this job keeps nothing", which is a
/// finding somebody would act on - it is why the column exists - and a
/// failed read has not earned it.
fn retention_text(retention: Option<Option<&str>>) -> String {
    match retention {
        Some(Some(keep)) => format!("keep {keep}"),
        Some(None) => String::new(),
        None => "-".to_string(),
    }
}

/// One maintenance job: what it keeps, when it last ran and when it runs
/// next.
///
/// The schedule comes from the job's timer unit and the retention from
/// the configuration, which is the whole reason the Nix buffer has this
/// section rather than pointing at the Timers one.
pub fn nix_policy_line(
    row: &NixPolicyRow,
    theme: &Theme,
    job_width: usize,
    retention_width: usize,
) -> Line<'static> {
    // A gc timer that is switched off is what this row exists to surface:
    // nothing else on the host reports that the store will now grow until
    // the disk fills.
    let (glyph, color) = if row.enabled {
        (".", theme.info)
    } else {
        ("~", theme.severity_warning)
    };
    let retention = retention_text(row.retention.as_ref().map(|keep| keep.as_deref()));
    // The same words the Timers section uses for the same two absences,
    // so a schedule reads the same in both buffers.
    let last = match row.last_ago_ms {
        Some(ago) => format!("last {} ago", human_duration(ago)),
        None => "never run".to_string(),
    };
    let next = match row.next_in_ms {
        Some(left) => format!("next in {}", human_duration(left)),
        None => "not scheduled".to_string(),
    };
    // Named only when it is not the obvious one, the same rule
    // `timer_line` applies to the unit a timer activates.
    let tail = if row.unit == format!("nix-{}.timer", row.job) {
        String::new()
    } else {
        format!("  {}", row.unit)
    };
    let disabled = if row.enabled { "" } else { "  disabled" };
    Line::from(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        Span::styled(
            format!(
                "{:<job_width$}  {retention:<retention_width$}  {last:<18}{next:<18}{tail}{disabled}",
                row.job
            ),
            if row.enabled {
                Style::default()
            } else {
                Style::default().fg(theme.severity_warning)
            },
        ),
    ])
}

/// The declared policy, job and retention columns sized to the widest of
/// them.
pub fn nix_policy_lines(rows: &[NixPolicyRow], theme: &Theme) -> Vec<Line<'static>> {
    let job_width = rows.iter().map(|row| row.job.width()).max().unwrap_or(0);
    let retention_width = rows
        .iter()
        .map(|row| retention_text(row.retention.as_ref().map(|keep| keep.as_deref())).width())
        .max()
        .unwrap_or(0);
    rows.iter()
        .map(|row| nix_policy_line(row, theme, job_width, retention_width))
        .collect()
}
