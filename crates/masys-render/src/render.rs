//! Frame layout: turns a `View` into ratatui widgets each draw.
//!
//! The line this draws against `crate::view` is content vs. chrome, not
//! pure vs. impure. `view.rs` owns what a *row* says - findings, section
//! headers, the Overview - and never sees a width, so it stays testable
//! one `Line` at a time. This module owns the frame around those rows:
//! the header line, the footer's separator and hint text, and the layout
//! that places all three. Text whose content depends on the terminal
//! width (the header's right-aligned timestamp, the `-` separator) can
//! only live here, because this is the only place the width is known.
//!
//! Takes `&View`, never `&App` - masys-render doesn't depend on
//! masys-app at all.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;

use masys_domain::finding::Finding;
use masys_view::{ActionGroup, ActionRow, Header, KeyBinding, Metrics, ModalView, Node, StatusLine, SwitchRow, View};

use crate::theme::Theme;
use crate::view;

fn header_lines(model: &View, width: u16, columns: &view::ProcColumns) -> Vec<Line<'static>> {
    match model.header {
        Header::Status { hostname, timestamp } => {
            let left = format!("masys - {hostname}");
            // Terminal columns, not bytes - the same measure
            // `view::finding_lines` pads by, and for the same reason:
            // systemd's pretty hostname (`hostnamectl --pretty`) is
            // arbitrary UTF-8, so a byte count would push the timestamp
            // off the right edge on any host that sets one.
            let pad = (width as usize).saturating_sub(left.width() + timestamp.width());
            vec![Line::from(format!("{left}{}{timestamp}", " ".repeat(pad)))]
        }
        // Built from the same column constants the rows are, so the
        // headings cannot drift out of alignment with what is under them.
        Header::Procs { sort, descending } => {
            vec![Line::from(format!("{ROW_INDENT}{}", view::proc_columns_header(sort, descending, columns)))]
        }
        // The unit on the left, which way it reads on the right - the
        // same shape as the Status header's clock, and padded the same
        // way, in terminal columns rather than bytes.
        Header::Log { unit, newest_first } => {
            let left = format!("log - {unit}");
            let right = if newest_first { "newest first" } else { "oldest first" };
            let pad = (width as usize).saturating_sub(left.width() + right.width());
            vec![Line::from(format!("{left}{}{right}", " ".repeat(pad)))]
        }
        Header::Titled(title) => vec![Line::from(title.to_string())],
    }
}

/// Two columns of leading space, as the mockup indents a section's rows
/// under their header. Lives here rather than in `crate::view` because
/// it is placement, not content - the same reason the header's padding
/// and the footer's separator are built in this module.
const ROW_INDENT: &str = "  ";

fn indented(line: Line<'static>) -> Line<'static> {
    let mut spans = vec![Span::raw(ROW_INDENT)];
    spans.extend(line.spans);
    Line::from(spans)
}

/// One non-`Finding` `Node`, as a list row. `Node::Overview` and the Nix
/// view's two blocks are the multi-line cases; `Finding` rows never come
/// through here - they are batched per section by `build_items` so their
/// labels can be aligned.
///
/// Exhaustive, with no `_` arm: the compiler pointing at this file is
/// what makes the next row type impossible to forget, and a catch-all
/// would trade that for a blank line nobody notices.
fn row_item(node: &Node, theme: &Theme, columns: &view::ProcColumns) -> ListItem<'static> {
    match node {
        // The one row that is *not* indented: it is what the others are
        // indented under.
        Node::SectionHeader { title, count, .. } => ListItem::new(view::section_header_line(title, *count, theme)),
        Node::Overview(overview) => ListItem::new(view::overview_lines(overview, theme).into_iter().map(indented).collect::<Vec<_>>()),
        // Several lines in one item, and never highlighted: the cursor
        // cannot rest here, so the selection style never reaches it.
        // Handled by build_items, never reaches here.
        Node::UnitDetail { .. } | Node::ProcDetail { .. } => ListItem::new(Line::default()),
        Node::Spacer => ListItem::new(Line::default()),
        // Handled by build_items, never reaches here.
        Node::Finding(finding) => ListItem::new(indented(view::finding_line(finding, theme))),
        // Handled by build_items, never reaches here.
        Node::Proc { .. } => ListItem::new(Line::default()),
        Node::ProcGroup { .. } => ListItem::new(view::proc_group_line(node, theme, columns)),
        Node::JournalEntry(_)
        | Node::Unit { .. }
        | Node::Filesystem { .. }
        | Node::DirEntry { .. }
        | Node::Disk { .. }
        | Node::Timer { .. }
        | Node::Interface { .. }
        // The three columnar Nix rows, batched per section by
        // `build_items` for the same reason units and timers are.
        | Node::Generation { .. }
        | Node::Input { .. }
        | Node::NixPolicy { .. } => ListItem::new(Line::default()),
        // The Nix view's two blocks. Several lines in one item, like
        // `Overview`, because each is one statement rather than a list.
        // The reboot block is the only row in the buffer that is *not*
        // indented: `build_nix_rows` gives it no section header, so it
        // stands where a header would.
        Node::RebootPending { .. } => ListItem::new(view::reboot_lines(node, theme)),
        Node::NixStore { .. } => ListItem::new(view::nix_store_lines(node, theme).into_iter().map(indented).collect::<Vec<_>>()),
    }
}

