use std::collections::HashMap;

use crate::sample::Snapshot;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcRate {
    pub pid: u32,
    pub cpu_percent: f32,
    pub io_read_bytes_per_sec: f64,
    pub io_write_bytes_per_sec: f64,
}

/// Turns two raw samples into per-process rates. Pure: no I/O, no clock
/// reads - `SystemService::sample` is responsible for producing comparable
/// snapshots, this only does the arithmetic.
///
/// A pid present in `curr` but absent from the returned `Vec` has no rate
/// yet - either it wasn't in `prev` (first sample since it started), or
/// its counters went backwards (the pid was reused by a different process
/// between samples). The view renders `-` for those, never `0.0`.
pub fn derive(prev: &Snapshot, curr: &Snapshot, clock_ticks_per_sec: u64) -> Vec<ProcRate> {
    if curr.taken_at_ms <= prev.taken_at_ms {
        return Vec::new();
    }
    let elapsed_secs = (curr.taken_at_ms - prev.taken_at_ms) as f64 / 1000.0;

    let mut prev_by_pid = HashMap::new();
    for p in &prev.procs {
        prev_by_pid.insert(p.pid, p);
    }

    curr.procs
        .iter()
        .filter_map(|c| {
            let p = prev_by_pid.get(&c.pid)?;
            if c.cpu_ticks < p.cpu_ticks
                || c.io_read_bytes < p.io_read_bytes
                || c.io_write_bytes < p.io_write_bytes
            {
                return None;
            }
            let delta_ticks = c.cpu_ticks - p.cpu_ticks;
            let cpu_percent =
                (delta_ticks as f64 / clock_ticks_per_sec as f64 / elapsed_secs * 100.0) as f32;
            let io_read_bytes_per_sec = (c.io_read_bytes - p.io_read_bytes) as f64 / elapsed_secs;
            let io_write_bytes_per_sec =
                (c.io_write_bytes - p.io_write_bytes) as f64 / elapsed_secs;
            Some(ProcRate {
                pid: c.pid,
                cpu_percent,
                io_read_bytes_per_sec,
                io_write_bytes_per_sec,
            })
        })
        .collect()
}

/// How much of the interval between two samples the CPU spent doing
/// something, as a percentage.
///
/// `None` in every case where there is no answer rather than a zero one,
/// which is the whole reason this module exists:
///
/// * either sample lacks `cpu_times` - one tick is not a rate, and a
///   kernel whose `/proc/stat` could not be read has not been measured;
/// * the counters went backwards, which means the host rebooted between
///   ticks rather than that it idled;
/// * no ticks elapsed, which would divide by zero.
///
/// Idle time is the kernel's own, and `masys-systemd` decides what counts
/// as idle when it reads the line - the arithmetic here is a ratio and
/// takes no view.
pub fn derive_cpu(prev: &Snapshot, curr: &Snapshot) -> Option<f32> {
    let (before, after) = (prev.cpu_times?, curr.cpu_times?);
    let total = after.total_ticks.checked_sub(before.total_ticks)?;
    let idle = after.idle_ticks.checked_sub(before.idle_ticks)?;
    // A machine cannot have been idle for longer than the interval; if it
    // claims to have been, the two samples are not comparable.
    if total == 0 || idle > total {
        return None;
    }
    Some(((total - idle) as f64 / total as f64 * 100.0) as f32)
}

