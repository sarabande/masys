//! `/proc/[pid]/io`. Unreadable for any process masys does not own, which
//! on a normal host is most of them - see `read_io`'s caller.

use masys_domain::error::MasysError;

/// `(read_bytes, write_bytes)` - bytes that actually reached storage, not
/// bytes requested. `rchar`/`wchar` count syscall traffic served from page
/// cache, which does not predict disk pressure; `read_bytes`/`write_bytes`
/// do, which is why `masys_domain::sample::Proc` documents them that way.
pub fn parse_io(text: &str) -> Result<(u64, u64), MasysError> {
    let mut read = None;
    let mut write = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let parsed = || value.trim().parse::<u64>().map_err(|_| MasysError::System(format!("io: {key} is not a number")));
        match key {
            "read_bytes" => read = Some(parsed()?),
            "write_bytes" => write = Some(parsed()?),
            _ => {}
        }
    }
    match (read, write) {
        (Some(r), Some(w)) => Ok((r, w)),
        _ => Err(MasysError::System("io: missing read_bytes or write_bytes".into())),
    }
}