/// One `ListItem` per `Node`, with consecutive `Finding`s gathered into
/// per-section batches on the way.
///
/// `view::finding_lines` pads every label in the slice it is given to one
/// common width, so it has to be handed exactly one section: a wider slice
/// over-pads the narrow section, a narrower one (a single finding at a
/// time) gives up alignment altogether. A *maximal run of consecutive*
/// `Node::Finding` is exactly that section, because
/// `masys_app::status::build_status_rows` emits a `SectionHeader` ahead of
/// every section's findings, so no two sections' findings can ever end up
/// adjacent in `nodes`. That header alone is what carries the invariant -
/// the `Spacer` `build_status_rows` also emits after each section is
/// visual spacing, and runs would still split correctly without it.
///
/// Same shape as `wagit-render`'s `build_rows`, which batches consecutive
/// diff lines per hunk for the same reason: the property being computed
/// belongs to the run, not to the row.
///
/// Returns the items and, beside them, where each *node* starts among
/// them. The two used to be index-for-index, and a detail block used to
/// be one tall item - until it turned out that ratatui does not clip a
/// `ListItem` taller than the list, it declines to draw it. Every process
/// launched from a shell carries twenty-odd environment variables, which
/// put the block over that line and made an opened row look inert.
///
/// So a detail block is now one item *per line*: no single item can ever
/// be too tall, and the list scrolls through the block like any other
/// rows. The cost is that a node index is no longer an item index, which
/// is what the second return value exists to fix - the cursor is a node
/// index, and handing it to the list untranslated would select the wrong
/// row the moment anything was open above it.
fn build_items<'a>(
    nodes: &[Node],
    theme: &Theme,
    columns: &view::ProcColumns,
    scope: Option<&'a str>,
) -> (Vec<ListItem<'static>>, Vec<usize>) {
    let mut items = Vec::with_capacity(nodes.len());
    let mut first_item: Vec<usize> = Vec::with_capacity(nodes.len());
    let mut index = 0;
    while index < nodes.len() {
        match &nodes[index] {
            Node::Finding(_) => {
                // Cloned rather than borrowed because `finding_lines`
                // takes a contiguous `&[Finding]`, which cannot be carved
                // out of a `&[Node]`. A section holds a handful of
                // findings, so the copy costs nothing per frame.
                let mut section: Vec<Finding> = Vec::new();
                while let Some(Node::Finding(finding)) = nodes.get(index) {
                    section.push(finding.clone());
                    index += 1;
                }
                // One item per finding, not one per section: `Node` and
                // `ListItem` stay index-for-index, which is what a
                // `selected` cursor will need.
                // One item per row in the batch, so these nodes keep a
                // one-to-one place among the items.
                first_item.extend(items.len()..items.len() + section.len());
                items.extend(view::finding_lines(&section, theme).into_iter().map(indented).map(ListItem::new));
            }
            // Batched for the same reason findings are: `view::proc_lines`
            // pads every name in the slice it is handed to one width, so
            // it has to see a whole ranking at once.
            Node::Proc { .. } => {
                let mut section: Vec<view::ProcRow> = Vec::new();
                while let Some(Node::Proc { proc, rate, cpu_seconds, age_ms, .. }) = nodes.get(index) {
                    section.push(view::ProcRow { proc: proc.clone(), rate: *rate, cpu_seconds: *cpu_seconds, age_ms: *age_ms });
                    index += 1;
                }
                // One item per row in the batch, so these nodes keep a
                // one-to-one place among the items.
                first_item.extend(items.len()..items.len() + section.len());
                items.extend(view::proc_lines(&section, theme, columns).into_iter().map(indented).map(ListItem::new));
            }
            // Batched for the same reason findings and processes are:
            // each of these pads a column across a whole section, so the
            // builder has to see the section rather than one row.
            Node::Timer { .. } => {
                let mut section: Vec<view::TimerRow> = Vec::new();
                while let Some(Node::Timer { name, activates, next_in_ms, last_ago_ms, children, activated_failed }) = nodes.get(index) {
                    section.push(view::TimerRow {
                        name: name.clone(),
                        activates: activates.clone(),
                        next_in_ms: *next_in_ms,
                        last_ago_ms: *last_ago_ms,
                        children: *children,
                        activated_failed: *activated_failed,
                    });
                    index += 1;
                }
                // One item per row in the batch, so these nodes keep a
                // one-to-one place among the items.
                first_item.extend(items.len()..items.len() + section.len());
                items.extend(view::timer_lines(&section, theme).into_iter().map(indented).map(ListItem::new));
            }
            Node::Unit { .. } => {
                let mut section: Vec<view::UnitRow> = Vec::new();
                while let Some(Node::Unit { unit, age_ms, expanded, depth, children }) = nodes.get(index) {
                    section.push(view::UnitRow {
                        unit: unit.clone(),
                        age_ms: *age_ms,
                        expanded: *expanded,
                        depth: *depth,
                        children: *children,
                    });
                    index += 1;
                }
                // One item per row in the batch, so these nodes keep a
                // one-to-one place among the items.
                first_item.extend(items.len()..items.len() + section.len());
                items.extend(view::unit_lines(&section, theme).into_iter().map(indented).map(ListItem::new));
            }
            Node::Disk { .. } => {
                let mut section: Vec<(masys_domain::sample::Disk, Option<masys_domain::rate::DiskRate>)> = Vec::new();
                while let Some(Node::Disk { disk, rate }) = nodes.get(index) {
                    section.push((disk.clone(), *rate));
                    index += 1;
                }
                // One item per row in the batch, so these nodes keep a
                // one-to-one place among the items.
                first_item.extend(items.len()..items.len() + section.len());
                items.extend(view::disk_lines(&section, theme).into_iter().map(indented).map(ListItem::new));
            }
            Node::Interface { .. } => {
                let mut section: Vec<(masys_domain::sample::Interface, Option<masys_domain::rate::NetRate>)> = Vec::new();
                while let Some(Node::Interface { interface, rate }) = nodes.get(index) {
                    section.push((interface.clone(), *rate));
                    index += 1;
                }
                // One item per row in the batch, so these nodes keep a
                // one-to-one place among the items.
                first_item.extend(items.len()..items.len() + section.len());
                items.extend(view::interface_lines(&section, theme).into_iter().map(indented).map(ListItem::new));
            }
            Node::DirEntry { .. } => {
                let mut section: Vec<view::DirRow> = Vec::new();
                while let Some(Node::DirEntry { path, bytes, depth, share, expanded, has_children }) = nodes.get(index) {
                    section.push(view::DirRow {
                        path: path.clone(),
                        bytes: *bytes,
                        depth: *depth,
                        share: *share,
                        expanded: *expanded,
                        has_children: *has_children,
                    });
                    index += 1;
                }
                first_item.extend(items.len()..items.len() + section.len());
                items.extend(view::dir_entry_lines(&section, theme).into_iter().map(indented).map(ListItem::new));
            }
            Node::Filesystem { .. } => {
                let mut section: Vec<(masys_domain::sample::Filesystem, bool)> = Vec::new();
                while let Some(Node::Filesystem { filesystem: fs, expanded }) = nodes.get(index) {
                    section.push((fs.clone(), *expanded));
                    index += 1;
                }
                // One item per row in the batch, so these nodes keep a
                // one-to-one place among the items.
                first_item.extend(items.len()..items.len() + section.len());
                items.extend(view::filesystem_lines(&section, theme).into_iter().map(indented).map(ListItem::new));
            }
            Node::JournalEntry(_) => {
                let mut section: Vec<masys_domain::journal::Entry> = Vec::new();
                while let Some(Node::JournalEntry(entry)) = nodes.get(index) {
                    section.push(entry.clone());
                    index += 1;
                }
                // One item per row in the batch, so these nodes keep a
                // one-to-one place among the items.
                first_item.extend(items.len()..items.len() + section.len());
                items.extend(view::journal_lines(&section, theme, scope).into_iter().map(indented).map(ListItem::new));
            }
            // The Nix view's three columnar sections, batched for the
            // reason findings and units are: each pads a column across a
            // whole run, so the builder has to see the run.
            //
            // A maximal run is exactly one section here too.
            // `masys_app::nix_buffer::build_nix_rows` emits a
            // `SectionHeader` ahead of every profile's generations, so
            // two profiles' generations can never end up adjacent and the
            // home profile's ids are never padded to the system
            // profile's.
            Node::Generation { .. } => {
                let mut section: Vec<view::GenerationRow> = Vec::new();
                while let Some(Node::Generation { generation, age_ms, .. }) = nodes.get(index) {
                    section.push(view::GenerationRow { generation: generation.clone(), age_ms: *age_ms });
                    index += 1;
                }
                // One item per row in the batch, so these nodes keep a
                // one-to-one place among the items.
                first_item.extend(items.len()..items.len() + section.len());
                items.extend(view::generation_lines(&section, theme).into_iter().map(indented).map(ListItem::new));
            }
            Node::Input { .. } => {
                let mut section: Vec<(masys_domain::declarative::Input, Option<u32>)> = Vec::new();
                while let Some(Node::Input { input, age_days }) = nodes.get(index) {
                    section.push((input.clone(), *age_days));
                    index += 1;
                }
                // One item per row in the batch, so these nodes keep a
                // one-to-one place among the items.
                first_item.extend(items.len()..items.len() + section.len());
                items.extend(view::input_lines(&section, theme).into_iter().map(indented).map(ListItem::new));
            }
            Node::NixPolicy { .. } => {
                let mut section: Vec<view::NixPolicyRow> = Vec::new();
                while let Some(Node::NixPolicy { job, unit, retention, next_in_ms, last_ago_ms, enabled }) = nodes.get(index) {
                    section.push(view::NixPolicyRow {
                        job: job.clone(),
                        unit: unit.clone(),
                        retention: retention.clone(),
                        next_in_ms: *next_in_ms,
                        last_ago_ms: *last_ago_ms,
                        enabled: *enabled,
                    });
                    index += 1;
                }
                // One item per row in the batch, so these nodes keep a
                // one-to-one place among the items.
                first_item.extend(items.len()..items.len() + section.len());
                items.extend(view::nix_policy_lines(&section, theme).into_iter().map(indented).map(ListItem::new));
            }
            // The two blocks that are several lines: one item each, so a
            // long one scrolls rather than vanishing.
            Node::UnitDetail { unit, detail } => {
                first_item.push(items.len());
                items.extend(view::unit_detail_lines(unit, detail.as_deref(), theme).into_iter().map(indented).map(ListItem::new));
                index += 1;
            }
            Node::ProcDetail { proc, detail } => {
                first_item.push(items.len());
                items.extend(view::proc_detail_lines(proc, detail.as_deref(), theme).into_iter().map(indented).map(ListItem::new));
                index += 1;
            }
            node => {
                first_item.push(items.len());
                items.push(row_item(node, theme, columns));
                index += 1;
            }
        }
    }
    (items, first_item)
}