/// How much of the interval between two samples the worst-affected core
/// spent thermally throttled, as a percentage.
///
/// The largest per-core difference, never the difference of the largest.
/// Those are not the same number, and the second one is wrong in a way
/// that matters: the previous maximum is at least the worst core's own
/// previous reading, so differencing maxima can only under-report. One
/// core carrying an old throttle event then masks a different core
/// throttling now - measured, a machine spending 40% of the interval
/// throttled reported 0%, which is below every threshold and so produced
/// no finding at all. A tool whose whole thesis is that unmeasured must
/// not read as fine cannot ship that.
///
/// `None` on the same grounds `derive_cpu` refuses, for the same reason -
/// an unmeasured machine must not read as a cool one:
///
/// * either sample has no counters, which is every host whose kernel does
///   not account for throttling;
/// * no time elapsed, which would divide by zero;
/// * no core appears in both samples, so there is no pair to difference;
/// * the worst core throttled for longer than the interval, which is not
///   a share of anything. That is the suspended-machine case: the counter
///   is wall-clock and `taken_at_ms` is too, but a host resumed from
///   suspend can report the two disagreeing.
///
/// A core whose counter went backwards is skipped rather than refused.
/// Backwards means that core's accounting was reset - a reboot resets
/// every core, so every core is skipped and the reading correctly
/// disappears, while a single core re-added by hotplug takes only itself
/// out of the comparison. A core in one sample and not the other is
/// skipped for the same reason: keyed by cpu number precisely so that
/// two different cores are never differenced against each other.
pub fn derive_thermal(prev: &Snapshot, curr: &Snapshot) -> Option<f32> {
    let (before, after) = (
        prev.thermal_throttled_ms_by_core.as_ref()?,
        curr.thermal_throttled_ms_by_core.as_ref()?,
    );
    let elapsed = curr.taken_at_ms.checked_sub(prev.taken_at_ms)?;
    if elapsed == 0 {
        return None;
    }
    let worst = after
        .iter()
        .filter_map(|(cpu, now)| now.checked_sub(*before.get(cpu)?))
        .max()?;
    if worst > elapsed {
        return None;
    }
    Some((worst as f64 / elapsed as f64 * 100.0) as f32)
}

/// What a whole machine is moving, in each direction.
///
/// One type for network and storage, because the question is the same
/// one asked of two kinds of pipe - which is the argument the IO buffer
/// already makes by putting its interfaces beside its block devices.
///
/// `in`/`out` rather than rx/tx or read/write: for a disk, a read brings
/// bytes in and a write sends them out, so the pair carries across both
/// without either borrowing the other's vocabulary.
///
/// Deliberately not a [`DiskRate`]. `busy_percent` is a fraction of
/// wall-clock time on *one* device and means nothing added across
/// several, and IOPS answer a question the overview does not ask. A sum
/// that carried them would invite somebody to read them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Throughput {
    pub in_bytes_per_sec: f64,
    pub out_bytes_per_sec: f64,
}

impl Throughput {
    /// What every device in `rates` is moving, added up.
    ///
    /// A total over whatever the caller passes, which is how the same
    /// figure ends up in two places without being measured twice: the IO
    /// buffer derives the per-device rates once, shows them, and sums
    /// exactly the set it showed.
    pub fn of_disks(rates: &[(String, DiskRate)]) -> Self {
        rates
            .iter()
            .fold(Throughput::default(), |total, (_, rate)| Throughput {
                in_bytes_per_sec: total.in_bytes_per_sec + rate.read_bytes_per_sec,
                out_bytes_per_sec: total.out_bytes_per_sec + rate.write_bytes_per_sec,
            })
    }

    /// The same, for network links.
    ///
    /// The caller decides which links count, because loopback is not a
    /// network path and only the layer holding the interfaces knows
    /// which of them is one.
    pub fn of_nets(rates: &[(String, NetRate)]) -> Self {
        rates
            .iter()
            .fold(Throughput::default(), |total, (_, rate)| Throughput {
                in_bytes_per_sec: total.in_bytes_per_sec + rate.rx_bytes_per_sec,
                out_bytes_per_sec: total.out_bytes_per_sec + rate.tx_bytes_per_sec,
            })
    }
}

/// One device's throughput between two samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiskRate {
    pub read_bytes_per_sec: f64,
    pub write_bytes_per_sec: f64,
    pub reads_per_sec: f64,
    pub writes_per_sec: f64,
    /// Fraction of wall-clock time the device had a request in flight,
    /// as a percentage. Can exceed 100 on a device that services
    /// requests in parallel, which is why it is not clamped.
    pub busy_percent: f32,
}

