//! `render` is the only function that touches a `Frame`, so it is the only
//! one that can be tested against real terminal cells. Everything asserted
//! here is a *placement* fact - which buffer line something lands on, and
//! which column - since what the lines say is already pinned by
//! `tests/view.rs`.

mod fake;

use fake::{FakePlatformService, FakeSystemService};
use masys_app::App;
use masys_domain::declarative::{Generation, Input, ProfileKind};
use masys_domain::finding::{Finding, PressureResource};
use masys_domain::sample::{Pressure, Snapshot, SystemState};
use masys_domain::unit::{ActiveState, Unit, UnitKind};
use masys_render::render;
use masys_render::theme::Theme;
use masys_view::{
    ActionGroup, ActionRow, CandidateList, Header, KeyBinding, KeyGroup, ModalView, Node, Overview, SectionKind, StatusLine, SwitchRow,
    View,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;
use unicode_width::UnicodeWidthStr;

const WIDTH: u16 = 80;
const HEIGHT: u16 = 24;
const TIMESTAMP: &str = "2026-05-20 09:14";

fn buffer_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_string())
        .collect()
}

/// Draws one frame at the fixed test size and hands back its cells as
/// text. `hostname` is a parameter only because the header padding test
/// needs a non-ASCII one.
fn draw(hostname: &str, rows: &[Node], status: StatusLine<'_>) -> Vec<String> {
    let theme = Theme::default();
    let hints = [
        KeyBinding { chord: "t".to_string(), label: "procs".to_string(), dimmed: false, active: false },
        KeyBinding { chord: "g".to_string(), label: "refresh".to_string(), dimmed: false, active: false },
        KeyBinding { chord: "?".to_string(), label: "keys".to_string(), dimmed: false, active: false },
        KeyBinding { chord: "q".to_string(), label: "back / quit".to_string(), dimmed: false, active: false },
    ];
    let model = View {
        header: Header::Status { hostname, timestamp: TIMESTAMP },
        rows,
        selected: None,
        status,
        modal: None,
        hints: &hints,
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &model, &theme);
        })
        .expect("a drawn frame");
    buffer_lines(&terminal)
}

/// The terminal column `needle` starts at, measured in display columns
/// rather than bytes - the same measure the padding under test uses, so
/// the assertion means what it says even for a non-ASCII label.
fn column_of(line: &str, needle: &str) -> usize {
    let at = line.find(needle).unwrap_or_else(|| panic!("{needle:?} is not in {line:?}"));
    UnicodeWidthStr::width(&line[..at])
}

fn overview_row() -> Node {
    Node::Overview(Overview {
        machine: None,
        system_state: SystemState::Running,
        unit_count: 47,
        load_1: None,
        load_5: None,
        load_15: None,
        uptime_secs: None,
        mem_used_bytes: None,
        mem_total_bytes: None,
        zram_percent: None,
        swap_free_bytes: None,
        clock_synced: true,
        smart_ok: None,
        pending_reboot: None,
    })
}

fn header(title: &str, kind: SectionKind, count: Option<u32>) -> Node {
    Node::SectionHeader { title: title.to_string(), kind, count }
}

/// The tail of every buffer built so far: the always-present System
/// section, so the tests above it exercise a realistic row list rather
/// than a finding section floating on its own.
fn system_section() -> Vec<Node> {
    vec![header("System", SectionKind::System, None), overview_row()]
}

#[test]
fn the_header_shows_hostname_left_and_timestamp_right() {
    let lines = draw("devbox", &system_section(), StatusLine::Hints);
    assert!(lines[0].starts_with(" masys - devbox"), "{:?}", lines[0]);
    assert!(lines[0].ends_with(TIMESTAMP), "{:?}", lines[0]);
    // One short of the terminal width: the frame is inset a column on
    // each side, so "flush right" means flush against the inset edge.
    assert_eq!(UnicodeWidthStr::width(lines[0].as_str()), WIDTH as usize - 1, "the timestamp is flush right");
    assert_eq!(lines[1], "", "a blank line separates the header from the first row");
}

/// systemd's pretty hostname is arbitrary UTF-8 (`hostnamectl --pretty`
/// accepts "Büro Läptop"), so padding the header by byte length would
/// push the timestamp off the right edge on any host that uses one.
#[test]
fn the_header_pads_by_display_width_not_byte_length() {
    let lines = draw("läptop", &system_section(), StatusLine::Hints);
    assert!(lines[0].ends_with(TIMESTAMP), "{:?}", lines[0]);
    assert_eq!(
        UnicodeWidthStr::width(lines[0].as_str()),
        WIDTH as usize - 1,
        "a 2-byte 1-column char must not steal a column from the padding"
    );
}

