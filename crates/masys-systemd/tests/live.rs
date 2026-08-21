//! Exercised against the machine running the suite, not a fixture.
//!
//! These are the only tests here that need a real system, and they assert
//! *invariants* rather than values: this host's process count, unit count,
//! and mount list all change between runs, so anything stricter would be
//! a flake rather than a test.
//!
//! `SystemdService::connect` needs the system bus. In an environment
//! without one - a minimal container, some CI images - every test below
//! skips rather than failing, since a missing bus is not a defect in
//! this crate.

use masys_domain::service::SystemService;
use masys_systemd::SystemdService;

const WINDOW_MS: u64 = 3_600_000;

fn service() -> Option<SystemdService> {
    SystemdService::connect(WINDOW_MS).ok()
}

#[test]
fn sample_reads_this_machine() {
    let Some(svc) = service() else { return };
    let snapshot = svc.sample().expect("a sample");

    assert!(!snapshot.procs.is_empty(), "a running host has processes");
    // The suite itself is running, so its own pid must be in the sweep.
    let me = snapshot.procs.iter().find(|p| p.pid == std::process::id()).expect("this test process");
    assert!(me.rss_bytes > 0, "a running process has resident memory");
    assert!(me.threads >= 1);
    // Unix milliseconds, from btime + starttime ticks. Sanity-check the
    // epoch rather than the value: a ticks/seconds mix-up lands decades off.
    assert!(me.started_at_ms > 1_600_000_000_000, "started_at_ms is Unix ms: {}", me.started_at_ms);

    assert!(snapshot.load.is_some(), "/proc/loadavg is always readable");
    assert!(snapshot.uptime_secs.unwrap_or(0) > 0);
    let memory = snapshot.memory.expect("/proc/meminfo is always readable");
    assert!(memory.total_bytes > 0);
    assert!(memory.used_bytes <= memory.total_bytes, "used cannot exceed total");
}

/// The Disk section's inputs. Every filesystem reported must be
/// measurable, or the percentages are meaningless.
#[test]
fn filesystems_are_real_and_deduplicated() {
    let Some(svc) = service() else { return };
    let filesystems = svc.sample().expect("a sample").filesystems;

    assert!(!filesystems.is_empty(), "at least the root filesystem");
    for fs in &filesystems {
        assert!(fs.used_percent >= 0.0 && fs.used_percent <= 100.0, "{fs:?}");
        assert!(fs.inode_used_percent >= 0.0 && fs.inode_used_percent <= 100.0, "{fs:?}");
    }
    let mut points: Vec<&str> = filesystems.iter().map(|f| f.mount_point.as_str()).collect();
    points.sort_unstable();
    let before = points.len();
    points.dedup();
    assert_eq!(points.len(), before, "a mount point is reported once: {filesystems:#?}");
}

/// `statvfs` on an autofs mount point triggers the automount, so the scan
/// must never reach a path that is *only* an autofs trigger.
///
/// An already-automounted path appears in `/proc/mounts` twice - as its
/// autofs trigger and as the real filesystem underneath - and measuring
/// that second entry is correct. Only a trigger with nothing mounted on
/// it yet is off limits, because touching it is what performs the mount.
#[test]
fn an_unmounted_autofs_trigger_is_never_measured() {
    let Some(svc) = service() else { return };
    let mounts = std::fs::read_to_string("/proc/mounts").expect("/proc/mounts");
    let entries: Vec<(&str, &str)> = mounts
        .lines()
        .filter_map(|l| {
            let mut c = l.split_whitespace();
            let _device = c.next()?;
            let point = c.next()?;
            Some((point, c.next()?))
        })
        .collect();
    let dormant: Vec<&str> = entries
        .iter()
        .filter(|(point, fstype)| *fstype == "autofs" && !entries.iter().any(|(p, f)| p == point && *f != "autofs"))
        .map(|(point, _)| *point)
        .collect();
    if dormant.is_empty() {
        return;
    }
    let measured = svc.sample().expect("a sample").filesystems;
    for point in dormant {
        assert!(
            !measured.iter().any(|f| f.mount_point == point),
            "{point} is a dormant autofs trigger; stat'ing it would perform the mount: {measured:#?}"
        );
    }
}

#[test]
fn units_reads_this_machines_units() {
    let Some(svc) = service() else { return };
    let units = svc.units().expect("units");

    assert!(!units.is_empty(), "a running host has units");
    // Every unit systemd knows, nothing filtered. `.device` units used to
    // be dropped for costing a round trip each; none of them costs one now.
    for suffix in [".service", ".device", ".target", ".socket", ".mount", ".timer"] {
        assert!(units.iter().any(|u| u.name.ends_with(suffix)), "no {suffix} unit is listed");
    }
    for unit in &units {
        assert!(!unit.sub_state.is_empty(), "every unit has a sub-state: {unit:?}");
    }
}