/// The footer's offer, from what the session says is available rather
/// than a hardcoded string. It used to be hardcoded per header variant,
/// which meant it could not tell the truth about a keymap it did not know
/// - and it advertised `[r]estart` and `[l]ogs`, neither of which exists.
fn footer_hint_spans(model: &View, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for hint in model.hints {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        let text = format!("[{}] {}", hint.chord, hint.label);
        // The view you are in is marked rather than removed, so the row
        // stays the same width and the same keys stay in the same places.
        // Reversed rather than merely bold: this is the one thing on the
        // footer that answers "where am I", and it has to survive a glance
        // at a row of eight similar-looking entries.
        let style = if hint.active { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default().fg(theme.info) };
        spans.push(Span::styled(text, style));
    }
    spans
}

/// How many rows of keys the footer will spend on itself.
///
/// Wrapping without a cap would let a long enough action set eat the
/// list it is describing. Three holds the systemd view's thirteen keys
/// at 80 columns, which is the widest set masys offers and the narrowest
/// terminal it targets.
const MAX_ACTION_ROWS: usize = 3;

/// Which keys land on which row, and whether any were left over.
struct ActionRows<'a> {
    rows: Vec<Vec<&'a KeyBinding>>,
    clipped: bool,
}

/// Fits the buffer's keys into rows - layout only, no styling, so the
/// footer's height can be settled before a theme is in hand.
///
/// The keys wrap rather than truncating at the right margin. Truncation
/// silently drops the rightmost keys, which is how the action keys went
/// missing twice before, and on the systemd view the rightmost key is
/// `[l] logs` - the one most worth reaching. Past `MAX_ACTION_ROWS` the
/// last row is still marked, because a cap that hides keys without
/// saying so is the same failure one row further out.
fn wrap_actions<'a>(actions: &'a [KeyBinding], width: u16) -> ActionRows<'a> {
    let mut rows: Vec<Vec<&'a KeyBinding>> = Vec::new();
    let mut row: Vec<&'a KeyBinding> = Vec::new();
    let mut used = 0usize;

    for binding in actions {
        let text_width = binding.chord.width() + binding.label.width() + 3;
        let sep = if row.is_empty() { 0 } else { 2 };
        // Only the last row masys will draw has to keep four columns
        // free for the marker; the rows above it can use the full width.
        let last_row = rows.len() + 1 == MAX_ACTION_ROWS;
        let reserve = if last_row { 4 } else { 0 };
        if !row.is_empty() && used + sep + text_width + reserve > width as usize {
            if last_row {
                rows.push(row);
                return ActionRows { rows, clipped: true };
            }
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        used += if row.is_empty() { 0 } else { 2 } + text_width;
        row.push(binding);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    ActionRows { rows, clipped: false }
}

/// The open buffer's own keys, as spans so the inapplicable ones can be
/// dimmed rather than removed.
///
/// A key that vanishes when it does not apply punishes muscle memory and
/// leaves the operator wondering whether they misremembered it; a dimmed
/// one in its usual place answers that on sight. The design's rule for
/// action rows, applied here.
fn footer_action_lines(model: &View, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let wrapped = wrap_actions(model.actions, width);
    let last = wrapped.rows.len().saturating_sub(1);
    wrapped
        .rows
        .iter()
        .enumerate()
        .map(|(n, row)| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for binding in row {
                if !spans.is_empty() {
                    spans.push(Span::raw("  "));
                }
                let style = if binding.dimmed {
                    Style::default().fg(theme.info).add_modifier(Modifier::DIM)
                } else {
                    Style::default().fg(theme.section_header)
                };
                spans.push(Span::styled(format!("[{}] {}", binding.chord, binding.label), style));
            }
            if wrapped.clipped && n == last {
                spans.push(Span::raw(" ..."));
            }
            Line::from(spans)
        })
        .collect()
}