#[test]
fn a_failed_unit_row_renders_its_glyph_and_text() {
    let mut rows = vec![
        header("Failed units", SectionKind::FailedUnits, Some(1)),
        Node::Finding(Finding::FailedUnit {
            unit: "restic-backup.service".to_string(),
            exit_code: Some(1),
            since_ms: 10_800_000,
            reason: None,
        }),
        Node::Spacer,
    ];
    rows.extend(system_section());

    let lines = draw("devbox", &rows, StatusLine::Hints);
    assert!(lines.iter().any(|l| l.contains("Failed units (1)")), "{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("x restic-backup.service")), "{lines:#?}");
}

#[test]
fn the_footer_shows_a_separator_and_hints() {
    let lines = draw("devbox", &system_section(), StatusLine::Hints);
    let separator = &lines[lines.len() - 2];
    // The inset costs the separator its two edge columns, one of which
    // `buffer_lines` then trims off the right.
    assert_eq!(separator.len(), WIDTH as usize - 1, "{separator:?}");
    assert!(separator.trim_start().chars().all(|c| c == '-'), "{separator:?}");
    assert!(separator.starts_with(" -"), "the separator is inset too: {separator:?}");
    assert!(lines[lines.len() - 1].contains("[t] procs"), "{:?}", lines[lines.len() - 1]);
}

#[test]
fn an_error_status_line_replaces_the_hints() {
    let lines = draw("devbox", &system_section(), StatusLine::Error("polkit: access denied"));
    assert!(lines[lines.len() - 1].contains("polkit: access denied"), "{:?}", lines[lines.len() - 1]);
    assert!(!lines.iter().any(|l| l.contains("[r]estart")), "an error takes the footer over entirely: {lines:#?}");
}

/// An action's stderr is arbitrarily long, and the footer takes its
/// height from it - unclamped, a `Constraint::Length` that large starves
/// the header and list areas to nothing, so a stack trace would erase the
/// very buffer it is reporting on.
#[test]
fn a_long_error_cannot_starve_the_header_and_list() {
    let error = (0..40).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
    let lines = draw("devbox", &system_section(), StatusLine::Error(&error));

    assert!(lines[0].starts_with(" masys - devbox"), "the header survives a 40-line error: {lines:#?}");
    assert_eq!(lines[2], " System", "at least one list row survives too: {lines:#?}");
    assert!(lines.iter().any(|l| l.contains("line 0")), "the error is still shown, truncated: {lines:#?}");
}

/// `App`'s state before the first `tick`, so this is what the very first
/// frame of a session draws.
#[test]
fn an_empty_row_list_still_draws_the_frame() {
    let lines = draw("devbox", &[], StatusLine::Hints);

    assert!(lines[0].starts_with(" masys - devbox"), "{:?}", lines[0]);
    assert!(lines[lines.len() - 1].contains("[t] procs"), "{:?}", lines[lines.len() - 1]);
    assert!(lines[2..lines.len() - 2].iter().all(|l| l.is_empty()), "no rows means a blank list: {lines:#?}");
}

/// The payoff of `view::finding_lines`: `render` must hand it a whole
/// section at once, so both labels pad out to the same width and the
/// columns after them line up. Asserting the column - not just that both
/// strings appear - is what makes this test about alignment.
#[test]
fn findings_in_one_section_share_a_padded_label_column() {
    let mut rows = vec![
        header("Pressure", SectionKind::Pressure, Some(2)),
        Node::Finding(Finding::Pressure { resource: PressureResource::Memory, some_avg60: 41.2, full_avg60: None }),
        Node::Finding(Finding::Pressure { resource: PressureResource::Io, some_avg60: 22.7, full_avg60: None }),
        Node::Spacer,
    ];
    rows.extend(system_section());

    let lines = draw("devbox", &rows, StatusLine::Hints);
    let columns: Vec<usize> = lines.iter().filter(|l| l.contains("some avg60")).map(|l| column_of(l, "some avg60")).collect();

    // 1 frame inset, 2 row indent, 2 for the glyph, 6 for "memory" (the
    // widest label), 2 for the tail separator. `io` is padded out to the
    // same column, not left at 9.
    assert_eq!(columns, vec![13, 13], "both tails start in the same column: {lines:#?}");
}

/// The other half of the batching contract: a section is a *maximal run*
/// of consecutive `Node::Finding`, so the wide `memory` label must not
/// pad out the narrow `/nix` one in the next section. Each section here
/// carries two differently-sized labels, so this test fails both ways -
/// if runs are not batched at all (the columns within a section diverge)
/// and if they are batched into one (the columns across sections
/// converge).
#[test]
fn padding_does_not_leak_across_a_section_boundary() {
    let mut rows = vec![
        header("Pressure", SectionKind::Pressure, Some(2)),
        Node::Finding(Finding::Pressure { resource: PressureResource::Memory, some_avg60: 41.2, full_avg60: None }),
        Node::Finding(Finding::Pressure { resource: PressureResource::Io, some_avg60: 22.7, full_avg60: None }),
        Node::Spacer,
        header("Disk", SectionKind::Disk, Some(2)),
        Node::Finding(Finding::ReadOnlyFilesystem { mount_point: "/nix".to_string() }),
        Node::Finding(Finding::ReadOnlyFilesystem { mount_point: "/".to_string() }),
        Node::Spacer,
    ];
    rows.extend(system_section());

    let lines = draw("devbox", &rows, StatusLine::Hints);
    let pressure: Vec<usize> = lines.iter().filter(|l| l.contains("some avg60")).map(|l| column_of(l, "some avg60")).collect();
    let disk: Vec<usize> = lines.iter().filter(|l| l.contains("remounted")).map(|l| column_of(l, "remounted")).collect();

    // 3 (inset + indent) + 2 (glyph) + 6 ("memory") + 2 (separator).
    assert_eq!(pressure, vec![13, 13], "{lines:#?}");
    // 3 + 2 (glyph) + 4 ("/nix") + 2 (separator) - the Disk section's own
    // widest label, untouched by the Pressure section above it.
    assert_eq!(disk, vec![11, 11], "the Disk section is padded to /nix, not to memory: {lines:#?}");
}

fn degraded_host() -> (FakeSystemService, FakePlatformService) {
    let snapshot = Snapshot {
        taken_at_ms: 0,
        procs: Vec::new(),
        pressure: Some(Pressure::default()),
        filesystems: Vec::new(),
        disks: Vec::new(),
        clock_synced: true,
        utc_offset_secs: -21_600,
        interfaces: Vec::new(),
        oom_kills: Vec::new(),
        system_state: SystemState::Degraded,
        clock_ticks_per_sec: 100,
        machine: None,
        load: None,
        uptime_secs: None,
        memory: None,
    };
    let units = vec![Unit {
        name: "restic-backup.service".to_string(),
        kind: UnitKind::Service,
        active_state: ActiveState::Failed,
        sub_state: "failed".to_string(),
        exit_code: Some(1),
        enabled: true,
        restart_timestamps_ms: Vec::new(),
        since_ms: 0,
        cgroup: None,
        slice: None,
        triggers: Vec::new(),
        timer: None,
    }];
    (FakeSystemService { snapshot, units, queued: Default::default() }, FakePlatformService { boot_pressure: None, pending_reboot: None })
}

/// The whole stack in one test: real ports (stubbed), real triage, real
/// row building, real frame. Every other test here hand-builds its rows,
/// which cannot catch a mismatch between what `build_status_rows` emits
/// and what `build_items` expects.
#[test]
fn app_tick_to_render_draws_a_failed_unit_on_a_real_frame() {
    let (system, platform) = degraded_host();
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(10_800_000, TIMESTAMP.to_string()).expect("tick");

    let theme = Theme::default();
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &app.view(), &theme);
        })
        .expect("a drawn frame");
    let lines = buffer_lines(&terminal);

    assert!(lines[0].starts_with(" masys - devbox"), "{:?}", lines[0]);
    for expected in ["Failed units (1)", "x restic-backup.service", "Degraded", "System"] {
        assert!(lines.iter().any(|l| l.contains(expected)), "{expected:?} is missing from {lines:#?}");
    }
}

