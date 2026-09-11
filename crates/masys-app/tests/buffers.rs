use std::collections::HashSet;

use masys_app::buffer::{Buffer, BufferGates, Registry};
use masys_app::io_buffer::IoBuffer;
use masys_app::log_buffer::{LogBuffer, TAIL};
use masys_app::systemd_buffer::SystemdBuffer;
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
    Filesystem {
        mount_point: mount.to_string(),
        used_percent: used,
        free_bytes: 1 << 30,
        inode_used_percent: 5.0,
        read_only,
    }
}

fn entry(ms: u64, message: &str) -> Entry {
    Entry {
        timestamp_ms: ms,
        unit: None,
        priority: Priority::Info,
        message: message.to_string(),
        origin: masys_domain::journal::Origin::Userspace,
    }
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
        kinded(
            "sys-devices-x.device",
            ActiveState::Active,
            UnitKind::Device,
        ),
        unit("a.service", ActiveState::Active),
        kinded("sshd.socket", ActiveState::Active, UnitKind::Socket),
        kinded("basic.target", ActiveState::Active, UnitKind::Target),
    ];
    let rows = SystemdBuffer {
        ..Default::default()
    }
    .rows(&units, &HashSet::new(), 5_000);
    assert_eq!(
        sections(&rows),
        vec![
            ("Services".to_string(), 1),
            ("Sockets".to_string(), 1),
            ("Targets".to_string(), 1),
            ("Devices".to_string(), 1)
        ]
    );
}

/// Failure stopped being a section; it did not stop mattering. A broken
/// unit is the first row under its own type rather than somewhere in an
/// alphabetical list of 138.
#[test]
fn a_failed_unit_sorts_to_the_top_of_its_type() {
    let units = vec![
        unit("a.service", ActiveState::Active),
        unit("z.service", ActiveState::Failed),
        unit("b.service", ActiveState::Activating),
    ];
    let names: Vec<String> = SystemdBuffer {
        ..Default::default()
    }
    .rows(&units, &HashSet::new(), 5_000)
    .iter()
    .filter_map(|r| match r {
        Node::Unit { unit, .. } => Some(unit.name.clone()),
        _ => None,
    })
    .collect();
    assert_eq!(
        names,
        vec!["z.service", "b.service", "a.service"],
        "failed, then activating, then active"
    );
}

/// A heading that is usually empty stops being read, so a type with no
/// units on this host - swap, scope - never appears at all.
#[test]
fn a_type_with_no_units_gets_no_heading() {
    let rows = SystemdBuffer {
        ..Default::default()
    }
    .rows(
        &[unit("a.service", ActiveState::Active)],
        &HashSet::new(),
        5_000,
    );
    assert_eq!(sections(&rows), vec![("Services".to_string(), 1)]);
}

#[test]
fn units_sort_by_name_within_a_type() {
    let units = vec![
        unit("z.service", ActiveState::Active),
        unit("a.service", ActiveState::Active),
    ];
    let names: Vec<String> = SystemdBuffer {
        ..Default::default()
    }
    .rows(&units, &HashSet::new(), 5_000)
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
    let rows = SystemdBuffer {
        ..Default::default()
    }
    .rows(
        &[unit("a.service", ActiveState::Active)],
        &HashSet::new(),
        5_000,
    );
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
    let rows = SystemdBuffer {
        ..Default::default()
    }
    .rows(&units, &HashSet::from(["Services".to_string()]), 5_000);
    assert_eq!(
        sections(&rows),
        vec![("Services".to_string(), 1)],
        "the count still tells you what is hidden"
    );
    assert!(!rows.iter().any(|r| matches!(r, Node::Unit { .. })));
}

/// A read-only filesystem is the urgent case this buffer exists for, and
/// the kernel can remount at any usage level - so it outranks percentage.
#[test]
fn io_puts_read_only_first_then_the_fullest() {
    let rows = IoBuffer {
        ..Default::default()
    }
    .rows(
        &[
            fs("/home", 12.0, false),
            fs("/boot", 96.0, false),
            fs("/data", 3.0, true),
        ],
        &[],
        &[],
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
            masys_domain::scan::DirSize {
                path: std::path::PathBuf::from("/data"),
                bytes: 1_000,
            },
            masys_domain::scan::DirSize {
                path: std::path::PathBuf::from("/data/big"),
                bytes: 400,
            },
            masys_domain::scan::DirSize {
                path: std::path::PathBuf::from("/data/half"),
                bytes: 200,
            },
        ],
        done: true,
        ..Default::default()
    };
    let rows = IoBuffer {
        scan,
        ..Default::default()
    }
    .rows(&[fs("/data", 50.0, false)], &[], &[]);
    let shares: Vec<(String, f32)> = rows
        .iter()
        .filter_map(|r| match r {
            Node::DirEntry { path, share, .. } => Some((path.display().to_string(), *share)),
            _ => None,
        })
        .collect();
    assert_eq!(shares.len(), 2, "{shares:?}");
    assert!(
        (shares[0].1 - 1.0).abs() < 0.01,
        "the largest fills its bar: {shares:?}"
    );
    assert!(
        (shares[1].1 - 0.5).abs() < 0.01,
        "half the largest, half the bar: {shares:?}"
    );
}