fn footer_lines(model: &View, theme: &Theme, height: u16, width: u16) -> Vec<Line<'static>> {
    match model.status {
        StatusLine::Hints => {
            let mut lines = vec![Line::from("-".repeat(width as usize))];
            lines.extend(footer_action_lines(model, width, theme));
            lines.push(Line::from(footer_hint_spans(model, theme)));
            lines
        }
        StatusLine::Busy => vec![Line::from(Span::styled("Working...", Style::default().fg(theme.section_header)))],
        StatusLine::Message(text) => vec![Line::from(text.to_string())],
        StatusLine::Error(text) => text
            .lines()
            .take(height as usize)
            .map(|line| Line::from(Span::styled(line.to_string(), Style::default().fg(theme.status_error))))
            .collect(),
    }
}

/// What the footer *wants*, before the frame gets a say. A multi-line
/// `StatusLine::Error` is unbounded (it's an action's raw stderr), so this
/// is only ever a request - `render` clamps it against the frame height.
fn footer_height(model: &View, width: u16) -> u16 {
    match model.status {
        // Separator, the buffer's own keys - however many rows they wrap
        // to - and the row of jumps and global verbs.
        StatusLine::Hints => 2 + wrap_actions(model.actions, width).rows.len() as u16,
        StatusLine::Busy | StatusLine::Message(_) => 1,
        StatusLine::Error(text) => text.lines().count().max(1) as u16,
    }
}