#[test]
fn the_frame_indents_rows_under_their_section_header() {
    let (system, platform) = degraded_host();
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(10_800_000, TIMESTAMP.to_string()).expect("tick");

    let theme = Theme::default();
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &app.view(), &theme);
        })
        .expect("a drawn frame");
    let lines = buffer_lines(&terminal);

    // The mockup insets the whole frame by one column, then indents a
    // section's rows two further columns under its header.
    let header = lines.first().expect("a header line");
    assert!(header.starts_with(" masys - devbox"), "header is not inset: {header:?}");

    let section = lines.iter().find(|l| l.contains("Failed units")).expect("the Failed units header");
    assert_eq!(section, " Failed units (1)", "section header sits at column 1");

    let finding = lines.iter().find(|l| l.contains("restic-backup.service")).expect("the failed unit row");
    assert!(finding.starts_with("   x restic-backup.service"), "finding row is not indented: {finding:?}");

    let overview = lines.iter().find(|l| l.contains("degraded  .")).expect("the overview row");
    assert!(overview.starts_with("   . degraded"), "overview row is not indented: {overview:?}");

    // Every view is listed, current one included, so the row always
    // begins at the first.
    let hints = lines.iter().find(|l| l.contains("[2] procs")).expect("the footer hints");
    assert!(hints.starts_with(" [1] status"), "footer is not inset: {hints:?}");
}

/// The cursor stays at the vertical middle of the list and the rows move
/// under it, rather than the cursor walking to the bottom edge and the
/// view then scrolling a line at a time.
///
/// On a live buffer this is the difference between reading and chasing:
/// rows are re-sorted every two seconds, so a cursor pinned to an edge has
/// no context on the side it is pinned to, and the row you are watching
/// keeps arriving from off-screen.
#[test]
fn the_cursor_sits_at_the_middle_of_the_list_while_scrolling() {
    let rows: Vec<Node> =
        (0..60).map(|n| Node::SectionHeader { title: format!("row-{n:02}"), kind: SectionKind::Units, count: None }).collect();

    // Deep enough into the list that centring is possible in both
    // directions - neither end can clamp it.
    let (lines, list_top, list_height) = draw_selected(&rows, 30);
    let y = lines.iter().position(|l| l.contains("row-30")).expect("the selected row is on screen");

    let middle = list_top + list_height / 2;
    assert!(y.abs_diff(middle) <= 1, "selected row is at line {y}, middle of the list is {middle}\n{}", lines.join("\n"));
}

/// Centring must not scroll past either end - a half-screen of blank
/// above the first row would be centring applied without judgement.
#[test]
fn the_ends_of_the_list_clamp_rather_than_showing_blank() {
    let rows: Vec<Node> =
        (0..60).map(|n| Node::SectionHeader { title: format!("row-{n:02}"), kind: SectionKind::Units, count: None }).collect();

    let (top, list_top, _) = draw_selected(&rows, 0);
    assert_eq!(top.iter().position(|l| l.contains("row-00")), Some(list_top), "the first row stays at the top of the list");

    let (bottom, _, _) = draw_selected(&rows, 59);
    assert!(bottom.iter().any(|l| l.contains("row-59")), "the last row is on screen");
    assert!(!bottom.iter().any(|l| l.contains("row-00")), "and the list has actually scrolled");
}

/// Draws `rows` with `selected` highlighted, and reports the frame plus
/// where the list area starts and how tall it is - both derived the same
/// way `render` derives them, so the assertion cannot drift from layout.
fn draw_selected(rows: &[Node], selected: usize) -> (Vec<String>, usize, usize) {
    let theme = Theme::default();
    let hints: [KeyBinding; 0] = [];
    let model = View {
        header: Header::Titled("systemd"),
        rows,
        selected: Some(selected),
        status: StatusLine::Hints,
        modal: None,
        hints: &hints,
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &model, &theme);
        })
        .expect("a drawn frame");
    let lines = buffer_lines(&terminal);
    // One header line plus the blank the renderer adds under it.
    let list_top = 2;
    // Separator plus the hint row; no actions in this model.
    let list_height = HEIGHT as usize - list_top - 2;
    (lines, list_top, list_height)
}

/// `?` draws the key help over the buffer. Asserted on real cells because
/// a popup that renders behind the list, or off the frame, still passes
/// every test of what its lines say.
#[test]
fn the_key_help_draws_over_the_buffer_and_stays_inside_the_frame() {
    let theme = Theme::default();
    let hints: [KeyBinding; 0] = [];
    let groups = vec![
        KeyGroup {
            heading: "Movement".to_string(),
            bindings: vec![KeyBinding { chord: "down".to_string(), label: "down".to_string(), dimmed: false, active: false }],
        },
        KeyGroup {
            heading: "Procs only".to_string(),
            bindings: vec![KeyBinding { chord: "o".to_string(), label: "cycle sort".to_string(), dimmed: false, active: false }],
        },
    ];
    let rows = system_section();
    let model = View {
        header: Header::Status { hostname: "devbox", timestamp: TIMESTAMP },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: Some(ModalView::Keys { groups }),
        hints: &hints,
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &model, &theme);
        })
        .expect("a drawn frame");
    let lines = buffer_lines(&terminal);

    assert!(lines.iter().any(|l| l.contains("Movement")), "{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("cycle sort")), "the buffer's own keys are listed: {lines:#?}");
    assert!(lines.iter().any(|l| l.contains("keys")), "the popup is titled: {lines:#?}");
    for line in &lines {
        assert!(UnicodeWidthStr::width(line.as_str()) <= WIDTH as usize, "{line:?} runs past the frame");
    }
}

/// A help list longer than the terminal must be clamped rather than drawn
/// outside it - the keymap grows, terminals do not.
#[test]
fn a_key_help_taller_than_the_frame_is_clamped() {
    let theme = Theme::default();
    let hints: [KeyBinding; 0] = [];
    let groups = vec![KeyGroup {
        heading: "Everything".to_string(),
        bindings: (0..80)
            .map(|n| KeyBinding { chord: format!("{n}"), label: format!("action {n}"), dimmed: false, active: false })
            .collect(),
    }];
    let rows = system_section();
    let model = View {
        header: Header::Status { hostname: "devbox", timestamp: TIMESTAMP },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: Some(ModalView::Keys { groups }),
        hints: &hints,
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    // The assertion is that this does not panic on out-of-bounds drawing.
    terminal
        .draw(|frame| {
            render(frame, &model, &theme);
        })
        .expect("a drawn frame");
    assert_eq!(buffer_lines(&terminal).len(), HEIGHT as usize);
}

/// The list outgrew the frame: the Procs buffer has 25 bindings needing
/// ~35 rows, and on a 30-row terminal the popup silently dropped the last
/// nine - every action key among them. A reference card that hides half
/// the keys is worse than no reference card.
#[test]
fn a_key_help_too_tall_for_the_frame_uses_columns_rather_than_truncating() {
    let theme = Theme::default();
    let hints: [KeyBinding; 0] = [];
    let groups: Vec<KeyGroup> = (0..4)
        .map(|g| KeyGroup {
            heading: format!("Group {g}"),
            bindings: (0..6)
                .map(|n| KeyBinding { chord: format!("{g}{n}"), label: format!("action {g}{n}"), dimmed: false, active: false })
                .collect(),
        })
        .collect();
    let rows = system_section();
    let model = View {
        header: Header::Status { hostname: "devbox", timestamp: TIMESTAMP },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: Some(ModalView::Keys { groups }),
        hints: &hints,
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &model, &theme);
        })
        .expect("a drawn frame");
    let lines = buffer_lines(&terminal);

    // Every binding is reachable, including the last group's - which is
    // exactly what truncation used to lose.
    for group in 0..4 {
        for n in 0..6 {
            let label = format!("action {group}{n}");
            assert!(lines.iter().any(|l| l.contains(&label)), "{label} is missing from {lines:#?}");
        }
    }
    // And nothing runs off the frame.
    for line in &lines {
        assert!(UnicodeWidthStr::width(line.as_str()) <= WIDTH as usize, "{line:?}");
    }
}

