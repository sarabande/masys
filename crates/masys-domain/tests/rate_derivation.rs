use masys_domain::rate;
use masys_domain::sample::{CpuTimes, Pressure, Proc, ProcState, Snapshot, SystemState};

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
        cpu_times: None,
        thermal_throttled_ms_by_core: None,
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
    assert!(
        (rates[0].cpu_percent - 100.0).abs() < 0.01,
        "{:?}",
        rates[0]
    );
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

    assert!(
        rates.is_empty(),
        "pid 9999 wasn't in prev - the view renders '-', not 0.0%"
    );
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
    assert!(
        (rate.busy_percent - 50.0).abs() < 0.01,
        "{}",
        rate.busy_percent
    );
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
    };
    let mut prev = snapshot_at(0, Vec::new());
    let mut curr = snapshot_at(1_000, Vec::new());
    prev.disks = vec![disk(1_000)];
    curr.disks = vec![disk(5)];
    assert!(masys_domain::rate::derive_disks(&prev, &curr).is_empty());
}

fn interface(
    name: &str,
    rx_bytes: u64,
    tx_bytes: u64,
    rx_packets: u64,
) -> masys_domain::sample::Interface {
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

/// Bytes alone do not notice every reset, which is why the guard reads
/// the packet counters too - they are its only reader now that no packet
/// rate is reported.
///
/// An interface removed and re-added can carry more bytes than it had:
/// a tunnel that comes back up and immediately transfers a large frame
/// clears the old byte total while its packet count is still near zero.
/// Comparing bytes alone sees a healthy 2 KB/s and reports it.
#[test]
fn an_interface_whose_packets_went_backwards_has_no_rate() {
    let mut prev = snapshot_at(0, Vec::new());
    let mut curr = snapshot_at(1_000, Vec::new());
    prev.interfaces = vec![interface("tailscale0", 1_000, 0, 900)];
    curr.interfaces = vec![interface("tailscale0", 3_048, 0, 20)];
    assert!(
        masys_domain::rate::derive_nets(&prev, &curr).is_empty(),
        "bytes rose but packets fell, so the counters reset"
    );
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

fn cpu_snapshot(taken_at_ms: u64, cpu_times: Option<CpuTimes>) -> Snapshot {
    Snapshot {
        cpu_times,
        ..snapshot_at(taken_at_ms, Vec::new())
    }
}

/// Utilisation is the share of the interval the CPU was not idle, which
/// needs two readings of a counter that only goes up.
#[test]
fn cpu_utilisation_is_the_busy_share_of_the_interval() {
    let prev = cpu_snapshot(
        0,
        Some(CpuTimes {
            total_ticks: 1_000,
            idle_ticks: 800,
        }),
    );
    // 400 ticks passed, 100 of them idle: three quarters busy.
    let curr = cpu_snapshot(
        2_000,
        Some(CpuTimes {
            total_ticks: 1_400,
            idle_ticks: 900,
        }),
    );
    let percent = rate::derive_cpu(&prev, &curr).expect("two samples of a counter");
    assert!((percent - 75.0).abs() < 0.01, "expected 75%, got {percent}");
}

/// The rule the whole `rate` module exists to keep: one sample is not a
/// rate, and an unmeasured CPU must not read as an idle one.
#[test]
fn cpu_utilisation_needs_two_samples_and_a_kernel_that_reports_them() {
    let measured = Some(CpuTimes {
        total_ticks: 1_000,
        idle_ticks: 800,
    });
    assert_eq!(
        rate::derive_cpu(&cpu_snapshot(0, None), &cpu_snapshot(2_000, measured)),
        None,
        "the first sample never read the counter"
    );
    assert_eq!(
        rate::derive_cpu(&cpu_snapshot(0, measured), &cpu_snapshot(2_000, None)),
        None,
        "and neither did the second"
    );
}

/// A counter that went backwards is a machine that rebooted between
/// ticks, not one that idled. `derive` refuses the same way for a reused
/// pid.
#[test]
fn cpu_utilisation_refuses_counters_that_went_backwards() {
    let prev = cpu_snapshot(
        0,
        Some(CpuTimes {
            total_ticks: 5_000,
            idle_ticks: 4_000,
        }),
    );
    let after_reboot = cpu_snapshot(
        2_000,
        Some(CpuTimes {
            total_ticks: 40,
            idle_ticks: 30,
        }),
    );
    assert_eq!(rate::derive_cpu(&prev, &after_reboot), None);
}

/// Two samples with no ticks between them - a tick that arrived early, or
/// a clock that did not advance - divide by zero. `None`, not a busy CPU.
#[test]
fn cpu_utilisation_of_an_interval_with_no_ticks_is_no_reading() {
    let times = Some(CpuTimes {
        total_ticks: 1_000,
        idle_ticks: 800,
    });
    assert_eq!(
        rate::derive_cpu(&cpu_snapshot(0, times), &cpu_snapshot(2_000, times)),
        None
    );
}

/// A whole machine's throughput is the sum of what its devices are
/// moving, and nothing else - `busy_percent` has no meaning added up
/// across devices, so it is not carried.
#[test]
fn throughput_adds_up_what_each_device_is_moving() {
    let disks = vec![
        (
            "nvme0n1".to_string(),
            rate::DiskRate {
                read_bytes_per_sec: 1_000.0,
                write_bytes_per_sec: 250.0,
                reads_per_sec: 4.0,
                writes_per_sec: 1.0,
                busy_percent: 40.0,
            },
        ),
        (
            "sda".to_string(),
            rate::DiskRate {
                read_bytes_per_sec: 500.0,
                write_bytes_per_sec: 125.0,
                reads_per_sec: 2.0,
                writes_per_sec: 1.0,
                busy_percent: 90.0,
            },
        ),
    ];
    assert_eq!(
        rate::Throughput::of_disks(&disks),
        rate::Throughput {
            in_bytes_per_sec: 1_500.0,
            out_bytes_per_sec: 375.0,
        }
    );

    let nets = vec![
        (
            "eth0".to_string(),
            rate::NetRate {
                rx_bytes_per_sec: 90.0,
                tx_bytes_per_sec: 10.0,
            },
        ),
        (
            "wlan0".to_string(),
            rate::NetRate {
                rx_bytes_per_sec: 9.0,
                tx_bytes_per_sec: 1.0,
            },
        ),
    ];
    assert_eq!(
        rate::Throughput::of_nets(&nets),
        rate::Throughput {
            in_bytes_per_sec: 99.0,
            out_bytes_per_sec: 11.0,
        }
    );
}

/// Nothing to add up is a measured zero: a host with no devices moved
/// no bytes. It is the *caller* that knows whether anything was measured
/// at all, which is why this returns a total rather than an `Option`.
#[test]
fn throughput_of_nothing_is_zero_rather_than_absent() {
    assert_eq!(
        rate::Throughput::of_disks(&[]),
        rate::Throughput {
            in_bytes_per_sec: 0.0,
            out_bytes_per_sec: 0.0,
        }
    );
}

fn thermal_snapshot(taken_at_ms: u64, cores: &[(u32, u64)]) -> Snapshot {
    Snapshot {
        thermal_throttled_ms_by_core: Some(cores.iter().copied().collect()),
        ..snapshot_at(taken_at_ms, Vec::new())
    }
}

fn unmeasured_snapshot(taken_at_ms: u64) -> Snapshot {
    Snapshot {
        thermal_throttled_ms_by_core: None,
        ..snapshot_at(taken_at_ms, Vec::new())
    }
}

/// Throttling is the share of the interval a core spent held below the
/// clock it asked for - the same shape as CPU utilisation, and for the
/// same reason: the kernel counts milliseconds, and a count is not a rate.
#[test]
fn thermal_throttling_is_the_held_back_share_of_the_interval() {
    let percent = rate::derive_thermal(
        &thermal_snapshot(0, &[(0, 1_000)]),
        &thermal_snapshot(2_000, &[(0, 1_500)]),
    )
    .expect("two samples of a counter");
    assert!((percent - 25.0).abs() < 0.01, "expected 25%, got {percent}");
}

/// The defect this reading was rebuilt for. Core 0 carries a second of
/// throttling from some earlier event and is idle now; core 1 spends
/// 800 ms of this 2 s interval throttled. Differencing the maxima gives
/// 1000 - 1000 = 0, so the old shape reported a perfectly cool machine
/// and produced no finding - below every threshold, silently.
#[test]
fn one_cores_history_does_not_mask_another_core_throttling_now() {
    let percent = rate::derive_thermal(
        &thermal_snapshot(0, &[(0, 1_000), (1, 0)]),
        &thermal_snapshot(2_000, &[(0, 1_000), (1, 800)]),
    )
    .expect("core 1 was throttled for 800 ms of the interval");
    assert!((percent - 40.0).abs() < 0.01, "expected 40%, got {percent}");
}

/// The worst core, not the total across them. Cores throttle in groups -
/// on the host this was written against, two hyperthread siblings of one
/// physical core reported the same figure while six reported zero - and
/// adding those together would claim more throttling than there was time.
#[test]
fn thermal_throttling_is_the_worst_core_not_their_sum() {
    let percent = rate::derive_thermal(
        &thermal_snapshot(0, &[(0, 0), (1, 0), (2, 0)]),
        &thermal_snapshot(2_000, &[(0, 600), (1, 600), (2, 100)]),
    )
    .expect("three cores, one interval");
    assert!((percent - 30.0).abs() < 0.01, "expected 30%, got {percent}");
}

/// The rule `pressure` keeps and this must too: a machine whose kernel
/// does not account for throttling has not been measured, and must not
/// read as one that was never throttled.
#[test]
fn thermal_throttling_needs_two_samples_and_a_kernel_that_reports_them() {
    assert_eq!(
        rate::derive_thermal(&unmeasured_snapshot(0), &thermal_snapshot(2_000, &[(0, 5)])),
        None,
        "the first sample never read the counters"
    );
    assert_eq!(
        rate::derive_thermal(&thermal_snapshot(0, &[(0, 5)]), &unmeasured_snapshot(2_000)),
        None,
        "and neither did the second"
    );
}

/// A counter that went backwards is that core's accounting reset. A
/// reboot resets every core, so every core drops out and the reading
/// disappears - rather than a reboot being reported as a cool interval.
#[test]
fn thermal_throttling_refuses_counters_that_went_backwards() {
    assert_eq!(
        rate::derive_thermal(
            &thermal_snapshot(0, &[(0, 9_000), (1, 9_000)]),
            &thermal_snapshot(2_000, &[(0, 12), (1, 5)]),
        ),
        None
    );
}

/// One core reset by hotplug takes only itself out of the comparison -
/// the other cores were sampled twice and still have an answer.
#[test]
fn a_single_core_reset_does_not_lose_the_other_cores() {
    let percent = rate::derive_thermal(
        &thermal_snapshot(0, &[(0, 9_000), (1, 100)]),
        &thermal_snapshot(2_000, &[(0, 3), (1, 400)]),
    )
    .expect("core 1 was sampled twice and went forwards");
    assert!((percent - 15.0).abs() < 0.01, "expected 15%, got {percent}");
}

/// Cores are matched by their own number, never by position. A core
/// appearing between samples has nothing to be differenced against, and
/// pairing it with whatever sat in its slot would compare two different
/// cores.
#[test]
fn a_core_absent_from_either_sample_is_not_differenced() {
    assert_eq!(
        rate::derive_thermal(
            &thermal_snapshot(0, &[(0, 100)]),
            &thermal_snapshot(2_000, &[(7, 900)]),
        ),
        None,
        "core 7 was never sampled before, and core 0 is gone"
    );
}

/// Two samples with no time between them divide by zero. `None`, not a
/// core that was never throttled.
#[test]
fn thermal_throttling_over_no_interval_is_no_reading() {
    assert_eq!(
        rate::derive_thermal(
            &thermal_snapshot(2_000, &[(0, 10)]),
            &thermal_snapshot(2_000, &[(0, 10)]),
        ),
        None
    );
}

/// More throttled milliseconds than there were milliseconds. The two
/// samples are not comparable - a suspended machine, or a counter read
/// across a kernel that reset it - and 400% is not a share of anything.
#[test]
fn thermal_throttling_refuses_more_throttling_than_interval() {
    assert_eq!(
        rate::derive_thermal(
            &thermal_snapshot(0, &[(0, 0)]),
            &thermal_snapshot(1_000, &[(0, 4_000)]),
        ),
        None
    );
}