/// Draws one frame: header, the buffer's rows as a list, and a footer -
/// no modal yet (nothing opens one), no persisted `ListState` yet (no
/// cursor exists to keep in view). Returns what it measured, for the
/// session to size its own paging once it tracks one.
pub fn render(frame: &mut Frame, model: &View, theme: &Theme) -> Metrics {
    // The mockup insets the whole frame by one column on each side, so
    // no text ever sits flush against the terminal edge. Everything below
    // measures against this area, not `frame.area()` - the header's
    // right-aligned timestamp and the footer's separator both derive
    // their width from it.
    let area = frame.area().inner(Margin::new(1, 0));
    let width = area.width;
    // The one place the terminal's width becomes a table layout. Every
    // row is drawn inside `ROW_INDENT`, so the table gets what is left
    // after it - and the header, which prefixes the same indent by hand,
    // is laid out from the identical answer.
    let columns = view::ProcColumns::fit((width as usize).saturating_sub(ROW_INDENT.len()));
    let mut header = header_lines(model, width, &columns);
    // Directly under the header, above the rows it is narrowing - and it
    // stays there while they move, which a centred popup could not do
    // without covering them.
    if let Some(filter) = model.filter {
        let mut spans = vec![
            Span::styled("/", Style::default().fg(theme.section_header)),
            Span::raw(filter.to_string()),
            // A block only while it is being typed, so a filter that is
            // merely *in effect* does not look like it is taking keys.
            Span::raw(if model.typing { "\u{2588}" } else { "" }),
        ];
        // The count as you type. A live filter over 353 rows narrows to
        // nothing one keystroke before you notice, and the rows going
        // blank looks the same whether the query matched nothing or the
        // buffer was empty to begin with.
        //
        // Coloured only when it is zero: a number that is always coloured
        // is a number nobody reads, and zero is the one answer worth
        // interrupting for.
        if let Some(matched) = model.filter_matches {
            let (text, style) = match matched {
                0 => ("no matches".to_string(), Style::default().fg(theme.severity_warning)),
                1 => ("1 match".to_string(), Style::default().fg(theme.info)),
                many => (format!("{many} matches"), Style::default().fg(theme.info)),
            };
            spans.push(Span::raw("   "));
            spans.push(Span::styled(text, style));
        }
        header.push(Line::from(spans));
    }
    // Directly under the header for the same reason the filter is: it
    // describes what the rows below are doing, and it has to stay visible
    // while they sit still. A frozen view that does not say it is frozen
    // is indistinguishable from a machine that has stopped answering.
    if model.auto_refresh_paused {
        header.push(Line::from(Span::styled(
            "paused - a process row is open.  fold it to resume, g to refresh once",
            Style::default().fg(theme.severity_warning),
        )));
    }
    // A blank line separates the header from the first row, matching the
    // mockup - part of the header area, not a `Spacer` node, so nothing
    // can navigate onto it.
    let header_height = header.len() as u16 + 1;
    // A long enough `StatusLine::Error` would otherwise claim the whole
    // frame through its `Constraint::Length` and leave no header and no
    // list at all. Reserve the header plus one list row, and let the
    // error take what's left - `footer_lines` truncates to the height it
    // is handed, so clamping here is what makes that `take` do any work.
    let footer_height = footer_height(model, width).min(area.height.saturating_sub(header_height + 1));

    let [header_area, list_area, footer_area] =
        Layout::vertical([Constraint::Length(header_height), Constraint::Min(0), Constraint::Length(footer_height)]).areas(area);

    frame.render_widget(Paragraph::new(header), header_area);
    // `render_stateful_widget` with a `ListState` rather than styling the
    // row ourselves: the widget also scrolls the selected row into view,
    // which is what makes a cursor usable in a buffer taller than the
    // frame. The state is rebuilt each draw because nothing here persists
    // between frames yet - ratatui only needs it to compute the offset,
    // and a fresh one recomputes the same offset from the same selection.
    let mut list_state = ListState::default();
    // The unit this view is scoped to, if it is scoped to one: a log row
    // names its own unit only where that differs.
    let scope = match model.header {
        Header::Log { unit, .. } => Some(unit),
        _ => None,
    };
    let (items, first_item) = build_items(model.rows, theme, &columns, scope);
    // The cursor is a node index; a detail block spans several items, so
    // the two only coincide when nothing is open above the cursor.
    list_state.select(model.selected.and_then(|node| first_item.get(node).copied()));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            // Keep the cursor at the middle and move the rows under it,
            // rather than letting it walk to an edge and scroll a line at
            // a time from there. On a buffer that re-sorts every two
            // seconds, a cursor pinned to an edge has no context on the
            // side it is pinned to, and the row being watched keeps
            // arriving from off-screen.
            //
            // `scroll_padding` rather than an offset computed here: it is
            // vim's `scrolloff`, and it measures real item heights, so the
            // Overview - the one row that renders as several lines - is
            // counted as what it draws rather than as one. It also shrinks
            // itself at both ends, which is what stops centring from
            // scrolling a half-screen of blank past the last row.
            .scroll_padding(list_area.height as usize / 2),
        list_area,
        &mut list_state,
    );
    frame.render_widget(Paragraph::new(footer_lines(model, theme, footer_height, width)), footer_area);

    let modal = model.modal.as_ref().map(|modal| draw_modal(frame, modal, area, theme)).unwrap_or((0, 0));
    Metrics { list_height: list_area.height, modal_height: modal.0, modal_content: modal.1 }
}