/// A help that already fits stays in one column - columns are a response
/// to running out of height, not the normal shape.
#[test]
fn a_short_key_help_stays_in_one_column() {
    let theme = Theme::default();
    let hints: [KeyBinding; 0] = [];
    let groups = vec![KeyGroup {
        heading: "Movement".to_string(),
        bindings: vec![KeyBinding { chord: "down".to_string(), label: "down".to_string(), dimmed: false, active: false }],
    }];
    let rows = system_section();
    let model = View {
        header: Header::Status { hostname: "devbox", timestamp: TIMESTAMP },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: Some(ModalView::Keys { groups }),
        hints: &hints,
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &model, &theme);
        })
        .expect("a drawn frame");
    let lines = buffer_lines(&terminal);
    let help = lines.iter().find(|l| l.contains("Movement")).expect("the help");
    // One column means the heading is followed by border, not by a
    // second column's heading.
    assert_eq!(help.matches("Movement").count(), 1, "{help:?}");
}

/// A reversal that is not announced looks like a key that did nothing -
/// journal entries are often seconds apart.
#[test]
fn the_log_header_names_the_unit_and_which_way_it_reads() {
    let rows: Vec<Node> = Vec::new();
    let newest = draw_model(&View {
        header: Header::Log { unit: "sshd.service", newest_first: true },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: None,
        hints: &[],
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    });
    assert!(newest[0].contains("sshd.service"), "{:?}", newest[0]);
    assert!(newest[0].contains("newest first"), "{:?}", newest[0]);

    let oldest = draw_model(&View {
        header: Header::Log { unit: "sshd.service", newest_first: false },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: None,
        hints: &[],
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    });
    assert!(oldest[0].contains("oldest first"), "{:?}", oldest[0]);
}

/// The systemd view offers thirteen keys, 162 columns of them, and one
/// row of an 80-column terminal cannot hold that. Wrapping rather than
/// clipping is the design's own rule applied to width: masys never hides
/// an action, and `[l] logs` sitting past an ellipsis is hidden.
#[test]
fn the_footer_wraps_rather_than_clipping_the_real_keymap() {
    let theme = Theme::default();
    let actions = masys_app::keymap::Keymap::default().actions(masys_app::Buffer::Systemd);
    let rows = system_section();
    let model = View {
        header: Header::Status { hostname: "devbox", timestamp: TIMESTAMP },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: None,
        hints: &[],
        actions: &actions,
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &model, &theme);
        })
        .expect("a drawn frame");
    let lines = buffer_lines(&terminal);
    let footer = lines.join("\n");

    for chord in ["r", "alt+r", "R", "S", "X", "e", "D", "F", "E", "M", "U", "l", "/"] {
        assert!(footer.contains(&format!("[{chord}]")), "[{chord}] is not on screen: {footer}");
    }
    assert!(!footer.contains("..."), "nothing was clipped: {footer}");
    for line in &lines {
        assert!(UnicodeWidthStr::width(line.as_str()) <= WIDTH as usize, "{line:?}");
    }
    // The rows are what the operator came for: the footer takes the space
    // it needs and no more.
    assert!(lines.iter().any(|l| l.contains("System")), "the list survived: {footer}");
}

/// Wrapping is not unbounded: a footer that ate the list would be worse
/// than a clipped one. Past the cap the last row is marked, and `?` has
/// the rest - the rule the clipped row always had.
#[test]
fn a_footer_too_narrow_for_every_action_says_so() {
    let theme = Theme::default();
    let actions: Vec<KeyBinding> =
        (0..20).map(|n| KeyBinding { chord: format!("{n}"), label: format!("action number {n}"), dimmed: false, active: false }).collect();
    let rows = system_section();
    let model = View {
        header: Header::Status { hostname: "devbox", timestamp: TIMESTAMP },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: None,
        hints: &[],
        actions: &actions,
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &model, &theme);
        })
        .expect("a drawn frame");
    let lines = buffer_lines(&terminal);

    let first = lines.iter().position(|l| l.contains("action number 0")).expect("the actions row");
    // Three rows of keys at most, and the third says it is not the end.
    let marked = lines[first..].iter().take(3).any(|l| l.ends_with("..."));
    assert!(marked, "a clipped footer says so: {:?}", &lines[first..first + 3]);
    assert!(!lines[first + 3].contains("action number"), "the cap held: {:?}", &lines[first..]);
    for line in &lines {
        assert!(UnicodeWidthStr::width(line.as_str()) <= WIDTH as usize, "{line:?}");
    }
}

/// The filter belongs above the rows it narrows, where it stays visible
/// while they move - a centred popup covered them.
#[test]
fn the_filter_draws_under_the_header_not_over_the_rows() {
    let theme = Theme::default();
    let rows = system_section();
    let model = View {
        header: Header::Status { hostname: "devbox", timestamp: TIMESTAMP },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: None,
        hints: &[],
        actions: &[],
        filter: Some("chrome"),
        typing: true,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &model, &theme);
        })
        .expect("a drawn frame");
    let lines = buffer_lines(&terminal);

    assert!(lines[1].contains("/chrome"), "directly under the header: {:?}", lines[1]);
    assert!(lines[1].contains('\u{2588}'), "a cursor while typing: {:?}", lines[1]);
    assert!(lines.iter().any(|l| l.contains("System")), "the rows are still visible: {lines:#?}");
}

/// One frame from a hand-built model, for the tests that are about the
/// chrome around the rows rather than the rows.
fn draw_model(model: &View<'_>) -> Vec<String> {
    let theme = Theme::default();
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, model, &theme);
        })
        .expect("a drawn frame");
    buffer_lines(&terminal)
}