/// One interface's throughput between two samples.
///
/// No error or drop rate: those counters are cumulative since boot and
/// the view reports them as totals, so a per-second figure would have no
/// consumer.
///
/// No packet rate either, for the same reason and only after it was
/// carried for a while anyway - the interface line shows byte rates,
/// errors and drops, and never had a column for packets per second. The
/// rule above was already written when the two packet fields were added
/// under it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetRate {
    pub rx_bytes_per_sec: f64,
    pub tx_bytes_per_sec: f64,
}

/// Turns two samples into per-interface rates, keyed by interface name.
///
/// Same contract as `derive_disks`: an interface absent from the result
/// has no rate yet - it was not in `prev`, or its counters went backwards
/// because it was removed and re-added.
pub fn derive_nets(prev: &Snapshot, curr: &Snapshot) -> Vec<(String, NetRate)> {
    if curr.taken_at_ms <= prev.taken_at_ms {
        return Vec::new();
    }
    let elapsed_secs = (curr.taken_at_ms - prev.taken_at_ms) as f64 / 1000.0;
    curr.interfaces
        .iter()
        .filter_map(|c| {
            let p = prev.interfaces.iter().find(|p| p.name == c.name)?;
            // All four counters, though only bytes are reported. The
            // packet counters are this guard's only reader, and they earn
            // the read: they reset at the same moment the byte counters
            // do, so an interface removed and re-added between samples
            // can leave bytes coincidentally higher while packets went
            // backwards. Two witnesses to the same event, and the cheaper
            // one is already parsed.
            if c.rx_bytes < p.rx_bytes
                || c.tx_bytes < p.tx_bytes
                || c.rx_packets < p.rx_packets
                || c.tx_packets < p.tx_packets
            {
                return None;
            }
            let per_sec = |curr: u64, prev: u64| (curr - prev) as f64 / elapsed_secs;
            Some((
                c.name.clone(),
                NetRate {
                    rx_bytes_per_sec: per_sec(c.rx_bytes, p.rx_bytes),
                    tx_bytes_per_sec: per_sec(c.tx_bytes, p.tx_bytes),
                },
            ))
        })
        .collect()
}

/// Turns two samples into per-device rates, keyed by device name.
///
/// Same contract as `derive`: a device absent from the result has no rate
/// yet - either it was not in `prev`, or its counters went backwards,
/// which happens when a device is removed and re-added.
pub fn derive_disks(prev: &Snapshot, curr: &Snapshot) -> Vec<(String, DiskRate)> {
    if curr.taken_at_ms <= prev.taken_at_ms {
        return Vec::new();
    }
    let elapsed_secs = (curr.taken_at_ms - prev.taken_at_ms) as f64 / 1000.0;
    let mut previous = HashMap::new();
    for disk in &prev.disks {
        previous.insert(disk.name.as_str(), disk);
    }

    curr.disks
        .iter()
        .filter_map(|c| {
            let p = previous.get(c.name.as_str())?;
            if c.read_sectors < p.read_sectors
                || c.write_sectors < p.write_sectors
                || c.io_ms < p.io_ms
            {
                return None;
            }
            // `/proc/diskstats` sectors are always 512 bytes.
            const SECTOR: f64 = 512.0;
            Some((
                c.name.clone(),
                DiskRate {
                    read_bytes_per_sec: (c.read_sectors - p.read_sectors) as f64 * SECTOR
                        / elapsed_secs,
                    write_bytes_per_sec: (c.write_sectors - p.write_sectors) as f64 * SECTOR
                        / elapsed_secs,
                    reads_per_sec: c.reads.saturating_sub(p.reads) as f64 / elapsed_secs,
                    writes_per_sec: c.writes.saturating_sub(p.writes) as f64 / elapsed_secs,
                    busy_percent: ((c.io_ms - p.io_ms) as f64 / (elapsed_secs * 1000.0) * 100.0)
                        as f32,
                },
            ))
        })
        .collect()
}
