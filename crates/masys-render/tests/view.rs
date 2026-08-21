use masys_domain::finding::{Finding, PressureResource};
use masys_domain::journal::{Entry, Priority};
use masys_domain::platform::PendingReboot;
use masys_domain::proc_detail::{Fd, FdTarget, ProcDetail};
use masys_domain::rate::ProcRate;
use masys_domain::sample::{Filesystem, Proc, ProcState, SystemState};
use masys_domain::unit::{ActiveState, Unit, UnitKind};
use masys_render::theme::Theme;
use masys_render::view::{
    ProcColumns, ProcRow, filesystem_lines, finding_line, finding_lines, gauge, human_bytes, human_duration, interface_line, journal_lines,
    overview_lines, proc_columns_header, proc_detail_lines, proc_group_line, proc_lines, section_header_line, unit_lines,
};
use masys_view::{Node, Overview};
use ratatui::style::{Color, Modifier};
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

/// A flat row, for the tests that are not about the tree.
fn flat(unit: masys_domain::unit::Unit, age_ms: Option<u64>, expanded: bool) -> masys_render::view::UnitRow {
    masys_render::view::UnitRow { unit, age_ms, expanded, depth: 0, children: false }
}

fn text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// The glyph span and its color - the severity tier, which `text` discards.
fn tier(line: &Line) -> (String, Option<Color>) {
    (line.spans[0].content.to_string(), line.spans[0].style.fg)
}

#[test]
fn human_bytes_matches_the_mockups_units() {
    assert_eq!(human_bytes(18 * (1u64 << 40)), "18T");
    assert_eq!(human_bytes(4 * (1u64 << 40)), "4.0T");
    assert_eq!(human_bytes(12 * (1u64 << 30)), "12G");
    assert_eq!(human_bytes(1536 * (1u64 << 20)), "1.5G", "the single-digit branch keeps one decimal");
    assert_eq!(human_bytes(61 * (1u64 << 20)), "61M");
    assert_eq!(human_bytes(4 * (1u64 << 10)), "4.0K");
    assert_eq!(human_bytes(512), "512B");
    assert_eq!(human_bytes(0), "0B");
}

#[test]
fn human_duration_picks_the_two_biggest_units() {
    assert_eq!(human_duration(10_800_000), "3h");
    assert_eq!(human_duration(3_600_000), "1h");
    assert_eq!(human_duration(187_200_000), "2d 4h");
    assert_eq!(human_duration(90_000), "1m");
}

#[test]
fn a_failed_unit_line_shows_exit_code_and_duration() {
    let theme = Theme::default();
    let finding = Finding::FailedUnit { unit: "restic-backup.service".to_string(), exit_code: Some(1), since_ms: 10_800_000, reason: None };
    let line = finding_line(&finding, &theme);
    assert_eq!(text(&line), "x restic-backup.service  failed (exit 1)  3h ago");
}

#[test]
fn a_failed_unit_without_an_exit_code_just_says_failed() {
    let theme = Theme::default();
    let finding = Finding::FailedUnit { unit: "sshd.service".to_string(), exit_code: None, since_ms: 3_600_000, reason: None };
    assert_eq!(text(&finding_line(&finding, &theme)), "x sshd.service  failed  1h ago");
}

#[test]
fn a_pressure_line_omits_a_zero_full_value() {
    let theme = Theme::default();
    let finding = Finding::Pressure { resource: PressureResource::Io, some_avg60: 22.7, full_avg60: Some(0.0) };
    let line = finding_line(&finding, &theme);
    assert_eq!(text(&line), "^ io  some avg60  22.7%", "a measured-zero full value is blank, not printed");
}

#[test]
fn a_pressure_line_omits_a_full_value_that_would_render_as_zero() {
    let theme = Theme::default();
    let rounds_to_zero = Finding::Pressure { resource: PressureResource::Io, some_avg60: 22.7, full_avg60: Some(0.04) };
    let exactly_zero = Finding::Pressure { resource: PressureResource::Io, some_avg60: 22.7, full_avg60: Some(0.0) };
    assert_eq!(
        text(&finding_line(&rounds_to_zero, &theme)),
        text(&finding_line(&exactly_zero, &theme)),
        "0.04 would print as \"0.0%\" - the string the zero case exists to suppress"
    );
}

#[test]
fn a_pressure_line_shows_a_nonzero_full_value() {
    let theme = Theme::default();
    let finding = Finding::Pressure { resource: PressureResource::Memory, some_avg60: 41.2, full_avg60: Some(8.1) };
    let line = finding_line(&finding, &theme);
    assert_eq!(text(&line), "^ memory  some avg60  41.2%      full avg60  8.1%");
}

#[test]
fn a_disk_capacity_line_shows_generations_only_for_boot() {
    let theme = Theme::default();
    let boot =
        Finding::DiskCapacity { mount_point: "/boot".to_string(), used_percent: 88.0, free_bytes: 61 * (1u64 << 20), generations: Some(9) };
    assert_eq!(text(&finding_line(&boot, &theme)), "^ /boot  88%   61M free   .  9 generations");

    let nix =
        Finding::DiskCapacity { mount_point: "/nix".to_string(), used_percent: 91.0, free_bytes: 12 * (1u64 << 30), generations: None };
    assert_eq!(text(&finding_line(&nix, &theme)), "^ /nix  91%   12G free");
}

/// The severity tier is what the color communicates, and `text` is blind
/// to it - so every variant's glyph *and* color are pinned here.
#[test]
fn every_finding_variant_maps_onto_its_severity_tier() {
    let theme = Theme::default();
    let cases = vec![
        (
            Finding::FailedUnit { unit: "a.service".to_string(), exit_code: Some(2), since_ms: 60_000, reason: None },
            "x ",
            theme.severity_dead,
            "x a.service  failed (exit 2)  1m ago",
        ),
        (
            Finding::FailedUnit { unit: "a.service".to_string(), exit_code: None, since_ms: 60_000, reason: None },
            "x ",
            theme.severity_dead,
            "x a.service  failed  1m ago",
        ),
        (
            Finding::FlappingUnit { unit: "a.service".to_string(), restarts: 5, window_ms: 3_600_000 },
            "~ ",
            theme.severity_warning,
            "~ a.service  5 restarts in 1h",
        ),
        (
            Finding::Pressure { resource: PressureResource::Cpu, some_avg60: 30.0, full_avg60: None },
            "^ ",
            theme.severity_warning,
            "^ cpu  some avg60  30.0%",
        ),
        (
            Finding::DiskCapacity { mount_point: "/".to_string(), used_percent: 90.0, free_bytes: 1u64 << 30, generations: None },
            "^ ",
            theme.severity_warning,
            "^ /  90%   1.0G free",
        ),
        (
            Finding::InodeExhaustion { mount_point: "/var".to_string(), inode_used_percent: 94.0 },
            "^ ",
            theme.severity_warning,
            "^ /var  94% inodes used",
        ),
        (Finding::ReadOnlyFilesystem { mount_point: "/home".to_string() }, "^ ", theme.severity_warning, "^ /home  remounted read-only"),
        (Finding::ClockUnsynchronized, "! ", theme.severity_urgent, "! clock not synchronized"),
        (
            Finding::OomKill { pid: 4213, comm: "firefox".to_string(), timestamp_ms: 1_000 },
            "! ",
            theme.severity_urgent,
            "! firefox  killed by OOM (pid 4213)",
        ),
        (Finding::SystemDegraded { failed_units: 3 }, "! ", theme.severity_urgent, "! system degraded (3 failed units)"),
    ];

    for (finding, glyph, color, expected) in cases {
        let line = finding_line(&finding, &theme);
        assert_eq!(tier(&line), (glyph.to_string(), Some(color)), "{finding:?}");
        assert_eq!(text(&line), expected, "{finding:?}");
    }
}