fn filtered(filter: &str, matches: Option<usize>, paused: bool, rows: &[Node]) -> Vec<String> {
    draw_model(&View {
        header: Header::Status { hostname: "devbox", timestamp: TIMESTAMP },
        rows,
        selected: None,
        status: StatusLine::Hints,
        modal: None,
        hints: &[],
        actions: &[],
        filter: Some(filter),
        typing: true,
        filter_matches: matches,
        auto_refresh_paused: paused,
    })
}

/// A live filter over 353 rows narrows to nothing one keystroke before
/// you notice, and rows going blank looks the same whether the query
/// matched nothing or the buffer was empty to begin with.
#[test]
fn the_filter_line_counts_its_matches_as_they_are_typed() {
    let rows = system_section();
    let lines = filtered("chrome", Some(12), false, &rows);
    let query = lines.iter().find(|l| l.contains("/chrome")).expect("the filter line");
    assert!(query.contains("12 matches"), "{query:?}");
}

/// Singular and plural, because `1 matches` is the kind of thing that
/// makes a careful reader distrust the number beside it.
#[test]
fn a_single_match_is_counted_in_the_singular() {
    let rows = system_section();
    let lines = filtered("chrome", Some(1), false, &rows);
    assert!(lines.iter().any(|l| l.contains("1 match") && !l.contains("1 matches")), "{lines:#?}");
}

/// Zero is the one answer worth interrupting for: it is the difference
/// between a filter that is narrowing and one that has hidden everything.
#[test]
fn no_matches_says_so_in_words() {
    let rows = system_section();
    let lines = filtered("zzzz", Some(0), false, &rows);
    assert!(lines.iter().any(|l| l.contains("no matches")), "{lines:#?}");
    assert!(!lines.iter().any(|l| l.contains("0 matches")), "a bare zero reads as a count, not an outcome: {lines:#?}");
}

/// No query, no count. A blank filter line reporting the whole buffer
/// would be answering a question nobody asked.
#[test]
fn an_empty_filter_reports_no_count_at_all() {
    let rows = system_section();
    let lines = filtered("", None, false, &rows);
    assert!(!lines.iter().any(|l| l.contains("match")), "{lines:#?}");
}

/// A view that has quietly stopped updating is indistinguishable from a
/// machine that has quietly stopped answering, and the second is a far
/// more alarming thing to conclude from the same still screen.
#[test]
fn a_paused_view_says_so_and_says_how_to_resume() {
    let rows = system_section();
    let lines = filtered("", None, true, &rows);
    let notice = lines.iter().find(|l| l.contains("paused")).expect("a pause notice");
    assert!(notice.contains("fold"), "it names the way out: {notice:?}");
    assert!(notice.contains('g'), "and the way to refresh anyway: {notice:?}");
}

#[test]
fn a_running_view_carries_no_pause_notice() {
    let rows = system_section();
    let lines = filtered("", None, false, &rows);
    assert!(!lines.iter().any(|l| l.contains("paused")), "{lines:#?}");
}

fn a_detail_proc(pid: u32, comm: &str) -> masys_domain::sample::Proc {
    masys_domain::sample::Proc {
        pid,
        comm: comm.into(),
        cgroup: Some("/user.slice/session.scope".into()),
        cpu_ticks: 100,
        rss_bytes: 40 << 20,
        io_read_bytes: 0,
        io_write_bytes: 0,
        state: masys_domain::sample::ProcState::Running,
        nice: 0,
        oom_score: 12,
        threads: 4,
        started_at_ms: 1_000,
    }
}

/// An open row and its block, with `vars` environment variables - the
/// unbounded part, and the one every shell-launched process has plenty of.
fn opened_process(vars: usize) -> Vec<Node> {
    let detail = masys_domain::proc_detail::ProcDetail {
        cmdline: Some("fish".into()),
        env: (0..vars).map(|n| (format!("VAR_{n}"), format!("value-{n}"))).collect(),
        ..Default::default()
    };
    vec![
        Node::Proc { proc: a_detail_proc(4242, "fish"), rate: None, cpu_seconds: 3, age_ms: Some(9_000), depth: 1, expanded: true },
        Node::ProcDetail { proc: a_detail_proc(4242, "fish"), detail: Some(Box::new(detail)) },
    ]
}

fn draw_rows(rows: &[Node], selected: Option<usize>) -> Vec<String> {
    draw_model(&View {
        header: Header::Procs { sort: masys_view::ProcSort::Cpu, descending: true },
        rows,
        selected,
        status: StatusLine::Hints,
        modal: None,
        hints: &[],
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: true,
    })
}

/// The bug this guards: ratatui does not *clip* a `ListItem` taller than
/// the list, it declines to draw it. A block one row too tall vanished
/// completely, and the row it belonged to looked inert when opened.
///
/// Every process launched from a shell carries twenty-odd environment
/// variables, which is exactly where that cliff sat - so this was every
/// process, not an edge case.
#[test]
fn an_open_process_always_renders_its_block_however_long_it_is() {
    for vars in [0usize, 5, 20, 21, 25, 60, 400] {
        let rows = opened_process(vars);
        let lines = draw_rows(&rows, Some(0));
        assert!(lines.iter().any(|l| l.contains("cmdline")), "the block vanished at {vars} vars: {lines:#?}");
        // And the row it belongs to is still on screen to own it.
        assert!(lines.iter().any(|l| l.contains("fish")), "at {vars} vars: {lines:#?}");
    }
}

/// The cursor cannot rest on a detail row - `selectable` excludes it -
/// but the renderer is handed a `selected` index by a session that could
/// be wrong, and a blanked buffer is a bad way to find that out.
#[test]
fn a_cursor_landing_on_a_detail_row_still_draws_the_buffer() {
    let rows = opened_process(60);
    let lines = draw_rows(&rows, Some(1));
    assert!(lines.iter().any(|l| l.contains("cmdline")), "{lines:#?}");
}

