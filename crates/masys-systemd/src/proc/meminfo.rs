//! `/proc/meminfo` and `/proc/swaps` - together, one `Memory`.

use masys_domain::error::MasysError;
use masys_domain::sample::Memory;

fn kb_field(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .and_then(|value| value.split_whitespace().next()?.parse::<u64>().ok())
        .map(|kb| kb * 1024)
}

/// zram fullness as a percentage of its configured size, summed across
/// every `/dev/zram*` row. `None` when the host has no zram swap at all,
/// which `masys_domain::sample::Memory` documents as a different fact
/// from memory not having been sampled.
///
/// `/proc/swaps` columns are `Filename Type Size Used Priority`, with
/// Size and Used in 1 KiB units - the ratio is unitless, so no conversion
/// is needed.
pub fn parse_swaps_zram_percent(text: &str) -> Option<f32> {
    let (mut size, mut used) = (0u64, 0u64);
    for line in text.lines().skip(1) {
        let mut columns = line.split_whitespace();
        let Some(filename) = columns.next() else { continue };
        if !filename.starts_with("/dev/zram") {
            continue;
        }
        let mut numbers = columns.skip(1).filter_map(|c| c.parse::<u64>().ok());
        let (Some(s), Some(u)) = (numbers.next(), numbers.next()) else { continue };
        size += s;
        used += u;
    }
    (size > 0).then(|| used as f32 * 100.0 / size as f32)
}

/// `used` is `MemTotal - MemAvailable`, not `MemTotal - MemFree`.
/// `MemFree` counts reclaimable page cache as used and reads ~85% on an
/// idle machine; `MemAvailable` is the kernel's own estimate of what a new
/// allocation could obtain, which is what an operator means by "used".
pub fn parse_meminfo(meminfo: &str, swaps: &str) -> Result<Memory, MasysError> {
    let total = kb_field(meminfo, "MemTotal").ok_or_else(|| MasysError::System("meminfo: no MemTotal".into()))?;
    let available = kb_field(meminfo, "MemAvailable").ok_or_else(|| MasysError::System("meminfo: no MemAvailable".into()))?;
    Ok(Memory {
        used_bytes: total.saturating_sub(available),
        total_bytes: total,
        swap_free_bytes: kb_field(meminfo, "SwapFree").unwrap_or(0),
        zram_percent: parse_swaps_zram_percent(swaps),
    })
}