/// Draws a popup centred over the buffer and reports `(height, content)` -
/// what it got and what it wanted, which is what a later scrolling modal
/// will need.
///
/// Clamped to the frame on both axes: the key help grows with the keymap,
/// and a popup taller than the terminal would otherwise be drawn partly
/// outside it and panic ratatui's own bounds check.
fn draw_modal(frame: &mut Frame, modal: &ModalView, area: Rect, theme: &Theme) -> (u16, u16) {
    let (title, lines) = match modal {
        // Laid out to the frame it has, because the list outgrew it: the
        // Procs buffer has 25 bindings needing ~35 rows, and on a 30-row
        // terminal the popup silently dropped the last nine - including
        // every action key. A reference card that hides half the keys is
        // worse than no reference card.
        ModalView::Keys { groups } => (" keys ".to_string(), key_help_lines(groups, theme, area.height.saturating_sub(2))),
        // A confirmation is one line and deliberately plain: the prompt
        // already names the signal and the process, and decorating it
        // would only make the two outcomes harder to tell apart.
        ModalView::Confirm { prompt } => {
            (" confirm ".to_string(), vec![Line::from(Span::styled(prompt.to_string(), Style::default().fg(theme.status_error)))])
        }
        // A block for a cursor, so it is obvious the popup is taking
        // keystrokes rather than the buffer behind it.
        //
        // Titled `input`, not `filter`: this is a transient's text-entry
        // sub-step - a retention, a generation spec, a search query - and
        // the buffer filter is not a popup at all. The old title dates
        // from before that was settled, and nothing caught it because
        // nothing had ever constructed an `Input`.
        ModalView::Input { prompt, typed, candidates } => (
            " input ".to_string(),
            std::iter::once(Line::from(vec![
                Span::styled(format!("{prompt}: "), Style::default().fg(theme.section_header)),
                Span::raw(format!("{typed}\u{2588}")),
            ]))
            .chain(candidates.iter().flat_map(|list| candidate_lines(list, theme)))
            .collect(),
        ),
        // The title names the subject - `Unit . restic-backup.service`,
        // `Generation 436` - so it is padded here the way the fixed titles
        // above are written padded, and not by every caller.
        ModalView::Transient { title, switches, groups } => (format!(" {title} "), transient_lines(switches, groups, theme)),
    };

    let content = lines.len() as u16;
    // The title is measured with the lines, not after them: a transient
    // names its subject - `Unit . restic-backup.service` - and is
    // routinely wider than any row under it. Sized to the rows alone, the
    // border ate the end of the title, which is the one part of a popup
    // that says what you are about to act on.
    let widest = lines.iter().map(|l| l.width()).chain([UnicodeWidthStr::width(title.as_str())]).max().unwrap_or(0) as u16;
    // +2 on each axis for the border.
    let height = (content + 2).min(area.height);
    let width = (widest + 4).min(area.width);
    let popup =
        Rect { x: area.x + (area.width.saturating_sub(width)) / 2, y: area.y + (area.height.saturating_sub(height)) / 2, width, height };

    // Clear first: a popup drawn over a list would otherwise show the
    // rows through its own blank cells.
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title).border_style(Style::default().fg(theme.section_header))),
        popup,
    );
    (height, content)
}