fn iface(
    name: &str,
    rx: u64,
    tx: u64,
    up: bool,
    loopback: bool,
) -> masys_domain::sample::Interface {
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
    let rows = IoBuffer {
        ..Default::default()
    }
    .rows(&[], &[], &interfaces);
    assert_eq!(
        interface_names(&rows),
        vec!["enp0s31f6".to_string(), "tailscale0".to_string()]
    );
}

/// Busiest first, the way Devices already orders by how busy they are -
/// and by name before any rate exists, so the list does not jump about on
/// the first tick.
#[test]
fn interfaces_are_ordered_by_throughput() {
    // Named so that alphabetical order and throughput order *disagree*.
    // The version of this fixture named them `slow` and `fast` sorted the
    // same way under either rule, so it passed against a builder that
    // ignored the rates entirely.
    let interfaces = vec![
        iface("enp0s31f6", 1, 1, true, false),
        iface("wlan0", 1, 1, true, false),
    ];
    let rate = |rx: f64, tx: f64| {
        (
            String::new(),
            masys_domain::rate::NetRate {
                rx_bytes_per_sec: rx,
                tx_bytes_per_sec: tx,
            },
        )
    };
    let rates = [
        ("enp0s31f6".to_string(), rate(10.0, 0.0).1),
        ("wlan0".to_string(), rate(9_000.0, 0.0).1),
    ];
    let rows = IoBuffer {
        net_rates: rates.to_vec(),
        ..Default::default()
    }
    .rows(&[], &[], &interfaces);
    assert_eq!(
        interface_names(&rows),
        vec!["wlan0".to_string(), "enp0s31f6".to_string()]
    );
}

/// A host with nothing worth showing gets no heading for it - the same
/// rule the Devices and Filesystems sections already follow.
#[test]
fn no_visible_interface_means_no_network_section() {
    let rows = IoBuffer {
        ..Default::default()
    }
    .rows(&[], &[], &[iface("lo", 5, 5, true, true)]);
    assert!(rows.is_empty(), "{rows:#?}");
}

/// The rate rides on the row, so a row can say "not measured yet" rather
/// than "idle" - the distinction `crate::rate` exists to preserve.
#[test]
fn an_interface_without_a_rate_yet_carries_none() {
    let rows = IoBuffer {
        ..Default::default()
    }
    .rows(&[], &[], &[iface("enp0s31f6", 10, 10, true, false)]);
    let carried = rows.iter().find_map(|r| match r {
        Node::Interface { rate, .. } => Some(*rate),
        _ => None,
    });
    assert_eq!(carried, Some(None), "no previous sample, so no rate");
}

#[test]
fn nothing_to_report_produces_no_rows() {
    assert!(
        IoBuffer {
            ..Default::default()
        }
        .rows(&[], &[], &[])
        .is_empty()
    );
}