/// `row_item` returns `Line::default()` for every variant `build_items`
/// batches - `Generation`, `Input` and `NixPolicy` among them - because
/// the real text is built by `view::generation_lines` and friends, called
/// only from `build_items`' own batching arm. Delete that arm (or blank
/// `row_item`'s `RebootPending` dispatch, which is not batched at all)
/// and every one of these rows renders as an empty line while
/// `tests/view.rs` - which calls `view::generation_lines` et al.
/// directly - stays entirely green. Only a real frame proves the row
/// actually gets there.
///
/// The same hole exists for `Unit`, `Timer`, `Disk` and `Filesystem`, and
/// is out of scope here: this closes it for the Nix view alone.
#[test]
fn every_nix_row_type_reaches_the_frame() {
    let rows = vec![
        // Un-indented and headerless, so it also proves the one row that
        // is not batched at all still reaches `row_item`'s real arm.
        Node::RebootPending { booted: Some(41), current: Some(42), kernel_changed: true, initrd_changed: false },
        Node::Spacer,
        header("Store", SectionKind::NixStore, None),
        Node::NixStore {
            used_percent: Some(62.0),
            free_bytes: Some(120_000_000_000),
            system_generations: Some(5),
            home_generations: Some(2),
            gc_roots: Some(3),
        },
        Node::NixPolicy {
            job: "gc".to_string(),
            unit: "nix-gc.timer".to_string(),
            retention: Some(Some("14d".to_string())),
            next_in_ms: Some(3_600_000),
            last_ago_ms: Some(7_200_000),
            enabled: true,
        },
        Node::Spacer,
        header("System generations", SectionKind::Generations(ProfileKind::System), Some(1)),
        Node::Generation {
            generation: Generation {
                id: 42,
                created_ms: Some(0),
                store_path: Some("/nix/store/42-nixos-system".to_string()),
                label: Some("26.11.20260804.abc1234".to_string()),
                kernel: None,
                current: true,
                booted: false,
            },
            kind: ProfileKind::System,
            age_ms: Some(3_600_000),
        },
        Node::Spacer,
        header("Inputs", SectionKind::Inputs, Some(1)),
        Node::Input {
            input: Input {
                name: "nixpkgs".to_string(),
                origin: Some("NixOS/nixpkgs".to_string()),
                rev: Some("abc1234".to_string()),
                last_modified_secs: Some(1_700_000_000),
                direct: true,
            },
            age_days: Some(10),
        },
    ];

    let lines = draw("nixbox", &rows, StatusLine::Hints);

    assert!(lines.iter().any(|l| l.contains("reboot pending")), "RebootPending never reached the frame: {lines:#?}");
    assert!(lines.iter().any(|l| l.contains("booted 41") && l.contains("current 42")), "{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("/nix 62%")), "NixStore never reached the frame: {lines:#?}");
    assert!(lines.iter().any(|l| l.contains("5 system generations")), "{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("gc") && l.contains("keep 14d")), "NixPolicy never reached the frame: {lines:#?}");
    assert!(lines.iter().any(|l| l.contains("42") && l.contains("26.11.20260804")), "Generation never reached the frame: {lines:#?}");
    assert!(lines.iter().any(|l| l.contains("nixpkgs") && l.contains("NixOS/nixpkgs")), "Input never reached the frame: {lines:#?}");
}
/// The unit popup the design mocks up, drawn for real. Every assertion
/// here is about *placement*, because a transient's whole claim is that
/// it can be read by position: the same action sits in the same place
/// whether or not it applies here.
fn unit_popup(ownership_note: Option<&str>) -> ModalView<'static> {
    ModalView::Transient {
        title: "Unit . restic-backup.service".to_string(),
        switches: Vec::new(),
        groups: vec![
            ActionGroup {
                heading: "Runtime".to_string(),
                note: None,
                rows: vec![
                    ActionRow { chord: 's', label: "start", note: None, dimmed: false },
                    ActionRow { chord: 'S', label: "stop", note: None, dimmed: false },
                    ActionRow { chord: 'r', label: "restart", note: None, dimmed: false },
                    ActionRow { chord: 'R', label: "reload", note: None, dimmed: false },
                ],
            },
            ActionGroup {
                heading: "Persistence".to_string(),
                note: ownership_note.map(str::to_string),
                rows: ['e', 'd', 'm']
                    .into_iter()
                    .zip(["enable", "disable", "mask"])
                    .map(|(chord, label)| ActionRow {
                        chord,
                        label,
                        note: ownership_note.map(|_| "reverts on nixos-rebuild".to_string()),
                        dimmed: ownership_note.is_some(),
                    })
                    .collect(),
            },
            ActionGroup {
                heading: "Inspect".to_string(),
                note: None,
                rows: vec![
                    ActionRow { chord: 'l', label: "logs", note: None, dimmed: false },
                    ActionRow { chord: 'p', label: "properties", note: None, dimmed: false },
                ],
            },
        ],
    }
}

fn draw_modal_frame(modal: ModalView<'_>, width: u16, height: u16) -> Vec<String> {
    let theme = Theme::default();
    let hints: [KeyBinding; 0] = [];
    let rows = system_section();
    let model = View {
        header: Header::Status { hostname: "devbox", timestamp: TIMESTAMP },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: Some(modal),
        hints: &hints,
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &model, &theme);
        })
        .expect("a drawn frame");
    buffer_lines(&terminal)
}

/// The transient renders at all. It returned `(0, 0)` and drew nothing
/// until this change, so the first thing worth pinning is that its
/// groups, chords and title reach the cells.
#[test]
fn a_transient_draws_its_title_groups_and_chords() {
    let lines = draw_modal_frame(unit_popup(None), WIDTH, HEIGHT);

    assert!(lines.iter().any(|l| l.contains("Unit . restic-backup.service")), "the popup names its subject: {lines:#?}");
    for heading in ["Runtime", "Persistence", "Inspect"] {
        assert!(lines.iter().any(|l| l.contains(heading)), "{heading} is missing: {lines:#?}");
    }
    for label in ["start", "stop", "restart", "reload", "enable", "disable", "mask", "logs", "properties"] {
        assert!(lines.iter().any(|l| l.contains(label)), "{label} is missing: {lines:#?}");
    }
    for line in &lines {
        assert!(UnicodeWidthStr::width(line.as_str()) <= WIDTH as usize, "{line:?} runs past the frame");
    }
}

/// The design draws `s start` and `S stop` on one line. A group with no
/// notes pairs its rows, which is what makes a four-action group two
/// lines rather than four.
#[test]
fn a_group_with_no_notes_pairs_its_rows_two_to_a_line() {
    let lines = draw_modal_frame(unit_popup(None), WIDTH, HEIGHT);

    let paired = lines.iter().find(|l| l.contains("start")).expect("a start row");
    assert!(paired.contains("stop"), "start and stop share a line: {paired:?}");
    let second = lines.iter().find(|l| l.contains("restart")).expect("a restart row");
    assert!(second.contains("reload"), "restart and reload share a line: {second:?}");
    assert!(lines.iter().find(|l| l.contains("logs")).is_some_and(|l| l.contains("properties")), "a two-row group is one line: {lines:#?}");
}

/// The ownership guard, drawn. A group note trails its heading and each
/// dimmed row keeps its own note - and, crucially, every row is still
/// *there*: masys marks an action it cannot make persist, it never drops
/// it.
#[test]
fn the_ownership_guard_notes_the_group_and_keeps_every_row() {
    let lines = draw_modal_frame(unit_popup(Some("! declared in nix")), WIDTH, HEIGHT);

    let heading = lines.iter().find(|l| l.contains("Persistence")).expect("a Persistence heading");
    assert!(heading.contains("! declared in nix"), "the group note trails the heading, marker and all: {heading:?}");
    for label in ["enable", "disable", "mask"] {
        let row = lines.iter().find(|l| l.contains(label)).unwrap_or_else(|| panic!("{label} is still listed: {lines:#?}"));
        assert!(row.contains("reverts on nixos-rebuild"), "{label} says why it is dimmed: {row:?}");
    }
}

/// An annotated group goes one row to a line, so a note always sits
/// beside the row it belongs to. Paired, `enable`'s note would sit next
/// to `disable` and read as `disable`'s.
#[test]
fn an_annotated_group_goes_one_row_to_a_line() {
    let lines = draw_modal_frame(unit_popup(Some("! declared in nix")), WIDTH, HEIGHT);

    let enable = lines.iter().find(|l| l.contains("enable")).expect("an enable row");
    assert!(!enable.contains("disable"), "a noted row does not share its line: {enable:?}");
}

/// Dimming is a style, not an omission. The cells say so: a dimmed row
/// carries `Modifier::DIM` and a live one does not, while both occupy a
/// real position in the popup.
#[test]
fn a_dimmed_row_is_styled_dim_and_a_live_one_is_not() {
    let theme = Theme::default();
    let hints: [KeyBinding; 0] = [];
    let rows = system_section();
    let model = View {
        header: Header::Status { hostname: "devbox", timestamp: TIMESTAMP },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: Some(unit_popup(Some("! declared in nix"))),
        hints: &hints,
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &model, &theme);
        })
        .expect("a drawn frame");

    let buffer = terminal.backend().buffer();
    let dim_at = |needle: &str| {
        let (x, y) = (0..buffer.area.height)
            .find_map(|y| {
                let line: String = (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect();
                line.find(needle).map(|x| (x as u16, y))
            })
            .unwrap_or_else(|| panic!("{needle} is drawn somewhere"));
        buffer[(x, y)].style().add_modifier.contains(Modifier::DIM)
    };

    assert!(dim_at("mask"), "a row the guard dims is drawn dim");
    assert!(!dim_at("restart"), "a row the guard does not dim is drawn normally");
}