/// The cold cache must not change *what* `units()` reports, only how fast
/// it gets there - so the units present in both polls must agree on every
/// cached field.
///
/// Deliberately compares the intersection rather than the whole list. The
/// unit set genuinely changes between two immediate polls: reading
/// `timedate1.NTPSynchronized` in `sample()` D-Bus-activates
/// `systemd-timedated.service`, so masys observing the clock can itself
/// bring a unit into existence. Asserting list equality fails for a
/// correct implementation.
#[test]
fn a_warm_cold_cache_agrees_with_the_fresh_read() {
    let Some(svc) = service() else { return };
    let first = svc.units().expect("units");
    let second = svc.units().expect("units");

    let mut compared = 0;
    for a in &first {
        let Some(b) = second.iter().find(|u| u.name == a.name) else { continue };
        assert_eq!(a.enabled, b.enabled, "{}: cached enabled must match the fresh read", a.name);
        assert_eq!(a.cgroup, b.cgroup, "{}: cached cgroup must match", a.name);
        compared += 1;
    }
    assert!(compared > 0, "the two polls shared no units at all, which cannot be right");
}

/// Restart history is recovered from the journal at startup, so the
/// first poll can already know about restarts that happened before masys
/// was running. Whatever it reports must be real: inside the window, and
/// in the past.
#[test]
fn the_first_poll_may_report_history_recovered_from_the_journal() {
    let Some(svc) = service() else { return };
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("a clock after 1970").as_millis() as u64;
    let units = svc.units().expect("units");

    for unit in &units {
        for stamp in &unit.restart_timestamps_ms {
            assert!(*stamp <= now_ms, "{}: a restart in the future: {stamp}", unit.name);
            assert!(now_ms - stamp <= WINDOW_MS + 60_000, "{}: a restart older than the window: {stamp}", unit.name);
        }
    }
    // A host with a volatile journal legitimately recovers nothing, so
    // this asserts shape rather than count.
}

/// The unit-scoped read is the only journal read left. A unit with an
/// empty log is legitimate - most units on a host have one - so this
/// asserts shape rather than count.
#[test]
fn a_units_journal_reads_that_unit() {
    let Some(svc) = service() else { return };
    let entries = svc.unit_journal("systemd-journald.service", 20).expect("journal entries");
    for entry in &entries {
        assert!(entry.timestamp_ms > 1_600_000_000_000, "journal timestamps are Unix ms: {entry:?}");
        // `-u` returns two things: the unit's own output, tagged with the
        // unit by journald from the sender's cgroup, and systemd's
        // messages *about* it, which come from pid 1 and are tagged
        // `init.scope`. Both belong; nothing else does. This is the same
        // distinction `App::last_words` relies on to tell a unit's own
        // last words from systemd's report that it died.
        if let Some(unit) = &entry.unit {
            assert!(
                unit == "systemd-journald.service" || unit == "init.scope",
                "a -u query returns the unit and pid 1's messages about it, not {unit}"
            );
        }
    }
}

/// The socket tables and the descriptor list, against this machine.
///
/// The byte-swapping in `proc::net` is the kind of thing that passes a
/// fixture and fails a real host, because a fixture is written by whoever
/// wrote the parser.
#[test]
fn proc_detail_reads_this_process() {
    use masys_systemd::proc;
    let sockets = proc::net::read_socket_table();
    let pid = std::process::id();
    let detail = proc::detail::read_proc_detail(pid, &sockets).expect("this process exists");

    let cmdline = detail.cmdline.as_deref().expect("a test binary has a command line");
    assert!(cmdline.contains("live"), "the running test binary: {cmdline:?}");
    assert!(detail.exe.as_deref().unwrap_or_default().contains("live"), "{:?}", detail.exe);
    assert!(detail.cwd.is_some(), "a process always has a working directory");
    assert!(detail.ppid.is_some(), "every process but init has a parent");
    assert!(detail.virt_bytes.unwrap_or(0) > 0, "a userspace process has mapped memory: {:?}", detail.virt_bytes);
    assert!(detail.env.iter().any(|(key, _)| key == "PATH"), "PATH is always set");

    // 0, 1 and 2 always exist, whatever they point at.
    let standard: Vec<u32> = detail.fds.iter().filter(|fd| fd.is_standard()).map(|fd| fd.number).collect();
    assert_eq!(standard, vec![0, 1, 2], "{:?}", detail.fds);
}

/// A dead pid has to be distinguishable from one masys merely cannot
/// read: every field degrades to absent, so without an explicit check
/// both would open a row full of blanks.
#[test]
fn a_dead_pid_is_an_error_not_an_empty_detail() {
    use masys_systemd::proc;
    // Above the kernel's default pid_max, so it cannot be live.
    assert!(proc::detail::read_proc_detail(4_294_967_290, &Default::default()).is_err());
}