/// The other direction reads in the order things happened, which is how
/// journalctl prints and what you want once you are reading rather than
/// checking.
#[test]
fn journal_entries_can_be_read_oldest_first() {
    let rows = LogBuffer {
        entries: [
            entry(300, "third"),
            entry(100, "first"),
            entry(200, "second"),
        ]
        .to_vec(),
        unit: Some("a.service".to_string()),
        newest_first: false,
    }
    .rows(0, &Default::default());
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
    let entries: Vec<Entry> = (0..TAIL as u64 + 50)
        .map(|n| entry(n, &format!("line {n}")))
        .collect();
    let rows = LogBuffer {
        entries: entries.to_vec(),
        unit: Some("busy.service".to_string()),
        newest_first: false,
    }
    .rows(0, &Default::default());
    let messages: Vec<String> = rows
        .iter()
        .filter_map(|r| match r {
            Node::JournalEntry(e) => Some(e.message.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), TAIL);
    assert_eq!(
        messages.last().unwrap(),
        &format!("line {}", TAIL + 49),
        "the newest survives"
    );
    assert_eq!(
        messages.first().unwrap(),
        "line 50",
        "the oldest is dropped"
    );
}

#[test]
fn an_empty_journal_produces_no_rows() {
    assert!(
        LogBuffer {
            entries: [].to_vec(),
            unit: None,
            newest_first: true
        }
        .rows(0, &Default::default())
        .is_empty()
    );
}

/// systemd reports an empty `StateChangeTimestamp` - 0 on the bus - for
/// units already in place before it started, such as the initrd-mounted
/// root. Treating that as a timestamp dates them to 1970.
#[test]
fn a_unit_with_no_recorded_transition_has_no_age() {
    let mut never = unit("-.mount", ActiveState::Active);
    never.since_ms = 0;
    let rows = SystemdBuffer {
        ..Default::default()
    }
    .rows(&[never], &HashSet::new(), 1_779_224_000_000);
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
    u.timer = Some(masys_domain::unit::Timer {
        next_ms: next,
        last_ms: last,
        activates: activates.to_string(),
    });
    u
}

fn timers(rows: &[Node]) -> Vec<(String, bool)> {
    rows.iter()
        .filter_map(|r| match r {
            Node::Timer {
                name,
                activated_failed,
                ..
            } => Some((name.clone(), *activated_failed)),
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
    let rows = SystemdBuffer {
        ..Default::default()
    }
    .rows(&units, &HashSet::new(), 5_000);
    assert_eq!(
        timers(&rows),
        vec![
            ("fine.timer".to_string(), false),
            ("nightly.timer".to_string(), true)
        ]
    );
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
    let names: Vec<String> = timers(
        &SystemdBuffer {
            ..Default::default()
        }
        .rows(&units, &HashSet::new(), 5_000),
    )
    .into_iter()
    .map(|(n, _)| n)
    .collect();
    assert_eq!(names, vec!["soon.timer", "later.timer", "never.timer"]);
}

/// The renderer has no clock, so both figures arrive as durations - the
/// conversion Node::Unit and triage already do.
#[test]
fn a_timer_row_carries_durations_not_timestamps() {
    let units = vec![timer_unit("a.timer", "a.service", Some(9_000), Some(1_000))];
    let rows = SystemdBuffer {
        ..Default::default()
    }
    .rows(&units, &HashSet::new(), 5_000);
    let (next, last) = rows
        .iter()
        .find_map(|r| match r {
            Node::Timer {
                next_in_ms,
                last_ago_ms,
                ..
            } => Some((*next_in_ms, *last_ago_ms)),
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
    let titles: Vec<String> = sections(
        &SystemdBuffer {
            ..Default::default()
        }
        .rows(&units, &HashSet::new(), 5_000),
    )
    .into_iter()
    .map(|(t, _)| t)
    .collect();
    assert_eq!(titles, vec!["Services", "Timers", "Devices"]);
}

/// A host with no timers gets no heading, like every other empty type.
#[test]
fn no_timers_means_no_timers_section() {
    let titles: Vec<String> = sections(
        &SystemdBuffer {
            ..Default::default()
        }
        .rows(
            &[unit("a.service", ActiveState::Active)],
            &HashSet::new(),
            5_000,
        ),
    )
    .into_iter()
    .map(|(t, _)| t)
    .collect();
    assert_eq!(titles, vec!["Services"]);
}

/// Slices nest, and the units in them hang off the slice that owns them.
///
/// This is systemd's real containment hierarchy - the same one the procs
/// buffer folds by - and it is what makes `l` on a slice make sense:
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
    let open: HashSet<String> = ["-.slice", "system.slice", "user.slice"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let rows = SystemdBuffer {
        open_slices: open.clone(),
        ..Default::default()
    }
    .rows(&units, &HashSet::new(), 5_000);

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
    let units = vec![
        sliced("-.slice", UnitKind::Slice, None),
        sliced("sshd.service", UnitKind::Service, Some("-.slice")),
    ];
    let rows = SystemdBuffer {
        ..Default::default()
    }
    .rows(&units, &HashSet::new(), 5_000);
    let under_slices: Vec<String> = rows
        .iter()
        .skip_while(|r| !matches!(r, Node::SectionHeader { title, .. } if title == "Slices"))
        .filter_map(|r| match r {
            Node::Unit { unit, .. } => Some(unit.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        under_slices,
        vec!["-.slice"],
        "the root is listed, its contents are not"
    );
}

/// The type sections are untouched: a service is still under Services
/// whether or not it also hangs off a slice.
#[test]
fn the_tree_does_not_take_units_out_of_their_type_section() {
    let units = vec![
        sliced("-.slice", UnitKind::Slice, None),
        sliced("sshd.service", UnitKind::Service, Some("-.slice")),
    ];
    let rows = SystemdBuffer {
        ..Default::default()
    }
    .rows(&units, &HashSet::new(), 5_000);
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

    let rows = SystemdBuffer {
        open_slices: open.clone(),
        ..Default::default()
    }
    .rows(&units, &HashSet::new(), 5_000);
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
    let rows = SystemdBuffer {
        ..Default::default()
    }
    .rows(&[timer], &HashSet::new(), 5_000);
    assert!(
        !rows
            .iter()
            .any(|r| matches!(r, Node::Unit { depth: 1, .. })),
        "{rows:#?}"
    );
}

/// A trigger naming a unit the poll never saw is skipped, rather than
/// drawn as a row with nothing behind it.
#[test]
fn a_trigger_pointing_at_nothing_draws_nothing() {
    let mut timer = timer_unit("nightly.timer", "gone.service", Some(9_000), None);
    timer.triggers = vec!["gone.service".to_string()];
    let open: HashSet<String> = ["nightly.timer".to_string()].into_iter().collect();
    let rows = SystemdBuffer {
        open_slices: open.clone(),
        ..Default::default()
    }
    .rows(&[timer], &HashSet::new(), 5_000);
    assert!(
        !rows.iter().any(|r| matches!(r, Node::Unit { .. })),
        "{rows:#?}"
    );
}

/// A timer's row is its schedule, not its state - which is the reason the
/// section exists: a timer sits `active` while the job it runs is broken.
#[test]
fn a_timer_section_renders_schedules_rather_than_unit_rows() {
    let units = vec![timer_unit("a.timer", "a.service", Some(9_000), Some(1_000))];
    let rows = SystemdBuffer {
        ..Default::default()
    }
    .rows(&units, &HashSet::new(), 5_000);
    assert!(rows.iter().any(|r| matches!(r, Node::Timer { .. })));
    assert!(
        !rows.iter().any(|r| matches!(r, Node::Unit { .. })),
        "a timer is not listed twice"
    );
}

/// A unit with nothing in the journal must say so. Pressing `l` is a
/// direct request for one unit's log, and replying with a blank screen
/// looks like the key failed rather than like the unit is quiet.
#[test]
fn a_unit_with_no_entries_says_so_rather_than_rendering_nothing() {
    let rows = LogBuffer {
        entries: [].to_vec(),
        unit: Some("dbus.service".to_string()),
        newest_first: true,
    }
    .rows(0, &Default::default());
    assert!(!rows.is_empty(), "a blank screen is not an answer");
    let named = rows
        .iter()
        .any(|r| matches!(r, Node::SectionHeader { title, .. } if title.contains("dbus.service")));
    assert!(named, "and it names the unit that has nothing: {rows:#?}");
}

/// With no unit named there is no question being asked, so the buffer is
/// empty rather than inventing one. That is the state `4` lands on before
/// `l` has ever been pressed.
#[test]
fn the_log_buffer_is_empty_until_a_unit_names_it() {
    assert!(
        LogBuffer {
            entries: [].to_vec(),
            unit: None,
            newest_first: true
        }
        .rows(0, &Default::default())
        .is_empty()
    );
}

/// The sections are days now, and the unit they all belong to is named
/// once, in the header. It used to title the single section here, which
/// there is no longer one of.
#[test]
fn a_unit_scoped_journal_is_sectioned_by_day() {
    let titles: Vec<String> = LogBuffer {
        entries: [entry(100, "hello")].to_vec(),
        unit: Some("sshd.service".to_string()),
        newest_first: true,
    }
    .rows(0, &Default::default())
    .iter()
    .filter_map(|r| match r {
        Node::SectionHeader { title, .. } => Some(title.clone()),
        _ => None,
    })
    .collect();
    assert_eq!(titles, vec!["1970-01-01"], "the day, not the unit");
}

/// The whole reason the registry stops being a static slice: on Debian
/// there is no Nix buffer, and a digit that opens an empty screen explaining
/// that NixOS was not found is worse than a digit that does nothing.
#[test]
fn the_nix_buffer_is_absent_without_a_declarative_service() {
    let registry = Registry::new(BufferGates::NONE);
    assert!(!registry.contains(Buffer::Nix));
    assert!(registry.by_key('5').is_none());
}

#[test]
fn the_nix_buffer_takes_digit_five_when_the_service_is_present() {
    let registry = Registry::new(BufferGates {
        declarative: true,
        packages: false,
    });
    assert_eq!(registry.by_key('5'), Some(Buffer::Nix));
}

/// Digits 1 to 4 are unaffected either way.
#[test]
fn the_existing_buffers_keep_their_digits_in_both_registries() {
    for present in [false, true] {
        let registry = Registry::new(BufferGates {
            declarative: present,
            packages: present,
        });
        assert_eq!(registry.by_key('1'), Some(Buffer::Status));
        assert_eq!(registry.by_key('4'), Some(Buffer::Io));
    }
}

/// `tab` on a slice shows its subtree, and the same key hides it again.
///
/// Asserted here rather than through a keypress because the toggle is
/// this buffer's own: breaking it so that a slice could only ever open
/// failed no test at all while the two lines lived in `App`.
#[test]
fn a_slice_subtree_shows_and_hides_on_the_same_key() {
    let mut systemd = SystemdBuffer::default();
    systemd.toggle_slice("system.slice".to_string());
    assert!(
        systemd.open_slices.contains("system.slice"),
        "the first press shows it"
    );
    systemd.toggle_slice("system.slice".to_string());
    assert!(
        systemd.open_slices.is_empty(),
        "the second press hides it again"
    );
}

/// The buffer-wide cycle shuts open details along with the sections
/// holding them.
///
/// A detail left open is inside a section that is about to stop existing,
/// and it would spring back the next time that section did - which is the
/// reason `shut_all` clears both sets rather than just the slices.
#[test]
fn shutting_everything_shuts_the_details_inside_it() {
    let mut systemd = SystemdBuffer {
        open_slices: ["system.slice".to_string()].into_iter().collect(),
        expanded: [("sshd.service".to_string(), None)].into_iter().collect(),
    };
    systemd.shut_all();
    assert!(systemd.open_slices.is_empty(), "the slice subtrees close");
    assert!(
        systemd.expanded.is_empty(),
        "and so do the details that were inside them"
    );
}

/// `6` opens Packages, and only on a host whose platform can list any.
///
/// The same gate the Nix buffer has and for the same reason: a digit that
/// opened a buffer with nothing behind it would be a key that reaches a
/// screen of dashes pretending to be a reading.
#[test]
fn the_packages_buffer_exists_only_where_something_can_list_packages() {
    let with = Registry::new(BufferGates {
        declarative: false,
        packages: true,
    });
    assert_eq!(with.by_key('6'), Some(Buffer::Packages));
    assert!(with.contains(Buffer::Packages));

    let without = Registry::new(BufferGates::NONE);
    assert_eq!(without.by_key('6'), None);
    assert!(!without.contains(Buffer::Packages));
}

/// The two gates are independent. A Debian host lists packages and has no
/// generations; a NixOS host has both; and neither implies the other.
#[test]
fn the_nix_and_packages_gates_are_independent() {
    for (declarative, packages) in [(false, false), (false, true), (true, false), (true, true)] {
        let registry = Registry::new(BufferGates {
            declarative,
            packages,
        });
        assert_eq!(
            registry.contains(Buffer::Nix),
            declarative,
            "nix gate: {declarative} {packages}"
        );
        assert_eq!(
            registry.contains(Buffer::Packages),
            packages,
            "packages gate: {declarative} {packages}"
        );
    }
}

/// **`Buffer::ALL` lists exactly the catalogue.**
///
/// `ALL`'s own doc says it is "the whole enum… a rule that only held on
/// the hosts where a buffer happened to exist would be a rule that stops
/// being checked" - and until 2026-08-30 nothing enforced it. `Packages`
/// was missing from it for the whole life of that buffer, and deleting a
/// variant from `ALL` passed the entire workspace suite.
///
/// Checked against `BUFFERS` rather than by listing the variants again,
/// which would just be a third place to forget: a buffer with no
/// `BUFFERS` entry cannot be reached or titled at all - `Buffer::title`
/// panics and `every_buffer_is_registered` catches it - so the catalogue
/// is effectively the enum, and equality with it is the closed loop.
#[test]
fn buffer_all_lists_exactly_the_catalogue() {
    let catalogue: Vec<Buffer> = masys_app::buffer::BUFFERS
        .iter()
        .map(|spec| spec.buffer)
        .collect();

    for buffer in &catalogue {
        assert!(
            Buffer::ALL.contains(buffer),
            "{buffer:?} is in BUFFERS but missing from Buffer::ALL, so every \
             rule that iterates ALL silently stops covering it"
        );
    }
    assert_eq!(
        Buffer::ALL.len(),
        catalogue.len(),
        "ALL and BUFFERS must be the same set: ALL={:?} catalogue={:?}",
        Buffer::ALL,
        catalogue
    );
}
