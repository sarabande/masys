use masys_domain::rate;
use masys_domain::sample::{Pressure, Proc, ProcState, Snapshot, SystemState};

fn snapshot_at(taken_at_ms: u64, procs: Vec<Proc>) -> Snapshot {
    Snapshot {
        taken_at_ms,
        procs,
        pressure: Some(Pressure::default()),
        filesystems: Vec::new(),
        disks: Vec::new(),
        clock_synced: true,
        utc_offset_secs: -21_600,
        interfaces: Vec::new(),
        oom_kills: Vec::new(),
        system_state: SystemState::Running,
        clock_ticks_per_sec: 100,
        machine: None,
        load: None,
        uptime_secs: None,
        memory: None,
    }
}

fn proc_at(pid: u32, cpu_ticks: u64, io_read_bytes: u64, io_write_bytes: u64) -> Proc {
    Proc {
        pid,
        comm: "test".to_string(),
        cgroup: None,
        cpu_ticks,
        rss_bytes: 0,
        io_read_bytes,
        io_write_bytes,
        state: ProcState::Running,
        nice: 0,
        oom_score: 0,
        threads: 1,
        started_at_ms: 0,
    }
}

#[test]
fn cpu_percent_over_a_two_second_tick() {
    let prev = snapshot_at(0, vec![proc_at(1204, 100, 0, 0)]);
    let curr = snapshot_at(2_000, vec![proc_at(1204, 300, 0, 0)]);

    let rates = rate::derive(&prev, &curr, 100);

    assert_eq!(rates.len(), 1);
    assert!((rates[0].cpu_percent - 100.0).abs() < 0.01, "{:?}", rates[0]);
}

#[test]
fn io_bytes_per_second() {
    let prev = snapshot_at(0, vec![proc_at(1, 0, 1_000, 500)]);
    let curr = snapshot_at(1_000, vec![proc_at(1, 0, 3_000, 500)]);

    let rates = rate::derive(&prev, &curr, 100);

    assert_eq!(rates.len(), 1);
    assert!((rates[0].io_read_bytes_per_sec - 2_000.0).abs() < 0.01);
    assert!((rates[0].io_write_bytes_per_sec - 0.0).abs() < 0.01);
}

#[test]
fn a_pid_absent_from_the_previous_sample_has_no_rate_yet() {
    let prev = snapshot_at(0, vec![]);
    let curr = snapshot_at(2_000, vec![proc_at(9999, 100, 0, 0)]);

    let rates = rate::derive(&prev, &curr, 100);

    assert!(rates.is_empty(), "pid 9999 wasn't in prev - the view renders '-', not 0.0%");
}

#[test]
fn a_reused_pid_with_lower_counters_is_treated_as_unmeasured_not_negative() {
    let prev = snapshot_at(0, vec![proc_at(50, 500, 0, 0)]);
    // Same pid, but cpu_ticks went backwards - a different process reused it.
    let curr = snapshot_at(2_000, vec![proc_at(50, 10, 0, 0)]);

    let rates = rate::derive(&prev, &curr, 100);

    assert!(rates.is_empty());
}

#[test]
fn no_elapsed_time_yields_no_rates_rather_than_dividing_by_zero() {
    let prev = snapshot_at(1_000, vec![proc_at(1, 100, 0, 0)]);
    let curr = snapshot_at(1_000, vec![proc_at(1, 200, 0, 0)]);

    assert!(rate::derive(&prev, &curr, 100).is_empty());
}