/// The `?` reference card, grouped Movement / Global / Buffers / this
/// buffer's own, packed into as many columns as it takes to fit
/// `max_height` rows.
///
/// Columns rather than scrolling: this is a reference card, and needing
/// to scroll a list of keys to find the key that scrolls is the kind of
/// joke a tool should not make. Groups are kept whole - a heading in one
/// column with its bindings in the next would be worse than either.
fn key_help_lines(groups: &[masys_view::KeyGroup], theme: &Theme, max_height: u16) -> Vec<Line<'static>> {
    let blocks: Vec<Vec<Line<'static>>> = groups.iter().map(|group| group_block(group, theme)).collect();
    let total: usize = blocks.iter().map(|b| b.len()).sum::<usize>() + blocks.len().saturating_sub(1);
    let height = (max_height as usize).max(1);

    if total <= height {
        return join(vec![blocks.into_iter().flat_map(|b| spaced(b)).collect()]);
    }

    // Greedy packing, groups kept whole. A group taller than the frame
    // still gets its own column and is clipped by the caller - nothing
    // better is available, and it is the only case that can still lose a
    // line.
    let mut columns: Vec<Vec<Line<'static>>> = Vec::new();
    let mut current: Vec<Line<'static>> = Vec::new();
    for block in blocks {
        if !current.is_empty() && current.len() + 1 + block.len() > height {
            columns.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(Line::default());
        }
        current.extend(block);
    }
    if !current.is_empty() {
        columns.push(current);
    }
    join(columns)
}

fn spaced(block: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let mut out = block;
    out.push(Line::default());
    out
}

fn group_block(group: &masys_view::KeyGroup, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines =
        vec![Line::from(Span::styled(group.heading.clone(), Style::default().fg(theme.section_header).add_modifier(Modifier::BOLD)))];
    for binding in &group.bindings {
        let (chord_style, label_style) = row_styles(binding.dimmed, theme);
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<6}", binding.chord), chord_style),
            Span::styled(binding.label.clone(), label_style),
        ]));
    }
    lines
}

/// A picker's candidates, the highlighted one reversed.
///
/// `REVERSED` rather than a `>` marker because it is what already means
/// "the cursor is here" everywhere else in masys - the list's
/// `highlight_style` and the footer's active buffer - and a second idiom
/// for the same idea would have to be learned separately.
///
/// An empty match set draws a line saying so. Drawing nothing would leave
/// the typed text with blank space under it, which reads as a picker
/// still thinking rather than one that has answered.
fn candidate_lines(list: &masys_view::CandidateList, theme: &Theme) -> Vec<Line<'static>> {
    if list.matches.is_empty() {
        return vec![Line::from(Span::styled("  no matches".to_string(), Style::default().fg(theme.info).add_modifier(Modifier::DIM)))];
    }

    list.matches
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let style = match index == list.selected {
                true => Style::default().add_modifier(Modifier::REVERSED),
                false => Style::default(),
            };
            Line::from(Span::styled(format!("  {candidate}"), style))
        })
        .collect()
}

/// A transient's contents: its switch groups, then its action groups,
/// one blank line between each.
///
/// Switches lead because they change what the actions under them will
/// do. Read after choosing an action, they have been read too late.
fn transient_lines(switches: &[SwitchRow], groups: &[ActionGroup], theme: &Theme) -> Vec<Line<'static>> {
    let blocks: Vec<Vec<Line<'static>>> =
        switch_blocks(switches, theme).into_iter().chain(groups.iter().map(|group| action_block(group, theme))).collect();

    blocks.into_iter().enumerate().flat_map(|(index, block)| (index > 0).then(Line::default).into_iter().chain(block)).collect()
}

/// The switch rows, bucketed by their `group` in first-appearance order.
///
/// A `SwitchRow` carries its heading on itself rather than sitting inside
/// a wrapper the way an `ActionRow` sits inside an `ActionGroup`, so
/// putting the buckets back together is the renderer's job. First
/// appearance rather than sorted: the order a transient lists its
/// switches in is the order its author chose, and alphabetising it here
/// would silently overrule them.
fn switch_blocks(switches: &[SwitchRow], theme: &Theme) -> Vec<Vec<Line<'static>>> {
    let headings = switches.iter().map(|switch| switch.group).fold(Vec::new(), |mut seen, group| {
        if !seen.contains(&group) {
            seen.push(group);
        }
        seen
    });
    // One label column across every switch group, not one per group, so
    // the `[x]` boxes line up down the whole popup rather than stepping
    // in and out at each heading.
    let width = switches.iter().map(|switch| UnicodeWidthStr::width(switch.label)).max().unwrap_or(0);

    headings
        .into_iter()
        .map(|heading| {
            std::iter::once(heading_line(heading.to_string(), None, theme))
                .chain(switches.iter().filter(|switch| switch.group == heading).map(|switch| switch_line(switch, theme, width)))
                .collect()
        })
        .collect()
}

