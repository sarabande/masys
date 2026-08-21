use std::collections::HashSet;

use masys_app::io_buffer::build_io_rows;
use masys_app::log_buffer::{TAIL, build_log_rows};
use masys_app::systemd_buffer::build_unit_rows;
use masys_domain::journal::{Entry, Priority};
use masys_domain::sample::Filesystem;
use masys_domain::unit::{ActiveState, Unit, UnitKind};
use masys_view::Node;

fn unit(name: &str, state: ActiveState) -> Unit {
    kinded(name, state, UnitKind::Service)
}

fn kinded(name: &str, state: ActiveState, kind: UnitKind) -> Unit {
    Unit {
        name: name.to_string(),
        kind,
        active_state: state,
        sub_state: "running".to_string(),
        exit_code: None,
        enabled: true,
        restart_timestamps_ms: Vec::new(),
        since_ms: 1_000,
        cgroup: None,
        slice: None,
        triggers: Vec::new(),
        timer: None,
    }
}

fn fs(mount: &str, used: f32, read_only: bool) -> Filesystem {
    Filesystem { mount_point: mount.to_string(), used_percent: used, free_bytes: 1 << 30, inode_used_percent: 5.0, read_only }
}

fn entry(ms: u64, message: &str) -> Entry {
    Entry { timestamp_ms: ms, unit: None, priority: Priority::Info, message: message.to_string() }
}

fn sections(rows: &[Node]) -> Vec<(String, u32)> {
    rows.iter()
        .filter_map(|r| match r {
            Node::SectionHeader { title, count, .. } => Some((title.clone(), count.unwrap_or(0))),
            _ => None,
        })
        .collect()
}

/// A section per unit type, in `UnitKind::ORDER` - services first because
/// they are what starts, stops and fails; devices last because there are
/// more of them than anything else and almost nothing can be done to one.
#[test]
fn units_group_by_type_in_a_fixed_order() {
    let units = vec![
        kinded("sys-devices-x.device", ActiveState::Active, UnitKind::Device),
        unit("a.service", ActiveState::Active),
        kinded("sshd.socket", ActiveState::Active, UnitKind::Socket),
        kinded("basic.target", ActiveState::Active, UnitKind::Target),
    ];
    let rows = build_unit_rows(&units, &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000);
    assert_eq!(
        sections(&rows),
        vec![("Services".to_string(), 1), ("Sockets".to_string(), 1), ("Targets".to_string(), 1), ("Devices".to_string(), 1)]
    );
}