#[test]
fn finding_lines_pads_labels_to_a_common_width() {
    let theme = Theme::default();
    let pressure = vec![
        Finding::Pressure { resource: PressureResource::Memory, some_avg60: 41.2, full_avg60: Some(8.1) },
        Finding::Pressure { resource: PressureResource::Io, some_avg60: 22.7, full_avg60: Some(0.0) },
    ];
    let lines = finding_lines(&pressure, &theme);
    assert_eq!(text(&lines[0]), "^ memory  some avg60  41.2%      full avg60  8.1%");
    assert_eq!(text(&lines[1]), "^ io      some avg60  22.7%", "io is padded out to memory's width");

    let disks = vec![
        Finding::DiskCapacity { mount_point: "/nix".to_string(), used_percent: 91.0, free_bytes: 12 * (1u64 << 30), generations: None },
        Finding::DiskCapacity { mount_point: "/boot".to_string(), used_percent: 88.0, free_bytes: 61 * (1u64 << 20), generations: Some(9) },
    ];
    let lines = finding_lines(&disks, &theme);
    assert_eq!(text(&lines[0]), "^ /nix   91%   12G free");
    assert_eq!(text(&lines[1]), "^ /boot  88%   61M free   .  9 generations");
}

#[test]
fn finding_lines_pads_by_terminal_display_width() {
    let theme = Theme::default();
    let findings = vec![
        // 9 chars, 17 bytes, 13 columns: each CJK glyph is one char but
        // two columns, so this is the *widest* label on screen while
        // being the middle one by char count. Only a display-width
        // measure picks it as the column the others pad out to.
        Finding::ReadOnlyFilesystem { mount_point: "/mnt/数据数据".to_string() },
        // 12 chars, 13 bytes, 12 columns - where bytes alone go wrong.
        Finding::ReadOnlyFilesystem { mount_point: "/mnt/données".to_string() },
        Finding::ReadOnlyFilesystem { mount_point: "/boot".to_string() },
    ];
    let lines = finding_lines(&findings, &theme);
    assert_eq!(text(&lines[0]), "^ /mnt/数据数据  remounted read-only");
    assert_eq!(text(&lines[1]), "^ /mnt/données   remounted read-only");
    assert_eq!(text(&lines[2]), "^ /boot          remounted read-only");

    // The invariant those literals encode: every tail starts in the same
    // terminal column, which is the whole point of the function. 17 = 2
    // for the glyph, 13 for the widest label, 2 for the tail separator.
    let tail_columns: Vec<usize> = lines.iter().map(|line| UnicodeWidthStr::width(text(line).split("remounted").next().unwrap())).collect();
    assert_eq!(tail_columns, vec![17, 17, 17], "tails must align in columns, not chars");
}

#[test]
fn finding_lines_leaves_a_label_less_finding_unindented() {
    let theme = Theme::default();
    let findings = vec![
        Finding::FailedUnit { unit: "restic-backup.service".to_string(), exit_code: Some(1), since_ms: 60_000, reason: None },
        Finding::ClockUnsynchronized,
    ];
    let lines = finding_lines(&findings, &theme);
    assert_eq!(text(&lines[1]), "! clock not synchronized", "no label means no padding, not a wide indent");
}

#[test]
fn a_lone_finding_is_padded_exactly_as_the_single_finding_entry_point() {
    let theme = Theme::default();
    let finding = Finding::Pressure { resource: PressureResource::Memory, some_avg60: 41.2, full_avg60: Some(8.1) };
    assert_eq!(text(&finding_lines(std::slice::from_ref(&finding), &theme)[0]), text(&finding_line(&finding, &theme)));
    assert!(finding_lines(&[], &theme).is_empty());
}

#[test]
fn a_section_header_shows_its_count() {
    let theme = Theme::default();
    assert_eq!(text(&section_header_line("Failed units", Some(1), &theme)), "Failed units (1)");
    assert_eq!(text(&section_header_line("System", None, &theme)), "System");
}

#[test]
fn a_section_header_is_bold_and_themed() {
    let theme = Theme::default();
    let line = section_header_line("Failed units", Some(1), &theme);
    assert_eq!(line.spans[0].style.fg, Some(theme.section_header));
    assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD), "{:?}", line.spans[0].style);
}

fn healthy_overview() -> Overview {
    Overview {
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
    }
}

#[test]
fn overview_with_nothing_measured_shows_only_state_and_clock() {
    let theme = Theme::default();
    let lines = overview_lines(&healthy_overview(), &theme);
    assert_eq!(lines.len(), 2, "line2 (mem/zram/swap) is entirely None, so it's omitted: {lines:#?}");
    assert_eq!(text(&lines[0]), ". running  .  47 units");
    assert_eq!(text(&lines[1]), ". clock synced");
}

#[test]
fn overview_reports_the_unhealthy_side_of_each_flag() {
    let theme = Theme::default();
    let overview = Overview { clock_synced: false, smart_ok: Some(false), ..healthy_overview() };
    let lines = overview_lines(&overview, &theme);
    assert_eq!(text(&lines[1]), ". clock not synced  .  smart failing");
}

#[test]
fn overview_with_everything_measured_shows_all_three_lines() {
    let theme = Theme::default();
    let overview = Overview {
        load_1: Some(1.82),
        load_5: Some(1.44),
        load_15: Some(1.20),
        uptime_secs: Some(2 * 86_400 + 4 * 3600),
        mem_used_bytes: Some(29_400_000_000),
        mem_total_bytes: Some(32_000_000_000),
        zram_percent: Some(100.0),
        swap_free_bytes: Some(0),
        smart_ok: Some(true),
        pending_reboot: Some(PendingReboot { reason: "generation 412 not yet booted".to_string() }),
        ..healthy_overview()
    };
    let lines = overview_lines(&overview, &theme);
    assert_eq!(lines.len(), 3, "{lines:#?}");
    assert!(text(&lines[0]).contains("load 1.82 1.44 1.20"), "{}", text(&lines[0]));
    assert!(text(&lines[0]).contains("up 2d 4h"), "{}", text(&lines[0]));
    assert_eq!(text(&lines[1]), ". mem 27G/30G  .  zram 100%  .  swap 0B free", "a measured-zero swap prints as 0B, not omitted");
    assert!(text(&lines[2]).contains("reboot pending: generation 412 not yet booted"), "{}", text(&lines[2]));
}

#[test]
fn a_single_failed_unit_is_not_pluralised() {
    let theme = Theme::default();
    assert_eq!(text(&finding_line(&Finding::SystemDegraded { failed_units: 1 }, &theme)), "! system degraded (1 failed unit)");
    assert_eq!(text(&finding_line(&Finding::SystemDegraded { failed_units: 3 }, &theme)), "! system degraded (3 failed units)");
}

fn a_proc(pid: u32, comm: &str, rss_mb: u64) -> Proc {
    Proc {
        pid,
        comm: comm.to_string(),
        cgroup: None,
        cpu_ticks: 0,
        rss_bytes: rss_mb * 1024 * 1024,
        io_read_bytes: 0,
        io_write_bytes: 0,
        state: ProcState::Sleeping,
        nice: 0,
        oom_score: 0,
        threads: 1,
        started_at_ms: 0,
    }
}

/// One row as `proc_lines` takes it: the process, its rate, and the
/// cumulative CPU seconds the app derived. Ageless, because most of these
/// tests are about the columns rather than the clock.
fn row(proc: Proc, rate: Option<ProcRate>, cpu_seconds: u64) -> ProcRow {
    ProcRow { proc, rate, cpu_seconds, age_ms: None }
}

/// The layout every test below uses unless it is specifically about the
/// width: the narrow one, which is what the table looked like before it
/// could see a terminal.
fn narrow() -> ProcColumns {
    ProcColumns::default()
}

fn proc_lines_narrow(rows: &[ProcRow], theme: &Theme) -> Vec<Line<'static>> {
    proc_lines(rows, theme, &narrow())
}

fn proc_group_line_narrow(node: &Node, theme: &Theme) -> Line<'static> {
    proc_group_line(node, theme, &narrow())
}

fn a_rate(pid: u32, cpu: f32) -> ProcRate {
    ProcRate { pid, cpu_percent: cpu, io_read_bytes_per_sec: 0.0, io_write_bytes_per_sec: 0.0 }
}