/// Switches lead, grouped, with their state already resolved into a box
/// the reader can see. An unsupported switch is listed and dimmed rather
/// than dropped, for the same reason a dimmed action row is.
#[test]
fn switches_are_drawn_above_the_actions_with_their_state_resolved() {
    let modal = ModalView::Transient {
        title: "Rebuild".to_string(),
        switches: vec![
            SwitchRow { group: "Arguments", chord: "-v", label: "verbose", supported: true, on: true },
            SwitchRow { group: "Arguments", chord: "-n", label: "dry run", supported: true, on: false },
            SwitchRow { group: "Arguments", chord: "-i", label: "impure", supported: false, on: false },
        ],
        groups: vec![ActionGroup {
            heading: "Rebuild".to_string(),
            note: None,
            rows: vec![ActionRow { chord: 's', label: "switch", note: None, dimmed: false }],
        }],
    };
    let lines = draw_modal_frame(modal, WIDTH, HEIGHT);

    let verbose = lines.iter().position(|l| l.contains("verbose")).expect("a verbose switch");
    let switch = lines.iter().position(|l| l.contains("switch")).expect("a switch action");
    assert!(verbose < switch, "switches are read before the actions they change: {lines:#?}");

    assert!(lines[verbose].contains("[x]"), "an enabled switch is ticked: {:?}", lines[verbose]);
    assert!(lines.iter().any(|l| l.contains("dry run") && l.contains("[ ]")), "a disabled switch is empty: {lines:#?}");
    assert!(lines.iter().any(|l| l.contains("impure")), "an unsupported switch is still listed: {lines:#?}");
}

/// A transient longer than the terminal reports what it got and what it
/// wanted, which is what `Metrics` is for and what a scrolling modal will
/// read. Phase 3's Nix transient is the long one.
///
/// Asserted on `Metrics` rather than on cells because the cells cannot
/// tell: ratatui 0.30 clips a `Block` to the buffer itself, so an
/// unclamped popup draws the same frame as a clamped one. The height is
/// still worth clamping - `modal_height` is documented as the height the
/// popup *got*, and an unclamped one reports a height that was never
/// drawn.
#[test]
fn a_transient_taller_than_the_frame_reports_what_it_could_not_show() {
    let theme = Theme::default();
    let hints: [KeyBinding; 0] = [];
    let rows = system_section();
    let model = View {
        header: Header::Status { hostname: "devbox", timestamp: TIMESTAMP },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: Some(ModalView::Transient {
            title: "Everything".to_string(),
            switches: Vec::new(),
            groups: (0..12)
                .map(|group| ActionGroup {
                    heading: format!("Group {group}"),
                    note: None,
                    rows: vec![ActionRow { chord: 'a', label: "an action", note: Some("a note".to_string()), dimmed: false }],
                })
                .collect(),
        }),
        hints: &hints,
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    const SHORT: u16 = 12;
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, SHORT)).expect("a test terminal");
    let mut metrics = None;
    terminal
        .draw(|frame| {
            metrics = Some(render(frame, &model, &theme));
        })
        .expect("a drawn frame");
    let metrics = metrics.expect("a drawn frame reports its metrics");

    assert!(metrics.modal_height <= SHORT, "the popup is never taller than the frame: {metrics:?}");
    assert!(metrics.modal_content > metrics.modal_height, "and it says how much it could not show: {metrics:?}");

    let lines = buffer_lines(&terminal);
    assert!(!lines.iter().any(|l| l.contains("Group 11")), "content past the frame is clipped, not grown into: {lines:#?}");
    for line in &lines {
        assert!(UnicodeWidthStr::width(line.as_str()) <= WIDTH as usize, "{line:?} runs past the frame");
    }
}

