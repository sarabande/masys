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
            if c.cpu_ticks < p.cpu_ticks || c.io_read_bytes < p.io_read_bytes || c.io_write_bytes < p.io_write_bytes {
                return None;
            }
            let delta_ticks = c.cpu_ticks - p.cpu_ticks;
            let cpu_percent = (delta_ticks as f64 / clock_ticks_per_sec as f64 / elapsed_secs * 100.0) as f32;
            let io_read_bytes_per_sec = (c.io_read_bytes - p.io_read_bytes) as f64 / elapsed_secs;
            let io_write_bytes_per_sec = (c.io_write_bytes - p.io_write_bytes) as f64 / elapsed_secs;
            Some(ProcRate { pid: c.pid, cpu_percent, io_read_bytes_per_sec, io_write_bytes_per_sec })
        })
        .collect()
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetRate {
    pub rx_bytes_per_sec: f64,
    pub tx_bytes_per_sec: f64,
    pub rx_packets_per_sec: f64,
    pub tx_packets_per_sec: f64,
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
            if c.rx_bytes < p.rx_bytes || c.tx_bytes < p.tx_bytes || c.rx_packets < p.rx_packets || c.tx_packets < p.tx_packets {
                return None;
            }
            let per_sec = |curr: u64, prev: u64| (curr - prev) as f64 / elapsed_secs;
            Some((
                c.name.clone(),
                NetRate {
                    rx_bytes_per_sec: per_sec(c.rx_bytes, p.rx_bytes),
                    tx_bytes_per_sec: per_sec(c.tx_bytes, p.tx_bytes),
                    rx_packets_per_sec: per_sec(c.rx_packets, p.rx_packets),
                    tx_packets_per_sec: per_sec(c.tx_packets, p.tx_packets),
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
            if c.read_sectors < p.read_sectors || c.write_sectors < p.write_sectors || c.io_ms < p.io_ms {
                return None;
            }
            // `/proc/diskstats` sectors are always 512 bytes.
            const SECTOR: f64 = 512.0;
            Some((
                c.name.clone(),
                DiskRate {
                    read_bytes_per_sec: (c.read_sectors - p.read_sectors) as f64 * SECTOR / elapsed_secs,
                    write_bytes_per_sec: (c.write_sectors - p.write_sectors) as f64 * SECTOR / elapsed_secs,
                    reads_per_sec: c.reads.saturating_sub(p.reads) as f64 / elapsed_secs,
                    writes_per_sec: c.writes.saturating_sub(p.writes) as f64 / elapsed_secs,
                    busy_percent: ((c.io_ms - p.io_ms) as f64 / (elapsed_secs * 1000.0) * 100.0) as f32,
                },
            ))
        })
        .collect()
}