/// Failure stopped being a section; it did not stop mattering. A broken
/// unit is the first row under its own type rather than somewhere in an
/// alphabetical list of 138.
#[test]
fn a_failed_unit_sorts_to_the_top_of_its_type() {
    let units =
        vec![unit("a.service", ActiveState::Active), unit("z.service", ActiveState::Failed), unit("b.service", ActiveState::Activating)];
    let names: Vec<String> = build_unit_rows(&units, &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000)
        .iter()
        .filter_map(|r| match r {
            Node::Unit { unit, .. } => Some(unit.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["z.service", "b.service", "a.service"], "failed, then activating, then active");
}

/// A heading that is usually empty stops being read, so a type with no
/// units on this host - swap, scope - never appears at all.
#[test]
fn a_type_with_no_units_gets_no_heading() {
    let rows = build_unit_rows(&[unit("a.service", ActiveState::Active)], &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000);
    assert_eq!(sections(&rows), vec![("Services".to_string(), 1)]);
}

#[test]
fn units_sort_by_name_within_a_type() {
    let units = vec![unit("z.service", ActiveState::Active), unit("a.service", ActiveState::Active)];
    let names: Vec<String> = build_unit_rows(&units, &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000)
        .iter()
        .filter_map(|r| match r {
            Node::Unit { unit, .. } => Some(unit.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["a.service", "z.service"]);
}

/// The renderer has no clock, so the age is computed here - the same
/// conversion triage does before a finding reaches the view.
#[test]
fn a_unit_row_carries_an_age_not_a_timestamp() {
    let rows = build_unit_rows(&[unit("a.service", ActiveState::Active)], &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000);
    let age = rows
        .iter()
        .find_map(|r| match r {
            Node::Unit { age_ms, .. } => Some(*age_ms),
            _ => None,
        })
        .expect("a unit row");
    assert_eq!(age, Some(4_000), "5000 now minus 1000 since");
}

#[test]
fn a_collapsed_type_hides_its_units_but_keeps_its_heading() {
    let units = vec![unit("a.service", ActiveState::Active)];
    let rows = build_unit_rows(&units, &HashSet::from(["Services".to_string()]), &HashSet::new(), &HashSet::new(), 5_000);
    assert_eq!(sections(&rows), vec![("Services".to_string(), 1)], "the count still tells you what is hidden");
    assert!(!rows.iter().any(|r| matches!(r, Node::Unit { .. })));
}

/// A read-only filesystem is the urgent case this buffer exists for, and
/// the kernel can remount at any usage level - so it outranks percentage.
#[test]
fn io_puts_read_only_first_then_the_fullest() {
    let rows = build_io_rows(
        &[fs("/home", 12.0, false), fs("/boot", 96.0, false), fs("/data", 3.0, true)],
        &[],
        &[],
        &[],
        &[],
        &Default::default(),
        &Default::default(),
    );
    let mounts: Vec<String> = rows
        .iter()
        .filter_map(|r| match r {
            Node::Filesystem { filesystem, .. } => Some(filesystem.mount_point.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(mounts, vec!["/data", "/boot", "/home"]);
    assert_eq!(sections(&rows), vec![("Filesystems".to_string(), 3)]);
}

/// The bar compares a directory with its largest sibling, not with the
/// filesystem's total.
///
/// Against the total, every row below the biggest collapses to an empty
/// bar: on this host `/home/user/tools` is 53G of a 622G tree, which is
/// 8.5% and rounds to no blocks at all. Ten rows of `[----------]` say
/// nothing. Against the largest sibling the column becomes a comparison,
/// which is the question being asked.
#[test]
fn a_directorys_bar_is_relative_to_its_largest_sibling() {
    let scan = masys_domain::scan::ScanProgress {
        root: Some(std::path::PathBuf::from("/data")),
        dirs: vec![
            masys_domain::scan::DirSize { path: std::path::PathBuf::from("/data"), bytes: 1_000 },
            masys_domain::scan::DirSize { path: std::path::PathBuf::from("/data/big"), bytes: 400 },
            masys_domain::scan::DirSize { path: std::path::PathBuf::from("/data/half"), bytes: 200 },
        ],
        done: true,
        ..Default::default()
    };
    let rows = build_io_rows(&[fs("/data", 50.0, false)], &[], &[], &[], &[], &scan, &Default::default());
    let shares: Vec<(String, f32)> = rows
        .iter()
        .filter_map(|r| match r {
            Node::DirEntry { path, share, .. } => Some((path.display().to_string(), *share)),
            _ => None,
        })
        .collect();
    assert_eq!(shares.len(), 2, "{shares:?}");
    assert!((shares[0].1 - 1.0).abs() < 0.01, "the largest fills its bar: {shares:?}");
    assert!((shares[1].1 - 0.5).abs() < 0.01, "half the largest, half the bar: {shares:?}");
}

fn iface(name: &str, rx: u64, tx: u64, up: bool, loopback: bool) -> masys_domain::sample::Interface {
    masys_domain::sample::Interface {
        name: name.to_string(),
        rx_bytes: rx,
        tx_bytes: tx,
        rx_packets: 0,
        tx_packets: 0,
        rx_errs: 0,
        tx_errs: 0,
        rx_drop: 0,
        tx_drop: 0,
        up,
        loopback,
    }
}

fn interface_names(rows: &[Node]) -> Vec<String> {
    rows.iter()
        .filter_map(|r| match r {
            Node::Interface { interface, .. } => Some(interface.name.clone()),
            _ => None,
        })
        .collect()
}

/// The development host has nine interfaces and two worth looking at.
/// Six idle bridges above the filesystems would bury the section that
/// matters.
#[test]
fn the_network_section_hides_what_cannot_be_carrying_traffic() {
    let interfaces = vec![
        iface("enp0s31f6", 6_736_398_191, 9_585_007_885, true, false),
        iface("tailscale0", 2_919_553, 6_600_744, true, false),
        // Loopback has moved 3.8 MB on this host, so "has it ever moved a
        // byte" does not hide it - it is hidden for being loopback.
        iface("lo", 3_794_998, 3_794_998, true, true),
        // Down, but *has* moved bytes: the case that makes `up` a field
        // rather than something inferred from the counters.
        iface("docker0", 2_117, 8_121, false, false),
        // Up and never used.
        iface("br-b78101c4a5f3", 0, 0, true, false),
    ];
    let rows = build_io_rows(&[], &[], &[], &interfaces, &[], &Default::default(), &Default::default());
    assert_eq!(interface_names(&rows), vec!["enp0s31f6".to_string(), "tailscale0".to_string()]);
}

/// Busiest first, the way Devices already orders by how busy they are -
/// and by name before any rate exists, so the list does not jump about on
/// the first tick.
#[test]
fn interfaces_are_ordered_by_throughput() {
    let interfaces = vec![iface("slow", 1, 1, true, false), iface("fast", 1, 1, true, false)];
    let rate = |rx: f64, tx: f64| {
        (
            String::new(),
            masys_domain::rate::NetRate { rx_bytes_per_sec: rx, tx_bytes_per_sec: tx, rx_packets_per_sec: 0.0, tx_packets_per_sec: 0.0 },
        )
    };
    let rates = vec![("slow".to_string(), rate(10.0, 0.0).1), ("fast".to_string(), rate(9_000.0, 0.0).1)];
    let rows = build_io_rows(&[], &[], &[], &interfaces, &rates, &Default::default(), &Default::default());
    assert_eq!(interface_names(&rows), vec!["fast".to_string(), "slow".to_string()]);
}

/// A host with nothing worth showing gets no heading for it - the same
/// rule the Devices and Filesystems sections already follow.
#[test]
fn no_visible_interface_means_no_network_section() {
    let rows = build_io_rows(&[], &[], &[], &[iface("lo", 5, 5, true, true)], &[], &Default::default(), &Default::default());
    assert!(rows.is_empty(), "{rows:#?}");
}

/// The rate rides on the row, so a row can say "not measured yet" rather
/// than "idle" - the distinction `crate::rate` exists to preserve.
#[test]
fn an_interface_without_a_rate_yet_carries_none() {
    let rows = build_io_rows(&[], &[], &[], &[iface("enp0s31f6", 10, 10, true, false)], &[], &Default::default(), &Default::default());
    let carried = rows.iter().find_map(|r| match r {
        Node::Interface { rate, .. } => Some(*rate),
        _ => None,
    });
    assert_eq!(carried, Some(None), "no previous sample, so no rate");
}

#[test]
fn nothing_to_report_produces_no_rows() {
    assert!(build_io_rows(&[], &[], &[], &[], &[], &Default::default(), &Default::default()).is_empty());
}

/// The other direction reads in the order things happened, which is how
/// journalctl prints and what you want once you are reading rather than
/// checking.
#[test]
fn journal_entries_can_be_read_oldest_first() {
    let rows =
        build_log_rows(&[entry(300, "third"), entry(100, "first"), entry(200, "second")], Some("a.service"), 0, false, &Default::default());
    let messages: Vec<String> = rows
        .iter()
        .filter_map(|r| match r {
            Node::JournalEntry(e) => Some(e.message.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(messages, vec!["first", "second", "third"]);
}

/// A busy host emits thousands of lines an hour; the tail is capped, and
/// what survives is the *newest*, not the first thousand read.
#[test]
fn the_journal_tail_is_capped_and_keeps_the_newest() {
    let entries: Vec<Entry> = (0..TAIL as u64 + 50).map(|n| entry(n, &format!("line {n}"))).collect();
    let rows = build_log_rows(&entries, Some("busy.service"), 0, false, &Default::default());
    let messages: Vec<String> = rows
        .iter()
        .filter_map(|r| match r {
            Node::JournalEntry(e) => Some(e.message.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), TAIL);
    assert_eq!(messages.last().unwrap(), &format!("line {}", TAIL + 49), "the newest survives");
    assert_eq!(messages.first().unwrap(), "line 50", "the oldest is dropped");
}

#[test]
fn an_empty_journal_produces_no_rows() {
    assert!(build_log_rows(&[], None, 0, true, &Default::default()).is_empty());
}

/// systemd reports an empty `StateChangeTimestamp` - 0 on the bus - for
/// units already in place before it started, such as the initrd-mounted
/// root. Treating that as a timestamp dates them to 1970.
#[test]
fn a_unit_with_no_recorded_transition_has_no_age() {
    let mut never = unit("-.mount", ActiveState::Active);
    never.since_ms = 0;
    let rows = build_unit_rows(&[never], &HashSet::new(), &HashSet::new(), &HashSet::new(), 1_779_224_000_000);
    let age = rows
        .iter()
        .find_map(|r| match r {
            Node::Unit { age_ms, .. } => Some(*age_ms),
            _ => None,
        })
        .expect("a unit row");
    assert_eq!(age, None, "not 20684 days since the epoch");
}

fn timer_unit(name: &str, activates: &str, next: Option<u64>, last: Option<u64>) -> Unit {
    // A `.timer` really is `UnitKind::Timer` - the old state grouping
    // never asked, so this helper never said so.
    let mut u = kinded(name, ActiveState::Active, UnitKind::Timer);
    u.timer = Some(masys_domain::unit::Timer { next_ms: next, last_ms: last, activates: activates.to_string() });
    u
}

fn timers(rows: &[Node]) -> Vec<(String, bool)> {
    rows.iter()
        .filter_map(|r| match r {
            Node::Timer { name, activated_failed, .. } => Some((name.clone(), *activated_failed)),
            _ => None,
        })
        .collect()
}

/// A timer stays `active` while the job it runs is broken, so its own
/// state says nothing about whether the schedule is working. Resolving
/// the activated unit's health is the column the section exists for.
#[test]
fn a_timer_is_marked_by_the_health_of_what_it_activates() {
    let units = vec![
        timer_unit("nightly.timer", "nightly.service", Some(9_000), Some(1_000)),
        unit("nightly.service", ActiveState::Failed),
        timer_unit("fine.timer", "fine.service", Some(8_000), Some(2_000)),
        unit("fine.service", ActiveState::Active),
    ];
    let rows = build_unit_rows(&units, &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000);
    assert_eq!(timers(&rows), vec![("fine.timer".to_string(), false), ("nightly.timer".to_string(), true)]);
}

/// Soonest first: the next thing to happen is the useful ordering, and a
/// timer that will not fire again sorts last because it will not happen.
#[test]
fn timers_are_ordered_by_when_they_next_fire() {
    let units = vec![
        timer_unit("later.timer", "later.service", Some(90_000), None),
        timer_unit("never.timer", "never.service", None, None),
        timer_unit("soon.timer", "soon.service", Some(6_000), None),
    ];
    let names: Vec<String> =
        timers(&build_unit_rows(&units, &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000)).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, vec!["soon.timer", "later.timer", "never.timer"]);
}

/// The renderer has no clock, so both figures arrive as durations - the
/// conversion Node::Unit and triage already do.
#[test]
fn a_timer_row_carries_durations_not_timestamps() {
    let units = vec![timer_unit("a.timer", "a.service", Some(9_000), Some(1_000))];
    let rows = build_unit_rows(&units, &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000);
    let (next, last) = rows
        .iter()
        .find_map(|r| match r {
            Node::Timer { next_in_ms, last_ago_ms, .. } => Some((*next_in_ms, *last_ago_ms)),
            _ => None,
        })
        .expect("a timer row");
    assert_eq!(next, Some(4_000), "9000 next minus 5000 now");
    assert_eq!(last, Some(4_000), "5000 now minus 1000 last");
}

/// Timers sit second, right after services: a section below 89 devices is
/// one nobody scrolls to, and catching a job that has been failing nightly
/// is the whole point of having it.
#[test]
fn the_timers_section_sits_second() {
    let units = vec![
        kinded("sys-x.device", ActiveState::Active, UnitKind::Device),
        unit("fine.service", ActiveState::Active),
        timer_unit("a.timer", "a.service", Some(9_000), None),
    ];
    let titles: Vec<String> =
        sections(&build_unit_rows(&units, &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000)).into_iter().map(|(t, _)| t).collect();
    assert_eq!(titles, vec!["Services", "Timers", "Devices"]);
}

/// A host with no timers gets no heading, like every other empty type.
#[test]
fn no_timers_means_no_timers_section() {
    let titles: Vec<String> =
        sections(&build_unit_rows(&[unit("a.service", ActiveState::Active)], &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000))
            .into_iter()
            .map(|(t, _)| t)
            .collect();
    assert_eq!(titles, vec!["Services"]);
}

/// Slices nest, and the units in them hang off the slice that owns them.
///
/// This is systemd's real containment hierarchy - the same one the procs
/// view folds by - and it is what makes `l` on a slice make sense:
/// `journalctl -u system.slice` returns every contained unit's log, which
/// reads as a wider answer than the row suggests until the row shows what
/// it contains.
#[test]
fn slices_nest_and_carry_the_units_they_own() {
    let units = vec![
        sliced("-.slice", UnitKind::Slice, None),
        sliced("system.slice", UnitKind::Slice, Some("-.slice")),
        sliced("user.slice", UnitKind::Slice, Some("-.slice")),
        sliced("sshd.service", UnitKind::Service, Some("system.slice")),
        sliced("dbus.service", UnitKind::Service, Some("system.slice")),
    ];
    // Everything open.
    let open: HashSet<String> = ["-.slice", "system.slice", "user.slice"].iter().map(|s| s.to_string()).collect();
    let rows = build_unit_rows(&units, &HashSet::new(), &HashSet::new(), &open, 5_000);

    let tree: Vec<(String, u32)> = rows
        .iter()
        .skip_while(|r| !matches!(r, Node::SectionHeader { title, .. } if title == "Slices"))
        .filter_map(|r| match r {
            Node::Unit { unit, depth, .. } => Some((unit.name.clone(), *depth)),
            _ => None,
        })
        .collect();
    assert_eq!(
        tree,
        vec![
            ("-.slice".to_string(), 0),
            ("system.slice".to_string(), 1),
            ("dbus.service".to_string(), 2),
            ("sshd.service".to_string(), 2),
            ("user.slice".to_string(), 1),
        ]
    );
}

/// A slice starts closed. Opening every slice by default would list all
/// 138 services a second time under `system.slice`, which is the opposite
/// of what a tree is for.
#[test]
fn a_slice_starts_closed() {
    let units = vec![sliced("-.slice", UnitKind::Slice, None), sliced("sshd.service", UnitKind::Service, Some("-.slice"))];
    let rows = build_unit_rows(&units, &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000);
    let under_slices: Vec<String> = rows
        .iter()
        .skip_while(|r| !matches!(r, Node::SectionHeader { title, .. } if title == "Slices"))
        .filter_map(|r| match r {
            Node::Unit { unit, .. } => Some(unit.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(under_slices, vec!["-.slice"], "the root is listed, its contents are not");
}

/// The type sections are untouched: a service is still under Services
/// whether or not it also hangs off a slice.
#[test]
fn the_tree_does_not_take_units_out_of_their_type_section() {
    let units = vec![sliced("-.slice", UnitKind::Slice, None), sliced("sshd.service", UnitKind::Service, Some("-.slice"))];
    let rows = build_unit_rows(&units, &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000);
    let in_services = rows
        .iter()
        .skip_while(|r| !matches!(r, Node::SectionHeader { title, .. } if title == "Services"))
        .take_while(|r| !matches!(r, Node::SectionHeader { title, .. } if title == "Slices"))
        .any(|r| matches!(r, Node::Unit { unit, .. } if unit.name == "sshd.service"));
    assert!(in_services, "{rows:#?}");
}

fn sliced(name: &str, kind: UnitKind, slice: Option<&str>) -> Unit {
    let mut u = kinded(name, ActiveState::Active, kind);
    u.slice = slice.map(str::to_string);
    u
}

/// A timer opens to the unit it runs - systemd's causal hierarchy, where
/// the slice tree is its containment one. For a timer this is the only
/// interesting question about it: the timer itself does nothing but wait.
#[test]
fn a_timer_opens_to_the_unit_it_runs() {
    let mut timer = timer_unit("nightly.timer", "nightly.service", Some(9_000), Some(1_000));
    timer.triggers = vec!["nightly.service".to_string()];
    let units = vec![timer, unit("nightly.service", ActiveState::Inactive)];
    let open: HashSet<String> = ["nightly.timer".to_string()].into_iter().collect();

    let rows = build_unit_rows(&units, &HashSet::new(), &HashSet::new(), &open, 5_000);
    let under: Vec<String> = rows
        .iter()
        .skip_while(|r| !matches!(r, Node::Timer { .. }))
        .filter_map(|r| match r {
            Node::Unit { unit, depth, .. } if *depth == 1 => Some(unit.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(under, vec!["nightly.service"], "{rows:#?}");
}

/// And it starts shut, like the slice tree: the unit it runs is already
/// listed under its own type.
#[test]
fn a_timer_starts_shut() {
    let mut timer = timer_unit("nightly.timer", "nightly.service", Some(9_000), Some(1_000));
    timer.triggers = vec!["nightly.service".to_string()];
    let rows = build_unit_rows(&[timer], &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000);
    assert!(!rows.iter().any(|r| matches!(r, Node::Unit { depth: 1, .. })), "{rows:#?}");
}

/// A trigger naming a unit the poll never saw is skipped, rather than
/// drawn as a row with nothing behind it.
#[test]
fn a_trigger_pointing_at_nothing_draws_nothing() {
    let mut timer = timer_unit("nightly.timer", "gone.service", Some(9_000), None);
    timer.triggers = vec!["gone.service".to_string()];
    let open: HashSet<String> = ["nightly.timer".to_string()].into_iter().collect();
    let rows = build_unit_rows(&[timer], &HashSet::new(), &HashSet::new(), &open, 5_000);
    assert!(!rows.iter().any(|r| matches!(r, Node::Unit { .. })), "{rows:#?}");
}

/// A timer's row is its schedule, not its state - which is the reason the
/// section exists: a timer sits `active` while the job it runs is broken.
#[test]
fn a_timer_section_renders_schedules_rather_than_unit_rows() {
    let units = vec![timer_unit("a.timer", "a.service", Some(9_000), Some(1_000))];
    let rows = build_unit_rows(&units, &HashSet::new(), &HashSet::new(), &HashSet::new(), 5_000);
    assert!(rows.iter().any(|r| matches!(r, Node::Timer { .. })));
    assert!(!rows.iter().any(|r| matches!(r, Node::Unit { .. })), "a timer is not listed twice");
}

/// A unit with nothing in the journal must say so. Pressing `l` is a
/// direct request for one unit's log, and replying with a blank screen
/// looks like the key failed rather than like the unit is quiet.
#[test]
fn a_unit_with_no_entries_says_so_rather_than_rendering_nothing() {
    let rows = build_log_rows(&[], Some("dbus.service"), 0, true, &Default::default());
    assert!(!rows.is_empty(), "a blank screen is not an answer");
    let named = rows.iter().any(|r| matches!(r, Node::SectionHeader { title, .. } if title.contains("dbus.service")));
    assert!(named, "and it names the unit that has nothing: {rows:#?}");
}

/// With no unit named there is no question being asked, so the view is
/// empty rather than inventing one. That is the state `4` lands on before
/// `l` has ever been pressed.
#[test]
fn the_log_view_is_empty_until_a_unit_names_it() {
    assert!(build_log_rows(&[], None, 0, true, &Default::default()).is_empty());
}

/// The sections are days now, and the unit they all belong to is named
/// once, in the header. It used to title the single section here, which
/// there is no longer one of.
#[test]
fn a_unit_scoped_journal_is_sectioned_by_day() {
    let titles: Vec<String> = build_log_rows(&[entry(100, "hello")], Some("sshd.service"), 0, true, &Default::default())
        .iter()
        .filter_map(|r| match r {
            Node::SectionHeader { title, .. } => Some(title.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(titles, vec!["1970-01-01"], "the day, not the unit");
}