/// The `Input` sub-step draws its prompt, what has been typed, and its
/// candidates - the last of which was dropped on the floor until now.
#[test]
fn an_input_draws_its_prompt_typed_text_and_candidates() {
    let lines = draw_modal_frame(
        ModalView::Input {
            prompt: "retention",
            typed: "30",
            candidates: Some(CandidateList { matches: vec!["30d", "60d", "90d"], selected: 1 }),
        },
        WIDTH,
        HEIGHT,
    );

    assert!(lines.iter().any(|l| l.contains("retention: 30")), "the prompt and the typed text: {lines:#?}");
    for candidate in ["30d", "60d", "90d"] {
        assert!(lines.iter().any(|l| l.contains(candidate)), "{candidate} is offered: {lines:#?}");
    }
    assert!(lines.iter().any(|l| l.contains("input")), "the popup is titled for what it is: {lines:#?}");
}

/// The highlighted candidate is reversed, which is what a cursor looks
/// like everywhere else in masys. Without it the popup lists three
/// choices and gives no way to tell which one enter would take.
#[test]
fn the_selected_candidate_is_the_only_one_reversed() {
    let theme = Theme::default();
    let hints: [KeyBinding; 0] = [];
    let rows = system_section();
    let model = View {
        header: Header::Status { hostname: "devbox", timestamp: TIMESTAMP },
        rows: &rows,
        selected: None,
        status: StatusLine::Hints,
        modal: Some(ModalView::Input {
            prompt: "retention",
            typed: "30",
            candidates: Some(CandidateList { matches: vec!["30d", "60d", "90d"], selected: 1 }),
        }),
        hints: &hints,
        actions: &[],
        filter: None,
        typing: false,
        filter_matches: None,
        auto_refresh_paused: false,
    };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &model, &theme);
        })
        .expect("a drawn frame");

    let buffer = terminal.backend().buffer();
    let reversed = |needle: &str| {
        let (x, y) = (0..buffer.area.height)
            .find_map(|y| {
                let line: String = (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect();
                line.find(needle).map(|x| (x as u16, y))
            })
            .unwrap_or_else(|| panic!("{needle} is drawn somewhere"));
        buffer[(x, y)].style().add_modifier.contains(Modifier::REVERSED)
    };

    assert!(reversed("60d"), "the selected candidate carries the cursor");
    assert!(!reversed("30d"), "an unselected candidate does not");
    assert!(!reversed("90d"), "and neither does the one after it");
}

/// A picker with nothing to offer says so. Drawn as blank space, an empty
/// match set reads as a picker still thinking.
#[test]
fn a_picker_with_no_matches_says_so() {
    let lines = draw_modal_frame(
        ModalView::Input { prompt: "package", typed: "zzz", candidates: Some(CandidateList { matches: Vec::new(), selected: 0 }) },
        WIDTH,
        HEIGHT,
    );

    assert!(lines.iter().any(|l| l.contains("no matches")), "{lines:#?}");
}

/// An input that takes free text rather than a choice carries no
/// candidate list at all, and draws as one line.
#[test]
fn an_input_with_no_candidates_draws_only_its_prompt() {
    let lines = draw_modal_frame(ModalView::Input { prompt: "query", typed: "ripgrep", candidates: None }, WIDTH, HEIGHT);

    assert!(lines.iter().any(|l| l.contains("query: ripgrep")), "{lines:#?}");
    assert!(!lines.iter().any(|l| l.contains("no matches")), "nothing is invented to fill the space: {lines:#?}");
}

/// A group note is drawn as written. The Nix transient's `Store` group is
/// annotated `policy: weekly, keep 14d`, which is a fact rather than a
/// warning - a renderer that decorated every note with `!` would report
/// a retention policy as an alarm.
#[test]
fn a_group_note_is_drawn_as_written_without_an_invented_marker() {
    let modal = ModalView::Transient {
        title: "Nix ops".to_string(),
        switches: Vec::new(),
        groups: vec![ActionGroup {
            heading: "Store".to_string(),
            note: Some("policy: weekly, keep 14d".to_string()),
            rows: vec![ActionRow { chord: 'o', label: "optimise", note: None, dimmed: false }],
        }],
    };
    let lines = draw_modal_frame(modal, WIDTH, HEIGHT);

    let heading = lines.iter().find(|l| l.contains("Store")).expect("a Store heading");
    assert!(heading.contains("policy: weekly, keep 14d"), "{heading:?}");
    assert!(!heading.contains('!'), "the renderer adds no marker of its own: {heading:?}");
}

/// The whole path, for the transient: a real session opens the popup on a
/// real unit row and a real frame draws it. Every other transient test
/// here hand-builds its `ModalView`, which cannot catch a `TransientDef`
/// whose projection and the renderer disagree.
#[test]
fn app_key_to_render_draws_the_unit_popup_on_a_real_frame() {
    let (system, platform) = degraded_host();
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(10_800_000, TIMESTAMP.to_string()).expect("tick");

    // Into the systemd buffer, then off its section header onto a unit.
    app.handle_key(masys_app::Key::char('3'));
    app.handle_key(masys_app::Key::new(masys_app::KeyCode::Down));
    app.handle_key(masys_app::Key::char('e'));

    let theme = Theme::default();
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("a test terminal");
    terminal
        .draw(|frame| {
            render(frame, &app.view(), &theme);
        })
        .expect("a drawn frame");
    let lines = buffer_lines(&terminal);

    assert!(lines.iter().any(|l| l.contains("Unit .")), "the popup names its unit: {lines:#?}");
    for expected in ["Runtime", "start", "stop", "Persistence", "enable", "Inspect", "logs"] {
        assert!(lines.iter().any(|l| l.contains(expected)), "{expected:?} is missing from {lines:#?}");
    }
    for line in &lines {
        assert!(UnicodeWidthStr::width(line.as_str()) <= WIDTH as usize, "{line:?} runs past the frame");
    }
}

/// A popup wider in its title than in its rows is sized to the title. A
/// transient names the thing it acts on, and that name is routinely
/// longer than `start` or `stop` - sized to the rows alone, the border
/// clipped the one part of the popup that says what you are about to act
/// on.
#[test]
fn a_popup_is_wide_enough_for_its_title() {
    let modal = ModalView::Transient {
        title: "Unit . a-very-long-unit-name.service".to_string(),
        switches: Vec::new(),
        groups: vec![ActionGroup {
            heading: "Runtime".to_string(),
            note: None,
            rows: vec![ActionRow { chord: 's', label: "start", note: None, dimmed: false }],
        }],
    };
    let lines = draw_modal_frame(modal, WIDTH, HEIGHT);

    let title = lines.iter().find(|l| l.contains("a-very-long-unit-name.service")).expect("a titled popup");
    assert!(title.contains("a-very-long-unit-name.service "), "the title keeps the padding it was written with: {title:?}");
    assert!(title.ends_with('\u{2510}'), "and the border closes after it: {title:?}");
}