#[test]
fn a_process_row_shows_its_name_cpu_and_memory() {
    let theme = Theme::default();
    let lines = proc_lines_narrow(&[row(a_proc(1, "chrome", 649), Some(a_rate(1, 35.4)), 0)], &theme);
    let rendered = text(&lines[0]);
    // The pid leads the row now, which is what disambiguates the several
    // processes a machine runs under one name.
    assert!(rendered.trim_start().starts_with("1  chrome"), "{rendered:?}");
    assert!(rendered.contains("35.4%"), "{rendered:?}");
    assert!(rendered.contains("649M"), "{rendered:?}");
}

/// The design's rule, stated for the Procs buffer and applying equally
/// here: `-` means not yet measured, and must never be shown as `0.0%`.
#[test]
fn an_unmeasured_process_shows_a_dash_not_zero() {
    let theme = Theme::default();
    let lines = proc_lines_narrow(&[row(a_proc(1, "chrome", 649), None, 0)], &theme);
    let rendered = text(&lines[0]);
    assert!(rendered.contains('-'), "{rendered:?}");
    assert!(!rendered.contains("0.0%"), "not-yet-measured must not read as idle: {rendered:?}");
}

/// A genuinely idle process is measured, and reads as zero rather than
/// as unknown - the other half of the same rule.
#[test]
fn a_measured_idle_process_shows_zero_not_a_dash() {
    let theme = Theme::default();
    let lines = proc_lines_narrow(&[row(a_proc(1, "chrome", 649), Some(a_rate(1, 0.0)), 0)], &theme);
    let rendered = text(&lines[0]);
    assert!(rendered.contains("0.0%"), "measured-idle reads as zero: {rendered:?}");
    assert!(!rendered.contains('-'), "{rendered:?}");
}

/// Names are padded to a common width so the CPU and memory columns line
/// up, the same way `finding_lines` aligns a section's tails.
#[test]
fn names_are_padded_so_the_columns_align() {
    let theme = Theme::default();
    let lines = proc_lines_narrow(
        &[row(a_proc(1, "chrome", 649), Some(a_rate(1, 35.4)), 0), row(a_proc(2, "rust-analyzer", 1601), Some(a_rate(2, 0.1)), 0)],
        &theme,
    );
    let columns: Vec<usize> = lines.iter().map(|l| text(l).find('%').expect("a percent sign")).collect();
    assert_eq!(columns[0], columns[1], "{:#?}", lines.iter().map(text).collect::<Vec<_>>());
}

/// The two rankings render as separate batches and stack directly on top
/// of each other, so their columns must agree even though neither can see
/// the other's names. A per-batch width would put the CPU column in two
/// places on one screen.
#[test]
fn the_columns_agree_across_two_separately_rendered_sections() {
    let theme = Theme::default();
    let cpu_section = proc_lines_narrow(&[row(a_proc(1, "chrome", 649), Some(a_rate(1, 35.4)), 0)], &theme);
    let memory_section = proc_lines_narrow(&[row(a_proc(2, "rust-analyzer", 1601), Some(a_rate(2, 0.1)), 0)], &theme);
    let percent_at = |l: &Line| text(l).find('%').expect("a percent sign");
    assert_eq!(
        percent_at(&cpu_section[0]),
        percent_at(&memory_section[0]),
        "{:?} vs {:?}",
        text(&cpu_section[0]),
        text(&memory_section[0])
    );
}

/// `/proc/[pid]/comm` is capped at 15 characters by the kernel
/// (TASK_COMM_LEN), so the widest possible name still fits the column
/// without truncation.
#[test]
fn the_widest_possible_comm_needs_no_truncation() {
    let theme = Theme::default();
    let lines = proc_lines_narrow(&[row(a_proc(1, "123456789012345", 1), Some(a_rate(1, 1.0)), 0)], &theme);
    assert!(text(&lines[0]).contains("123456789012345"), "{:?}", text(&lines[0]));
}

/// `io` is split evenly between read and write, so the narrow layout's
/// summed `IO/s` column still sees the figure each test names.
fn a_group(name: &str, cpu: f32, mem_mb: u64, io: f64, count: u32, expanded: bool) -> Node {
    Node::ProcGroup {
        name: name.to_string(),
        depth: 0,
        expanded,
        cpu_percent: cpu,
        mem_bytes: mem_mb * 1024 * 1024,
        read_bytes_per_sec: io / 2.0,
        write_bytes_per_sec: io / 2.0,
        proc_count: count,
        sparkline: Vec::new(),
    }
}

/// A cgroup path is long and front-loaded with the parts every row
/// shares, so truncating the whole path to the column width identifies
/// nothing: `/user.slice/user-1000.slice/user@1000.service/app.slice/x`
/// becomes `/user.slice/user-1000.`, which is the same prefix as its
/// siblings.
#[test]
fn a_group_row_shows_the_leaf_cgroup_not_the_whole_path() {
    let theme = Theme::default();
    let node = a_group("/user.slice/user-1000.slice/user@1000.service", 1.0, 10, 0.0, 3, true);
    let rendered = text(&proc_group_line(&node, &theme, &narrow()));
    assert!(rendered.contains("user@1000.service"), "{rendered:?}");
    assert!(!rendered.contains("/user.slice"), "the shared prefix is dropped: {rendered:?}");

    // The kernel bucket has no path separators and survives unchanged.
    let kernel = text(&proc_group_line_narrow(&a_group("kernel", 0.0, 0, 0.0, 145, false), &theme));
    assert!(kernel.contains("kernel"), "{kernel:?}");
}

/// Group rows and process rows sit directly on top of each other, so
/// their columns have to line up despite being built by different
/// functions from different data.
#[test]
fn group_and_process_rows_share_their_columns() {
    let theme = Theme::default();
    let group = text(&proc_group_line_narrow(&a_group("/system.slice/sshd.service", 44.1, 1433, 0.0, 38, true), &theme));
    let proc = text(&proc_lines_narrow(&[row(a_proc(1204, "sshd", 649), Some(a_rate(1204, 2.3)), 0)], &theme)[0]);
    assert_eq!(group.find('%').expect("a group percent"), proc.find('%').expect("a process percent"), "{group:?} vs {proc:?}");
}

#[test]
fn a_group_row_shows_its_fold_marker_and_aggregates() {
    let theme = Theme::default();
    let open = text(&proc_group_line_narrow(&a_group("/system.slice", 44.1, 1433, 2_100_000.0, 38, true), &theme));
    assert!(open.starts_with("v "), "an expanded group points down: {open:?}");
    assert!(open.contains("system.slice"), "{open:?}");
    assert!(open.contains("44.1%"), "{open:?}");
    assert!(open.contains("38"), "the process count: {open:?}");

    let shut = text(&proc_group_line_narrow(&a_group("/system.slice", 44.1, 1433, 2_100_000.0, 38, false), &theme));
    assert!(shut.starts_with("> "), "a collapsed group points right: {shut:?}");
}

/// The design's rendering rule for this buffer, in its own words: blank
/// means measured-zero, `-` means not yet measured. A group whose members
/// have no rates yet has no IO figure at all.
#[test]
fn a_group_with_no_io_shows_blank_not_zero() {
    let theme = Theme::default();
    let rendered = text(&proc_group_line_narrow(&a_group("/kernel", 0.0, 0, 0.0, 112, true), &theme));
    assert!(!rendered.contains("0B"), "measured-zero memory and io are blank, not 0B: {rendered:?}");
    assert!(rendered.contains("112"), "the process count still shows: {rendered:?}");

    // A group that genuinely moves bytes still reports them. No `/s`
    // suffix on the value - the column heading already says IO/s, and
    // the process rows beneath omit it for the same reason.
    let busy = text(&proc_group_line_narrow(&a_group("/system.slice", 1.0, 1433, 2_100_000.0, 38, true), &theme));
    assert!(busy.contains("2.0M"), "a busy group shows an io rate: {busy:?}");
}