/// `/proc/diskstats` sectors are always 512 bytes regardless of the
/// device's real block size - the kernel fixes that unit so the field
/// means the same thing on every disk.
#[test]
fn disk_rates_convert_sectors_at_512_bytes() {
    let disk = |read_sectors, write_sectors, io_ms| masys_domain::sample::Disk {
        name: "sda".to_string(),
        reads: 0,
        writes: 0,
        read_sectors,
        write_sectors,
        io_ms,
        in_flight: 0,
    };
    let mut prev = snapshot_at(0, Vec::new());
    let mut curr = snapshot_at(1_000, Vec::new());
    prev.disks = vec![disk(0, 0, 0)];
    curr.disks = vec![disk(2, 4, 500)];

    let rates = masys_domain::rate::derive_disks(&prev, &curr);
    assert_eq!(rates.len(), 1);
    let (name, rate) = &rates[0];
    assert_eq!(name, "sda");
    assert_eq!(rate.read_bytes_per_sec, 1024.0, "2 sectors in one second");
    assert_eq!(rate.write_bytes_per_sec, 2048.0);
    // 500ms of IO time in 1000ms of wall clock.
    assert!((rate.busy_percent - 50.0).abs() < 0.01, "{}", rate.busy_percent);
}

/// A device removed and re-added resets its counters, and a negative
/// delta would render as an enormous rate.
#[test]
fn a_device_whose_counters_went_backwards_has_no_rate() {
    let disk = |read_sectors| masys_domain::sample::Disk {
        name: "sda".to_string(),
        reads: 0,
        writes: 0,
        read_sectors,
        write_sectors: 0,
        io_ms: 0,
        in_flight: 0,
    };
    let mut prev = snapshot_at(0, Vec::new());
    let mut curr = snapshot_at(1_000, Vec::new());
    prev.disks = vec![disk(1_000)];
    curr.disks = vec![disk(5)];
    assert!(masys_domain::rate::derive_disks(&prev, &curr).is_empty());
}

fn interface(name: &str, rx_bytes: u64, tx_bytes: u64, rx_packets: u64) -> masys_domain::sample::Interface {
    masys_domain::sample::Interface {
        name: name.to_string(),
        rx_bytes,
        tx_bytes,
        rx_packets,
        tx_packets: 0,
        rx_errs: 0,
        tx_errs: 0,
        rx_drop: 0,
        tx_drop: 0,
        up: true,
        loopback: false,
    }
}

/// The same shape as `derive_disks`, for the same reason: a counter is
/// only useful as a difference over a known interval.
#[test]
fn an_interface_gets_a_throughput_from_two_samples() {
    let mut prev = snapshot_at(0, Vec::new());
    let mut curr = snapshot_at(1_000, Vec::new());
    prev.interfaces = vec![interface("enp0s31f6", 1_000, 2_000, 10)];
    curr.interfaces = vec![interface("enp0s31f6", 3_048, 6_096, 30)];

    let rates = masys_domain::rate::derive_nets(&prev, &curr);
    assert_eq!(rates.len(), 1);
    let (name, rate) = &rates[0];
    assert_eq!(name, "enp0s31f6");
    assert_eq!(rate.rx_bytes_per_sec, 2_048.0, "2048 bytes in one second");
    assert_eq!(rate.tx_bytes_per_sec, 4_096.0);
    assert_eq!(rate.rx_packets_per_sec, 20.0);
}

/// An interface that goes away and comes back resets its counters, and a
/// negative delta would render as an enormous rate.
#[test]
fn an_interface_whose_counters_went_backwards_has_no_rate() {
    let mut prev = snapshot_at(0, Vec::new());
    let mut curr = snapshot_at(1_000, Vec::new());
    prev.interfaces = vec![interface("tailscale0", 9_000, 0, 0)];
    curr.interfaces = vec![interface("tailscale0", 12, 0, 0)];
    assert!(masys_domain::rate::derive_nets(&prev, &curr).is_empty());
}

/// An interface that was not in the previous sample has nothing to
/// difference against, and `None` must stay distinguishable from idle.
#[test]
fn an_interface_seen_only_once_has_no_rate_yet() {
    let prev = snapshot_at(0, Vec::new());
    let mut curr = snapshot_at(1_000, Vec::new());
    curr.interfaces = vec![interface("wg0", 500, 500, 5)];
    assert!(masys_domain::rate::derive_nets(&prev, &curr).is_empty());
}
