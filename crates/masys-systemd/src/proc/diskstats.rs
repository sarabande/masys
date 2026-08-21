//! `/proc/diskstats` - per-device IO counters, on every Linux since 2.6.

use masys_domain::sample::Disk;

/// Every real block device's counters.
///
/// Partitions are dropped in favour of whole devices. `sda`, `sda1` and
/// `sda2` all appear in the file and their counters overlap, so listing
/// all three would show the same writes two or three times over. A
/// partition is recognised by its name ending in a digit *and* a
/// whole-device prefix existing - `sda1` is a partition of `sda`, while
/// `zram0` and `nvme0n1` are devices in their own right despite ending in
/// one.
///
/// Fields, in order: major, minor, name, reads completed, reads merged,
/// sectors read, ms reading, writes completed, writes merged, sectors
/// written, ms writing, IOs in flight, ms doing IO, weighted ms. Kernels
/// since 4.18 append discard and flush statistics, which are ignored -
/// reading by index from the left is what makes that forward-compatible.
pub fn parse_diskstats(text: &str) -> Vec<Disk> {
    let rows: Vec<Vec<&str>> = text.lines().map(|line| line.split_whitespace().collect()).filter(|f: &Vec<&str>| f.len() >= 14).collect();
    let devices: Vec<&str> = rows.iter().map(|f| f[2]).collect();

    rows.iter()
        .filter(|fields| !is_partition(fields[2], &devices))
        // A device that has never done any IO since boot is not
        // interesting and there are usually a lot of them: this host
        // carries eight unused `loop` devices, which would fill the
        // buffer with rows that can only ever read zero.
        .filter(|fields| fields[3] != "0" || fields[7] != "0")
        .map(|fields| {
            let num = |at: usize| fields.get(at).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            Disk {
                name: fields[2].to_string(),
                reads: num(3),
                read_sectors: num(5),
                writes: num(7),
                write_sectors: num(9),
                in_flight: num(11),
                io_ms: num(12),
            }
        })
        .collect()
}

/// Whether `name` is a partition of some other device in the same file.
///
/// The digit-suffix test alone is wrong - `zram0` and `nvme0n1` end in a
/// digit and are whole devices - so it only counts as a partition when
/// the device it would belong to is also present.
fn is_partition(name: &str, devices: &[&str]) -> bool {
    let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if trimmed == name {
        return false;
    }
    // `nvme0n1p1` is a partition of `nvme0n1`; the `p` is part of the
    // partition suffix rather than the device name.
    let parent = trimmed.strip_suffix('p').unwrap_or(trimmed);
    devices.iter().any(|d| *d == trimmed || *d == parent)
}
