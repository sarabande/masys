//! `/proc/loadavg` and `/proc/uptime` - two one-line files.

use masys_domain::error::MasysError;
use masys_domain::sample::LoadAverage;

/// `1.37 0.50 0.28 1/1340 1758249` - the trailing running/total and last
/// pid are of no interest to any buffer.
pub fn parse_loadavg(text: &str) -> Result<LoadAverage, MasysError> {
    let mut fields = text.split_whitespace().filter_map(|f| f.parse::<f32>().ok());
    match (fields.next(), fields.next(), fields.next()) {
        (Some(one), Some(five), Some(fifteen)) => Ok(LoadAverage { one, five, fifteen }),
        _ => Err(MasysError::System("loadavg: fewer than three averages".into())),
    }
}

/// `85356.39 429938.13` - uptime seconds, then idle seconds summed over
/// all cores. Only the first is wanted, truncated to whole seconds.
pub fn parse_uptime(text: &str) -> Result<u64, MasysError> {
    text.split_whitespace()
        .next()
        .and_then(|f| f.parse::<f64>().ok())
        .map(|seconds| seconds as u64)
        .ok_or_else(|| MasysError::System("uptime: no leading seconds field".into()))
}