/// Every column the Procs buffer offers, all of it from data `Proc`
/// already carries - no new adapter read paid for a wider table.
#[test]
fn a_process_row_carries_every_column() {
    let theme = Theme::default();
    let mut proc = a_proc(1204, "chrome", 486);
    proc.threads = 31;
    proc.state = ProcState::Running;
    let mut rate = a_rate(1204, 2.3);
    rate.io_read_bytes_per_sec = 1_000_000.0;
    rate.io_write_bytes_per_sec = 200_000.0;

    let rendered = text(&proc_lines_narrow(&[row(proc, Some(rate), 1324)], &theme)[0]);
    assert!(rendered.contains("1204"), "the pid: {rendered:?}");
    assert!(rendered.contains("chrome"), "{rendered:?}");
    assert!(rendered.contains("2.3%"), "{rendered:?}");
    assert!(rendered.contains("486M"), "{rendered:?}");
    assert!(rendered.contains("1.1M"), "io/s is read plus write: {rendered:?}");
    assert!(rendered.contains("31"), "thread count: {rendered:?}");
    assert!(rendered.contains(" R "), "the state letter: {rendered:?}");
    assert!(rendered.contains("22:04"), "1324 seconds is 22m04s: {rendered:?}");
}

/// Over an hour, TIME grows a field rather than rolling over - a process
/// up for two days reading `04:11` would be actively misleading.
#[test]
fn cumulative_cpu_time_shows_hours_when_it_has_them() {
    let theme = Theme::default();
    let rendered = text(&proc_lines_narrow(&[row(a_proc(1, "x", 1), None, 4_462)], &theme)[0]);
    assert!(rendered.contains("1:14:22"), "4462 seconds is 1h14m22s: {rendered:?}");
}

/// Kernel threads use no memory and do no IO. Both blank, for the same
/// reason the group rows blank them.
#[test]
fn a_process_with_no_memory_or_io_leaves_those_columns_blank() {
    let theme = Theme::default();
    let mut proc = a_proc(2, "kthreadd", 0);
    proc.threads = 1;
    let rendered = text(&proc_lines_narrow(&[row(proc, Some(a_rate(2, 0.0)), 0)], &theme)[0]);
    assert!(!rendered.contains("0B"), "{rendered:?}");
}

/// The table used to be a fixed 65 columns however wide the terminal was,
/// leaving a third of a modern window blank. Everything spare goes to the
/// name column, which pins the numbers to the right edge.
#[test]
fn the_name_column_absorbs_whatever_the_terminal_has_spare() {
    let theme = Theme::default();
    let wide = ProcColumns::fit(140);
    let rendered = text(&proc_lines(&[row(a_proc(1, "chrome", 649), Some(a_rate(1, 35.4)), 0)], &theme, &wide)[0]);
    assert_eq!(rendered.width(), 140, "the row reaches the right edge: {rendered:?}");
    assert!(rendered.width() > text(&proc_lines_narrow(&[row(a_proc(1, "chrome", 649), Some(a_rate(1, 35.4)), 0)], &theme)[0]).width());
}

/// The header is laid out from the same `ProcColumns` the rows are, so
/// every heading sits over its own column at any width. This is the
/// alignment that silently drifts if each side computes its own spacing.
#[test]
fn the_header_lines_up_with_the_rows_at_any_width() {
    let theme = Theme::default();
    for width in [65, 80, 100, 140, 200] {
        let columns = ProcColumns::fit(width);
        // Sorted by name, so the `v` marker lands on `NAME` - which is
        // left-aligned and shifts nothing - rather than inside the
        // right-aligned column this compares.
        let header = proc_columns_header(masys_view::ProcSort::Name, true, &columns);
        let rendered = text(&proc_lines(&[row(a_proc(1204, "sshd", 649), Some(a_rate(1204, 2.3)), 0)], &theme, &columns)[0]);
        assert_eq!(header.width(), rendered.width(), "at {width}: {header:?} vs {rendered:?}");
        // `%CPU` is right-aligned in its column and so is `2.3%`, so
        // their trailing edges are the column's edge.
        assert_eq!(header.find("%CPU").map(|at| at + 4), rendered.find("2.3%").map(|at| at + 4), "at {width}: {header:?} vs {rendered:?}");
    }
}

/// A column that does not fit stops the ones after it, so the extras are
/// always a prefix. Otherwise a terminal one column wider would insert a
/// heading in the middle of the table and shove the rest sideways.
#[test]
fn extra_columns_switch_on_left_to_right_as_the_width_allows() {
    let seen = |width: usize| {
        let c = ProcColumns::fit(width);
        (c.split_io, c.uptime, c.nice, c.oom)
    };
    assert_eq!(seen(65), (false, false, false, false), "the narrowest table is what it always was");
    assert_eq!(seen(73), (true, false, false, false));
    assert_eq!(seen(81), (true, true, false, false));
    assert_eq!(seen(85), (true, true, true, false));
    assert_eq!(seen(90), (true, true, true, true));

    // Never a hole: no width switches a later column on while an earlier
    // one is off.
    for width in 0..200 {
        let c = ProcColumns::fit(width);
        let on = [c.split_io, c.uptime, c.nice, c.oom];
        assert!(on.windows(2).all(|w| w[0] || !w[1]), "a gap at width {width}: {on:?}");
    }
}

/// Too narrow to split, and read plus write are summed back into one
/// column rather than one of them being dropped - a row that showed only
/// reads would understate a process filling a disk.
#[test]
fn a_narrow_table_sums_read_and_write_back_into_one_column() {
    let theme = Theme::default();
    let mut rate = a_rate(1, 1.0);
    rate.io_read_bytes_per_sec = 1_000_000.0;
    rate.io_write_bytes_per_sec = 1_000_000.0;

    let narrow = text(&proc_lines_narrow(&[row(a_proc(1, "chrome", 649), Some(rate), 0)], &theme)[0]);
    assert!(narrow.contains("1.9M"), "one column carrying both halves: {narrow:?}");

    let wide = text(&proc_lines(&[row(a_proc(1, "chrome", 649), Some(rate), 0)], &theme, &ProcColumns::fit(140))[0]);
    assert_eq!(wide.matches("977K").count(), 2, "read and write, each in its own column: {wide:?}");
}

/// Wall-clock age, not CPU time. A daemon up for a week having spent four
/// seconds on the CPU and a service that restarted forty seconds ago look
/// identical in `TIME`, and opposite in `UP`.
#[test]
fn the_uptime_column_reads_wall_clock_age() {
    let theme = Theme::default();
    let wide = ProcColumns::fit(140);
    let veteran = ProcRow { proc: a_proc(1, "sshd", 8), rate: None, cpu_seconds: 4, age_ms: Some(7 * 24 * 3_600_000) };
    let restarted = ProcRow { proc: a_proc(2, "flapping", 8), rate: None, cpu_seconds: 4, age_ms: Some(40_000) };

    let lines: Vec<String> = proc_lines(&[veteran, restarted], &theme, &wide).iter().map(text).collect();
    assert!(lines[0].contains("7d"), "a week of uptime: {:?}", lines[0]);
    assert!(lines[1].contains("0m"), "forty seconds rounds down, and that is the tell: {:?}", lines[1]);
}

/// A process whose start time never arrived shows `-`, the same
/// not-yet-measured mark the CPU and IO columns use. Zero would read as
/// "just started", which is the one thing this column exists to report.
#[test]
fn an_unknown_start_time_shows_a_dash_not_zero() {
    let theme = Theme::default();
    let rendered = text(&proc_lines(&[row(a_proc(1, "sshd", 8), None, 4)], &theme, &ProcColumns::fit(140))[0]);
    assert!(!rendered.contains("0m"), "an unknown age must not read as freshly started: {rendered:?}");
}

/// `nice` and `oom_score` are 0 for almost every process, and a column of
/// zeroes is a column nobody reads. Blank at the default, so only the
/// rows someone reniced - or the kernel has queued for the OOM killer -
/// carry a number.
#[test]
fn nice_and_oom_are_blank_at_their_defaults() {
    let theme = Theme::default();
    let wide = ProcColumns::fit(140);
    let header = proc_columns_header(masys_view::ProcSort::Name, true, &wide);
    let ni = header.find("NI").expect("a nice heading");
    let oom = header.find("OOM").expect("an oom heading");
    let ordinary = text(&proc_lines(&[row(a_proc(1, "sshd", 8), None, 0)], &theme, &wide)[0]);
    assert!(ordinary[ni..oom + 3].trim().is_empty(), "no zeroes in NI or OOM on an ordinary process: {ordinary:?}");

    let mut hungry = a_proc(2, "chrome", 2048);
    hungry.nice = 19;
    hungry.oom_score = 667;
    let flagged = text(&proc_lines(&[row(hungry, None, 0)], &theme, &wide)[0]);
    assert!(flagged.contains("19"), "the nice value: {flagged:?}");
    assert!(flagged.contains("667"), "the oom score: {flagged:?}");
}

