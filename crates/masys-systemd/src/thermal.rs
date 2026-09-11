//! What the thermal governor has cost this machine, from
//! `/sys/devices/system/cpu/cpu*/thermal_throttle`.
//!
//! Beside [`crate::smart`] rather than under `proc`, and for the same
//! reason: both ask the hardware how it is rather than asking the kernel
//! what it is running.
//!
//! **Why not the temperature.** `/sys/class/thermal` is the obvious
//! source and it is the wrong one. Reading it means comparing a
//! temperature against a trip point to decide whether the machine is in
//! trouble, and on the host this was written against both trip points
//! read `-274000` - a millidegree value below absolute zero, which is
//! how that interface spells "unset". A `temp >= trip` test would have
//! compared 74000 against -274000 and reported a throttled machine
//! forever. The throttle counters say what actually happened instead of
//! what a threshold implies, and they need no view about what
//! temperature is too hot for a particular piece of silicon.

use std::collections::BTreeMap;
use std::path::Path;

/// The counter, in whatever directory holds one.
///
/// `core_throttle_total_time_ms` rather than `core_throttle_count`: a
/// count says throttling happened at some point since boot, which is
/// true of most laptops and stays true forever. Milliseconds can be
/// differenced against the interval to say what it is costing *now*,
/// which is the only form of this reading worth a row.
const COUNTER: &str = "core_throttle_total_time_ms";

/// Each core's cumulative throttled milliseconds, keyed by cpu number,
/// or `None` where this kernel accounts for none.
pub fn throttled_ms_by_core() -> Option<BTreeMap<u32, u64>> {
    throttled_ms_by_core_under(Path::new("/sys/devices/system/cpu"))
}

/// Every core that answers, keyed by its own cpu number.
///
/// `None` rather than an empty map where nothing answers, which is every
/// non-x86 host: an empty map would say masys looked and found a machine
/// with no cores, and `derive_thermal` would have to invent the
/// distinction back. A core that is present but unreadable is left out
/// rather than recorded as zero - a number masys never read must not be
/// offered as one it did.
///
/// Keyed rather than collected in order, because a host can gain and
/// lose cores between samples and the caller differences these by core.
/// Two samples matched up by position would difference two different
/// cores the moment a core is hotplugged.
///
/// This returned a single maximum until 2026-09-05, which was wrong by
/// arithmetic rather than by taste: differencing two maxima can only
/// under-report, so one core's old throttle event masked another core's
/// current throttling entirely. See `rate::derive_thermal`.
pub fn throttled_ms_by_core_under(cpu_root: &Path) -> Option<BTreeMap<u32, u64>> {
    let cores: BTreeMap<u32, u64> = std::fs::read_dir(cpu_root)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let cpu = cpu_number(&entry.file_name().to_string_lossy())?;
            let counter =
                std::fs::read_to_string(entry.path().join("thermal_throttle").join(COUNTER))
                    .ok()?;
            Some((cpu, parse_counter(&counter)?))
        })
        .collect();
    (!cores.is_empty()).then_some(cores)
}

/// The number in a `cpuN` directory name.
///
/// `None` for everything else in that directory - `cpufreq`, `cpuidle`,
/// `power` and the rest are siblings of the cores and parse as nothing,
/// which is what keeps them out without a list of names to maintain.
fn cpu_number(name: &str) -> Option<u32> {
    name.strip_prefix("cpu")?.parse().ok()
}

/// One counter file's contents.
///
/// `None` for anything that is not a number, rather than a zero: these
/// files are written by the kernel and a value masys cannot read means
/// it has not understood the interface, not that the core was never
/// throttled.
fn parse_counter(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}