/// `-v  verbose            [x]`.
///
/// An unsupported switch is dimmed rather than dropped, for the reason a
/// dimmed action row is: a switch that vanishes on some hosts makes the
/// popup unreadable by position.
fn switch_line(switch: &SwitchRow, theme: &Theme, width: usize) -> Line<'static> {
    let (chord_style, label_style) = row_styles(!switch.supported, theme);
    Line::from(vec![
        Span::styled(format!("  {:<4}", switch.chord), chord_style),
        Span::styled(format!("{:<width$}", switch.label, width = width), label_style),
        Span::styled(if switch.on { "  [x]" } else { "  [ ]" }.to_string(), label_style),
    ])
}

/// One action group: its heading, then its rows.
///
/// Rows pair two to a line when no row in the group carries a note, and
/// go one to a line when any does - which is exactly how the design draws
/// the unit popup, `s start / S stop` side by side above `e enable` alone
/// with `reverts on nixos-rebuild` beside it. The rule is per group
/// rather than per row so that a group reads as one shape; pairing the
/// note-less rows of a mixed group would put the same group's actions in
/// two different places to look.
fn action_block(group: &ActionGroup, theme: &Theme) -> Vec<Line<'static>> {
    let heading = heading_line(group.heading.clone(), group.note.clone(), theme);
    let width = group.rows.iter().map(|row| UnicodeWidthStr::width(row.label)).max().unwrap_or(0);
    let annotated = group.rows.iter().any(|row| row.note.is_some());
    let per_line = if annotated { 1 } else { 2 };

    std::iter::once(heading)
        .chain(group.rows.chunks(per_line).map(|pair| {
            Line::from(
                pair.iter().enumerate().flat_map(|(index, row)| action_cell(row, theme, width, index + 1 < pair.len())).collect::<Vec<_>>(),
            )
        }))
        .collect()
}

/// One action row's spans - `  s  start` plus whatever follows it, which
/// is its note, or padding if another cell shares the line, or nothing.
///
/// Spans rather than a `Line` so that two cells can be laid on one.
fn action_cell(row: &ActionRow, theme: &Theme, width: usize, paired: bool) -> Vec<Span<'static>> {
    let (chord_style, label_style) = row_styles(row.dimmed, theme);
    let label = if paired || row.note.is_some() { format!("{:<width$}", row.label, width = width) } else { row.label.to_string() };

    std::iter::once(Span::styled(format!("  {}  ", row.chord), chord_style))
        .chain(std::iter::once(Span::styled(label, label_style)))
        .chain(row.note.iter().map(|note| Span::styled(format!("  {note}"), Style::default().add_modifier(Modifier::DIM))))
        .chain(paired.then(|| Span::raw("   ".to_string())))
        .collect()
}

/// A group heading, with its group-level note trailing it.
///
/// The note is drawn exactly as given. The designs write two of them -
/// the unit popup's `! declared in nix` and the Nix transient's
/// `policy: weekly, keep 14d` - and only one is a warning. A renderer
/// that prefixed every note with `!` would announce a retention policy as
/// an alarm, so the marker belongs to whoever wrote the note and knows
/// which kind it is.
fn heading_line(heading: String, note: Option<String>, theme: &Theme) -> Line<'static> {
    Line::from(
        std::iter::once(Span::styled(heading, Style::default().fg(theme.section_header).add_modifier(Modifier::BOLD)))
            .chain(note.map(|note| Span::styled(format!("   {note}"), Style::default().fg(theme.info))))
            .collect::<Vec<_>>(),
    )
}

/// Dimming, in one place: a dimmed row keeps its position and its chord
/// still works, so only the styling says anything is different about it.
fn row_styles(dimmed: bool, theme: &Theme) -> (Style, Style) {
    match dimmed {
        true => (Style::default().fg(theme.info).add_modifier(Modifier::DIM), Style::default().add_modifier(Modifier::DIM)),
        false => (Style::default().fg(theme.info), Style::default()),
    }
}

/// Lays columns side by side, padding each to its own widest line so the
/// next one starts in a straight edge.
fn join(columns: Vec<Vec<Line<'static>>>) -> Vec<Line<'static>> {
    let height = columns.iter().map(|c| c.len()).max().unwrap_or(0);
    let widths: Vec<usize> = columns.iter().map(|c| c.iter().map(|l| l.width()).max().unwrap_or(0)).collect();

    (0..height)
        .map(|row| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (index, column) in columns.iter().enumerate() {
                let line = column.get(row);
                let used = line.map(|l| l.width()).unwrap_or(0);
                if let Some(line) = line {
                    spans.extend(line.spans.clone());
                }
                // Three columns of gutter, except after the last.
                if index + 1 < columns.len() {
                    spans.push(Span::raw(" ".repeat(widths[index].saturating_sub(used) + 3)));
                }
            }
            Line::from(spans)
        })
        .collect()
}