/// Group rows and process rows stack directly on top of each other at
/// every width, not only the narrowest one.
#[test]
fn group_and_process_rows_share_their_columns_when_stretched() {
    let theme = Theme::default();
    for width in [65, 80, 100, 140, 200] {
        let columns = ProcColumns::fit(width);
        let group = text(&proc_group_line(&a_group("/system.slice/sshd.service", 44.1, 1433, 4_200_000.0, 38, true), &theme, &columns));
        let proc = text(&proc_lines(&[row(a_proc(1204, "sshd", 649), Some(a_rate(1204, 2.3)), 0)], &theme, &columns)[0]);
        assert_eq!(
            group.find('%').expect("a group percent"),
            proc.find('%').expect("a process percent"),
            "at {width}: {group:?} vs {proc:?}"
        );
        // The process count sits in the THR column, which is the last
        // thing a group row draws - so a group row is exactly the S and
        // TIME columns shorter than the process rows beneath it.
        assert_eq!(group.width() + 3 + 10, proc.width(), "at {width}: {group:?} vs {proc:?}");
    }
}

/// A cgroup has no nice value, and the oldest member's age is not the
/// group's age. Blank rather than an invented aggregate.
#[test]
fn a_group_row_leaves_the_per_process_columns_blank() {
    let theme = Theme::default();
    let mut proc = a_proc(1204, "sshd", 649);
    proc.nice = 19;
    let columns = ProcColumns::fit(140);
    let group = text(&proc_group_line(&a_group("/system.slice/sshd.service", 44.1, 1433, 4_200_000.0, 38, true), &theme, &columns));
    let ni_at = proc_columns_header(masys_view::ProcSort::Cpu, true, &columns).find("NI").expect("a nice heading");
    assert!(group[ni_at..].trim_start().starts_with("38"), "nothing between IO and the process count: {group:?}");
}

fn a_full_detail() -> ProcDetail {
    ProcDetail {
        cmdline: Some("/nix/store/abc-postgresql/bin/postgres -D /var/lib/postgresql/16".into()),
        exe: Some("/nix/store/abc-postgresql/bin/postgres".into()),
        cwd: Some("/var/lib/postgresql/16".into()),
        ppid: Some(1),
        virt_bytes: Some(4_300_000_000),
        swap_bytes: Some(2_000_000),
        env: vec![("PGDATA".into(), "/var/lib/postgresql/16".into()), ("LS_COLORS".into(), "rs=0:di=01;34".into())],
        fds: vec![
            Fd { number: 0, target: FdTarget::Path("/dev/null".into()) },
            Fd { number: 1, target: FdTarget::Pipe },
            Fd { number: 2, target: FdTarget::Pipe },
            Fd { number: 7, target: FdTarget::Tcp { local: "0.0.0.0:5432".into(), peer: None, state: "LISTEN".into() } },
            Fd { number: 8, target: FdTarget::Unix { path: Some("/run/postgresql/.s.PGSQL.5432".into()), listening: true } },
            // A client of that same socket names the same path and is
            // not a listener - the distinction only `SO_ACCEPTCON` makes.
            Fd { number: 10, target: FdTarget::Unix { path: Some("/run/postgresql/.s.PGSQL.5432".into()), listening: false } },
            Fd {
                number: 9,
                target: FdTarget::Tcp { local: "127.0.0.1:5432".into(), peer: Some("127.0.0.1:40000".into()), state: "ESTABLISHED".into() },
            },
        ],
    }
}

fn proc_detail_text(proc: &Proc, detail: Option<&ProcDetail>) -> String {
    proc_detail_lines(proc, detail, &Theme::default()).iter().map(text).collect::<Vec<_>>().join("\n")
}

/// `Proc::comm` is capped at 15 bytes by the kernel, so the table cannot
/// tell one `python3` from another. This is the line that can.
#[test]
fn the_detail_block_leads_with_the_command_line() {
    let rendered = proc_detail_text(&a_proc(9932, "postgres", 1402), Some(&a_full_detail()));
    let first = rendered.lines().next().expect("a line");
    assert!(first.contains("cmdline"), "{first:?}");
    assert!(first.contains("-D /var/lib/postgresql/16"), "{first:?}");
}

/// A socket descriptor reads as `socket:[38271]` in /proc, which names a
/// number. The whole point of the fd table is that it names the service.
#[test]
fn a_listening_socket_reads_as_an_address_not_an_inode() {
    let rendered = proc_detail_text(&a_proc(9932, "postgres", 1402), Some(&a_full_detail()));
    assert!(rendered.contains("tcp 0.0.0.0:5432 LISTEN"), "{rendered}");
    assert!(rendered.contains("unix /run/postgresql/.s.PGSQL.5432"), "{rendered}");
    // The established connection is a socket, not a listener, so it is
    // counted but not pulled to the front.
    assert!(!rendered.contains("127.0.0.1:40000"), "only listeners are named: {rendered}");
}

/// The three the shell hands every process, by the names people use for
/// them - a daemon with stdout on a pipe and one with stdout on the
/// journal behave differently when the far end goes away.
#[test]
fn the_standard_streams_are_named_rather_than_numbered() {
    let rendered = proc_detail_text(&a_proc(9932, "postgres", 1402), Some(&a_full_detail()));
    assert!(rendered.contains("stdin"), "{rendered}");
    assert!(rendered.contains("stdout"), "{rendered}");
    assert!(rendered.contains("stderr"), "{rendered}");
}

/// "Four hundred open files" and "four hundred open sockets" are
/// different problems with different fixes.
#[test]
fn descriptors_are_counted_by_kind() {
    let rendered = proc_detail_text(&a_proc(9932, "postgres", 1402), Some(&a_full_detail()));
    let fds = rendered.lines().find(|l| l.contains("fds")).expect("an fds line");
    assert!(fds.contains("7 open"), "{fds:?}");
    assert!(fds.contains("4 sockets"), "{fds:?}");
    assert!(fds.contains("2 pipes"), "{fds:?}");
    assert!(fds.contains("1 files"), "{fds:?}");
}

/// The user's call, made explicitly: these are shown as read. The kernel
/// already gates `/proc/[pid]/environ` on same-uid or CAP_SYS_PTRACE, so
/// this shows an operator only what `cat` would.
#[test]
fn environment_values_are_shown_verbatim() {
    let rendered = proc_detail_text(&a_proc(9932, "postgres", 1402), Some(&a_full_detail()));
    assert!(rendered.contains("PGDATA=/var/lib/postgresql/16"), "{rendered}");
    // `LS_COLORS=rs=0:di=01;34` is a real variable, and splitting on
    // every `=` would truncate it at the first.
    assert!(rendered.contains("LS_COLORS=rs=0:di=01;34"), "{rendered}");
}

/// Most of what the detail reads is unreadable for another user's
/// process. The block still draws what `Proc` alone carries, because a
/// process is worth looking at when only half of it is readable.
#[test]
fn a_process_with_no_readable_detail_still_renders_what_proc_carries() {
    let mut proc = a_proc(9932, "postgres", 1402);
    proc.cgroup = Some("/system.slice/postgresql.service".into());
    proc.threads = 9;
    let rendered = proc_detail_text(&proc, None);
    assert!(rendered.contains("/system.slice/postgresql.service"), "{rendered}");
    assert!(rendered.contains("1.4G rss"), "{rendered}");
    assert!(rendered.contains("9 threads"), "{rendered}");
    // Nothing is invented for the fields that could not be read.
    assert!(!rendered.contains("cmdline"), "{rendered}");
    assert!(!rendered.contains("fds"), "{rendered}");
}

/// Reserved and resident are different facts, and a process being
/// squeezed shows only in swap.
#[test]
fn memory_separates_resident_from_mapped_from_swapped() {
    let rendered = proc_detail_text(&a_proc(9932, "postgres", 1402), Some(&a_full_detail()));
    let memory = rendered.lines().find(|l| l.contains("memory")).expect("a memory line");
    assert!(memory.contains("1.4G rss"), "{memory:?}");
    assert!(memory.contains("4.0G virt"), "{memory:?}");
    assert!(memory.contains("1.9M swap"), "{memory:?}");
}

/// Zero swap is the overwhelming default and says nothing, so it earns no
/// column - the same rule the table's NI and OOM follow.
#[test]
fn zero_swap_earns_no_mention() {
    let detail = ProcDetail { swap_bytes: Some(0), ..a_full_detail() };
    let rendered = proc_detail_text(&a_proc(9932, "postgres", 1402), Some(&detail));
    assert!(!rendered.contains("swap"), "{rendered}");
}

/// A block is drawn one item per line, so any length renders - but the
/// cursor cannot rest inside one, and moving it past jumps to the next
/// real row. Lines below the fold can be seen and never scrolled to, so a
/// block that claimed to list four hundred variables would be listing
/// most of them nowhere.
#[test]
fn a_very_long_block_is_capped_and_says_how_much_it_dropped() {
    let detail = ProcDetail { env: (0..400).map(|n| (format!("V{n}"), "x".into())).collect(), ..a_full_detail() };
    let rendered = proc_detail_text(&a_proc(9932, "postgres", 1402), Some(&detail));
    let lines: Vec<&str> = rendered.lines().collect();
    assert!(lines.len() <= 24, "capped: {} lines", lines.len());
    assert!(lines.last().unwrap().contains("more lines not shown"), "{:?}", lines.last());
}

/// The cut lands on the tail, which is why the block is ordered with the
/// identifying facts first: the command line survives a long environment.
#[test]
fn the_cap_takes_the_environment_before_the_command_line() {
    let detail = ProcDetail { env: (0..400).map(|n| (format!("V{n}"), "x".into())).collect(), ..a_full_detail() };
    let rendered = proc_detail_text(&a_proc(9932, "postgres", 1402), Some(&detail));
    assert!(rendered.contains("cmdline"), "{rendered}");
    assert!(rendered.contains("tcp 0.0.0.0:5432 LISTEN"), "the descriptors survive too: {rendered}");
}

/// A block that fits says nothing about trimming, because nothing was.
#[test]
fn a_block_that_fits_carries_no_truncation_notice() {
    let rendered = proc_detail_text(&a_proc(9932, "postgres", 1402), Some(&a_full_detail()));
    assert!(!rendered.contains("more lines"), "{rendered}");
}

/// The design's gauge widget. Filled cells round *down* so a filesystem
/// at 99.6% still shows an empty cell: "nearly full" and "full" are
/// different operational states, and a bar that saturates early stops
/// telling them apart exactly when it matters.
#[test]
fn the_gauge_rounds_down_and_never_overflows() {
    assert_eq!(gauge(0.0, 10), "[----------]");
    assert_eq!(gauge(50.0, 10), "[#####-----]");
    assert_eq!(gauge(99.6, 10), "[#########-]", "nearly full is not full");
    assert_eq!(gauge(100.0, 10), "[##########]");
    // Out-of-range input clamps rather than panicking on a repeat count.
    assert_eq!(gauge(140.0, 10), "[##########]");
    assert_eq!(gauge(-5.0, 10), "[----------]");
}

#[test]
fn a_failed_unit_row_shows_its_exit_code() {
    let theme = Theme::default();
    let mut unit = a_unit("restic-backup.service", ActiveState::Failed);
    unit.exit_code = Some(1);
    let rendered = text(&unit_lines(&[flat(unit, Some(10_800_000), false)], &theme)[0]);
    assert!(rendered.starts_with("x "), "a failed unit is the dead tier: {rendered:?}");
    assert!(rendered.contains("failed (exit 1)"), "{rendered:?}");
    assert!(rendered.contains("3h"), "the age: {rendered:?}");
}

/// Most units on a normal host are static or generated. A column of
/// `disabled` down the screen says nothing about any of them.
#[test]
fn enabled_earns_a_column_only_when_true() {
    let theme = Theme::default();
    let mut off = a_unit("a.service", ActiveState::Active);
    off.enabled = false;
    assert!(!text(&unit_lines(&[flat(off, Some(0), false)], &theme)[0]).contains("enabled"));
    assert!(text(&unit_lines(&[flat(a_unit("a.service", ActiveState::Active), Some(0), false)], &theme)[0]).contains("enabled"));
}

/// The marker is the affordance that says the row opens onto what is
/// using the space. A read-only filesystem keeps its urgent glyph, which
/// outranks it.
#[test]
fn an_open_filesystem_row_draws_the_fold_marker() {
    let theme = Theme::default();
    let shut = text(&filesystem_lines(&[(a_fs("/", 81.0, false), false)], &theme)[0]);
    assert!(shut.starts_with("> "), "{shut:?}");
    let open = text(&filesystem_lines(&[(a_fs("/", 81.0, false), true)], &theme)[0]);
    assert!(open.starts_with("v "), "{open:?}");
    let broken = text(&filesystem_lines(&[(a_fs("/", 81.0, true), true)], &theme)[0]);
    assert!(broken.starts_with("! "), "read-only outranks the marker: {broken:?}");
}

#[test]
fn a_read_only_filesystem_is_flagged_urgent() {
    let theme = Theme::default();
    let rendered = text(&filesystem_lines(&[(a_fs("/nix", 91.0, true), false)], &theme)[0]);
    assert!(rendered.starts_with("! "), "the urgent tier: {rendered:?}");
    assert!(rendered.contains("read-only"), "{rendered:?}");
    assert!(rendered.contains("91%"), "{rendered:?}");
    assert!(rendered.contains('#'), "the gauge is drawn: {rendered:?}");
}

/// An entry with no `_SYSTEMD_UNIT` came from the kernel. Naming it beats
/// a gap - "no unit" and "the kernel" are the same fact here.
#[test]
fn a_journal_entry_without_a_unit_says_kernel() {
    let theme = Theme::default();
    let entry = Entry { timestamp_ms: 0, unit: None, priority: Priority::Warning, message: "i915 GPU hang".to_string() };
    let rendered = text(&journal_lines(&[entry], &theme, None)[0]);
    assert!(rendered.starts_with("~ "), "a warning is the middle tier: {rendered:?}");
    assert!(rendered.contains("kernel"), "{rendered:?}");
    assert!(rendered.contains("i915 GPU hang"), "{rendered:?}");
}

/// Journal messages are arbitrary and carry newlines; a row is a row.
#[test]
fn a_multiline_journal_message_stays_one_row() {
    let theme = Theme::default();
    let entry =
        Entry { timestamp_ms: 0, unit: Some("sshd.service".to_string()), priority: Priority::Error, message: "first\nsecond".to_string() };
    let rendered = text(&journal_lines(&[entry], &theme, None)[0]);
    assert!(!rendered.contains('\n'), "{rendered:?}");
    assert!(rendered.contains("first second"), "{rendered:?}");
}

/// Every row in the log view belongs to the unit the header names, so
/// repeating it 2,000 times says nothing.
#[test]
fn a_row_whose_unit_is_the_views_own_does_not_repeat_it() {
    let theme = Theme::default();
    let entry = Entry {
        timestamp_ms: 0,
        unit: Some("sshd.service".to_string()),
        priority: Priority::Info,
        message: "Server listening on 0.0.0.0 port 22.".to_string(),
    };
    let rendered = text(&journal_lines(&[entry], &theme, Some("sshd.service"))[0]);
    assert!(!rendered.contains("sshd.service"), "the scope is in the header: {rendered:?}");
    assert!(rendered.contains("Server listening"), "{rendered:?}");
}

/// `journalctl -u sshd.service` does not return only sshd's own output:
/// pid 1's messages *about* the unit come back stamped `init.scope`. That
/// distinction is worth a column when it applies.
#[test]
fn a_row_from_another_unit_names_itself() {
    let theme = Theme::default();
    let entry = Entry {
        timestamp_ms: 0,
        unit: Some("init.scope".to_string()),
        priority: Priority::Info,
        message: "Starting SSH Daemon...".to_string(),
    };
    let rendered = text(&journal_lines(&[entry], &theme, Some("sshd.service"))[0]);
    assert!(rendered.contains("init.scope"), "systemd said this, not sshd: {rendered:?}");
}

/// A daemon that stamps its own lines - syncthing, and 12% of the lines
/// on this host - puts a date and time in the message, beside the column
/// masys already draws one in. The row read `21:17:57  syncthing.service
/// 2026-05-19 21:17:57 INF ...`: the same second, twice, and a date the
/// column deliberately leaves out.
#[test]
fn a_senders_own_timestamp_is_dropped_from_the_message() {
    let theme = Theme::default();
    let entry = Entry {
        timestamp_ms: 0,
        unit: Some("syncthing.service".to_string()),
        priority: Priority::Info,
        message: "2026-05-19 21:17:57 INF Completed initial scan".to_string(),
    };
    let rendered = text(&journal_lines(&[entry], &theme, None)[0]);
    assert!(rendered.contains("INF Completed initial scan"), "{rendered:?}");
    assert!(!rendered.contains("2026-05-19"), "the date is gone: {rendered:?}");
    assert!(!rendered.contains("21:17:57 INF"), "and so is the duplicated time: {rendered:?}");
}

/// The other shape that actually occurs here: RFC 3339, with fractional
/// seconds and a zone offset.
#[test]
fn an_rfc3339_sender_timestamp_is_dropped_too() {
    let theme = Theme::default();
    let entry = Entry {
        timestamp_ms: 0,
        unit: Some("docker.service".to_string()),
        priority: Priority::Warning,
        message: "2026-05-19T21:17:57.123456-0400 level=warn msg=\"slow\"".to_string(),
    };
    let rendered = text(&journal_lines(&[entry], &theme, None)[0]);
    assert!(rendered.contains("level=warn"), "{rendered:?}");
    assert!(!rendered.contains("2026-05-19"), "{rendered:?}");
}

/// The otel collector - 1,508 lines on this host - separates its fields
/// with tabs, so its stamp is followed by a tab rather than a space.
#[test]
fn a_sender_timestamp_followed_by_a_tab_is_dropped() {
    let theme = Theme::default();
    let entry = Entry {
        timestamp_ms: 0,
        unit: Some("otel-collector-host-logs.service".to_string()),
        priority: Priority::Info,
        message: "2026-05-19T08:15:48.450-0600\tinfo\tadapter/receiver.go:41\tStarting stanza".to_string(),
    };
    let rendered = text(&journal_lines(&[entry], &theme, None)[0]);
    assert!(!rendered.contains("2026-05-19"), "{rendered:?}");
    assert!(rendered.contains("info"), "{rendered:?}");
}

/// A tab is drawn as nothing at all, so a tab-separated line arrived with
/// its fields glued together - `...-0600infoadapter/receiver.go:41`. A row
/// is a row, and the same rule newlines get applies here.
#[test]
fn tabs_in_a_message_become_spaces_rather_than_vanishing() {
    let theme = Theme::default();
    let entry = Entry {
        timestamp_ms: 0,
        unit: Some("otel-collector-host-logs.service".to_string()),
        priority: Priority::Info,
        message: "info\tadapter/receiver.go:41\tStarting stanza".to_string(),
    };
    let rendered = text(&journal_lines(&[entry], &theme, None)[0]);
    assert!(rendered.contains("info adapter/receiver.go:41 Starting stanza"), "{rendered:?}");
}

/// Only a *leading* stamp is the sender's own. A date inside a sentence is
/// the message, and cutting it would be editing what a unit said.
#[test]
fn a_date_in_the_middle_of_a_message_survives() {
    let theme = Theme::default();
    let entry = Entry {
        timestamp_ms: 0,
        unit: Some("restic.service".to_string()),
        priority: Priority::Info,
        message: "snapshot for 2026-05-19 21:17:57 completed".to_string(),
    };
    let rendered = text(&journal_lines(&[entry], &theme, None)[0]);
    assert!(rendered.contains("snapshot for 2026-05-19 21:17:57 completed"), "{rendered:?}");
}

/// A line that is *only* a timestamp leaves nothing behind, and a blank
/// row says less than a redundant one.
#[test]
fn a_message_that_is_nothing_but_a_timestamp_is_left_alone() {
    let theme = Theme::default();
    let entry = Entry {
        timestamp_ms: 0,
        unit: Some("odd.service".to_string()),
        priority: Priority::Info,
        message: "2026-05-19 21:17:57".to_string(),
    };
    let rendered = text(&journal_lines(&[entry], &theme, None)[0]);
    assert!(rendered.contains("2026-05-19 21:17:57"), "{rendered:?}");
}

fn an_interface(name: &str, tx_drop: u64) -> masys_domain::sample::Interface {
    masys_domain::sample::Interface {
        name: name.to_string(),
        rx_bytes: 1,
        tx_bytes: 1,
        rx_packets: 0,
        tx_packets: 0,
        rx_errs: 0,
        tx_errs: 0,
        rx_drop: 0,
        tx_drop,
        up: true,
        loopback: false,
    }
}

#[test]
fn an_interface_row_shows_throughput_both_ways() {
    let theme = Theme::default();
    let rate = masys_domain::rate::NetRate {
        rx_bytes_per_sec: 1_258_291.0,
        tx_bytes_per_sec: 348_160.0,
        rx_packets_per_sec: 900.0,
        tx_packets_per_sec: 400.0,
    };
    let rendered = text(&interface_line(&an_interface("enp0s31f6", 0), Some(rate), &theme, 12));
    assert!(rendered.contains("enp0s31f6"), "{rendered:?}");
    assert!(rendered.contains("1.2M/s"), "down: {rendered:?}");
    assert!(rendered.contains("340K/s"), "up: {rendered:?}");
}

/// Drops are cumulative since boot, so a zero is the normal case and
/// printing `0 drop` on every row would train the eye to skip the column
/// that matters.
#[test]
fn drops_appear_only_when_there_are_some() {
    let theme = Theme::default();
    let clean = text(&interface_line(&an_interface("enp0s31f6", 0), None, &theme, 12));
    assert!(!clean.contains("drop"), "{clean:?}");

    let dropping = text(&interface_line(&an_interface("enp0s31f6", 37), None, &theme, 12));
    assert!(dropping.contains("37 drop"), "{dropping:?}");
}

/// `-` rather than `0 B/s`: not yet measured and genuinely idle are
/// different facts, and the whole rate module exists to keep them apart.
#[test]
fn an_interface_with_no_rate_yet_draws_a_dash() {
    let theme = Theme::default();
    let rendered = text(&interface_line(&an_interface("wg0", 0), None, &theme, 12));
    assert!(rendered.contains('-'), "{rendered:?}");
    assert!(!rendered.contains("0B/s"), "a measured zero would say that: {rendered:?}");
}

fn a_unit(name: &str, state: ActiveState) -> Unit {
    Unit {
        name: name.to_string(),
        kind: UnitKind::Service,
        active_state: state,
        sub_state: "running".to_string(),
        exit_code: None,
        enabled: true,
        restart_timestamps_ms: Vec::new(),
        since_ms: 0,
        cgroup: None,
        slice: None,
        triggers: Vec::new(),
        timer: None,
    }
}

fn a_fs(mount: &str, used: f32, read_only: bool) -> Filesystem {
    Filesystem { mount_point: mount.to_string(), used_percent: used, free_bytes: 12 * (1u64 << 30), inode_used_percent: 6.0, read_only }
}

/// systemd escapes device and mount paths into unit names, so they are
/// unbounded - this host has several over 130 characters. Padding a 177-unit
/// section to its longest name pushes the state column off screen for
/// every other row.
#[test]
fn a_very_long_unit_name_does_not_push_the_columns_off_screen() {
    let theme = Theme::default();
    let long = a_unit("dev-disk-by\\x2duuid-1b899112\\x2d0386\\x2d42b2\\x2d8784\\x2d642ad5a73356.swap", ActiveState::Active);
    let short = a_unit("a.service", ActiveState::Active);
    let lines = unit_lines(&[flat(long, Some(0), false), flat(short, Some(0), false)], &theme);
    for line in &lines {
        assert!(UnicodeWidthStr::width(text(line).as_str()) <= 80, "{:?}", text(line));
    }
    // Both rows still agree on where the state column starts.
    let state_at = |l: &Line| text(l).find("running").expect("a state");
    assert_eq!(state_at(&lines[0]), state_at(&lines[1]));
}

/// systemd reports an empty `StateChangeTimestamp` for units that were
/// already in place before it started - `-.mount` is mounted by the
/// initrd. That arrives as 0, and dating it from the Unix epoch rendered
/// as `20684d 14h` on this host.
#[test]
fn a_unit_with_no_recorded_transition_shows_no_age() {
    let theme = Theme::default();
    let unknown = text(&unit_lines(&[flat(a_unit("-.mount", ActiveState::Active), None, false)], &theme)[0]);
    let known = text(&unit_lines(&[flat(a_unit("-.mount", ActiveState::Active), Some(10_800_000), false)], &theme)[0]);

    assert!(unknown.contains("-.mount"), "{unknown:?}");
    assert!(unknown.trim_end().ends_with("enabled"), "the age column is simply empty: {unknown:?}");
    assert!(known.trim_end().ends_with("3h"), "a known age still renders: {known:?}");
}

/// "exit 6" against "curl: (6) Could not resolve host" is the difference
/// between knowing something broke and knowing what to do about it.
#[test]
fn a_failed_unit_shows_why_when_the_reason_is_known() {
    let theme = Theme::default();
    let finding = Finding::FailedUnit {
        unit: "nightly-backup.service".to_string(),
        exit_code: Some(6),
        since_ms: 10_800_000,
        reason: Some("curl: (6) Could not resolve host: storage.googleapis.com".to_string()),
    };
    let rendered = text(&finding_line(&finding, &theme));
    assert!(rendered.contains("failed (exit 6)"), "{rendered}");
    assert!(rendered.contains("3h ago"), "{rendered}");
    assert!(rendered.ends_with("Could not resolve host: storage.googleapis.com"), "{rendered}");
}

/// A unit that said nothing of its own, or whose journal is gone, reads
/// exactly as it did before - no empty column, no placeholder.
#[test]
fn a_failed_unit_with_no_known_reason_is_unchanged() {
    let theme = Theme::default();
    let finding = Finding::FailedUnit { unit: "a.service".to_string(), exit_code: Some(1), since_ms: 10_800_000, reason: None };
    assert_eq!(text(&finding_line(&finding, &theme)), "x a.service  failed (exit 1)  3h ago");
}

/// An opened unit shows what `systemctl status` leads with - description,
/// where it was loaded from, what it is running - in this tool's
/// label-column idiom. No log: `l` opens that in full.
#[test]
fn an_opened_unit_shows_status_facts() {
    let unit = masys_domain::unit::Unit {
        name: "sshd.service".to_string(),
        kind: masys_domain::unit::UnitKind::Service,
        active_state: masys_domain::unit::ActiveState::Active,
        sub_state: "running".to_string(),
        exit_code: None,
        enabled: true,
        restart_timestamps_ms: Vec::new(),
        since_ms: 1_779_224_000_000,
        cgroup: None,
        slice: None,
        triggers: Vec::new(),
        timer: None,
    };
    let detail = masys_domain::unit::UnitDetail {
        description: Some("SSH Daemon".to_string()),
        fragment_path: Some("/etc/systemd/system/sshd.service".to_string()),
        main_pid: Some(897),
        tasks: Some(1),
        memory_bytes: Some(4_775_936),
        cpu_nsec: Some(96_103_000),
        cgroup: Some("/system.slice/sshd.service".to_string()),
        result: Some("success".to_string()),
        memory_peak_bytes: Some(9_175_040),
        tasks_max: Some(38_324),
        io_read_bytes: Some(3_571_712),
        io_write_bytes: Some(0),
        ip_ingress_bytes: Some(5_806),
        ip_egress_bytes: Some(5_806),
        documentation: vec!["man:sshd(8)".to_string()],
    };
    let block = detail_text(&unit, Some(&detail));

    assert!(block.contains("SSH Daemon"), "{block}");
    assert!(block.contains("/etc/systemd/system/sshd.service"), "{block}");
    assert!(block.contains("pid 897"), "{block}");
    assert!(block.contains("1 task"), "and singular, not `1 tasks`: {block}");
    assert!(block.contains("4.6M"), "memory is scaled, not raw bytes: {block}");
    assert!(block.contains("cpu 96ms"), "nanoseconds are scaled the way systemctl scales them: {block}");
    assert!(block.contains("/system.slice/sshd.service"), "{block}");
}

/// A unit type that has none of these must omit them rather than print a
/// column of blanks - a `.target` has no pid, a `.device` no memory.
#[test]
fn a_unit_without_process_facts_omits_them() {
    let unit = masys_domain::unit::Unit {
        name: "basic.target".to_string(),
        kind: masys_domain::unit::UnitKind::Target,
        active_state: masys_domain::unit::ActiveState::Active,
        sub_state: "active".to_string(),
        exit_code: None,
        enabled: false,
        restart_timestamps_ms: Vec::new(),
        since_ms: 0,
        cgroup: None,
        slice: None,
        triggers: Vec::new(),
        timer: None,
    };
    let detail = masys_domain::unit::UnitDetail { description: Some("Basic System".to_string()), ..Default::default() };
    let block = detail_text(&unit, Some(&detail));

    assert!(block.contains("Basic System"), "{block}");
    assert!(!block.contains("running"), "no process facts line at all: {block}");
    assert!(block.contains("no unit file"), "and it says so rather than leaving the line blank: {block}");
}

/// An open unit shows a `v` where a closed one shows nothing, so the fold
/// reads without the row shifting.
#[test]
fn an_open_unit_is_marked_in_its_own_row() {
    let unit = masys_domain::unit::Unit {
        name: "sshd.service".to_string(),
        kind: masys_domain::unit::UnitKind::Service,
        active_state: masys_domain::unit::ActiveState::Active,
        sub_state: "running".to_string(),
        exit_code: None,
        enabled: true,
        restart_timestamps_ms: Vec::new(),
        since_ms: 1_000,
        cgroup: None,
        slice: None,
        triggers: Vec::new(),
        timer: None,
    };
    let closed = text(&unit_lines(&[flat(unit.clone(), Some(10), false)], &Theme::default())[0]);
    let open = text(&unit_lines(&[flat(unit, Some(10), true)], &Theme::default())[0]);
    assert!(open.starts_with(". v "), "an open unit is marked: {open:?}");
    assert!(closed.starts_with(".   "), "a closed one is not: {closed:?}");
    assert_eq!(open.len(), closed.len(), "and the row does not shift");
}

fn detail_text(unit: &masys_domain::unit::Unit, detail: Option<&masys_domain::unit::UnitDetail>) -> String {
    masys_render::view::unit_detail_lines(unit, detail, &Theme::default())
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.clone()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A unit row is coloured by its state, not just its glyph.
///
/// On 366 rows the glyph alone is a two-column signal in a wall of text.
/// Colouring the row is what lets a failure be found without reading:
/// failed rows carry the dead tier, anything mid-transition the warning
/// tier, and inactive ones recede so the eye passes over them.
#[test]
fn a_unit_row_is_coloured_by_its_state() {
    let theme = Theme::default();
    let colour = |state| {
        let unit = a_unit("x.service", state);
        let line = &unit_lines(&[flat(unit, Some(10), false)], &theme)[0];
        // The name span, not the glyph - the glyph was always coloured.
        line.spans.last().expect("a name span").style.fg
    };

    assert_eq!(colour(ActiveState::Failed), Some(theme.severity_dead), "a failure is findable without reading");
    assert_eq!(colour(ActiveState::Activating), Some(theme.severity_warning), "mid-transition is not settled");
    assert_eq!(colour(ActiveState::Reloading), Some(theme.severity_warning));
    assert_eq!(colour(ActiveState::Inactive), Some(theme.info), "inactive recedes");
    assert_eq!(colour(ActiveState::Active), None, "and a healthy unit is plain text, so the others stand out");
}
