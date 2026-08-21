//! `/proc/[pid]/status` - the two fields `/proc/[pid]/stat` does not carry
//! in a directly usable form.

use masys_domain::error::MasysError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawStatus {
    pub threads: u32,
    /// `VmRSS` in bytes. `stat`'s field 24 reports the same figure in
    /// pages; this one is preferred because it needs no page-size lookup
    /// and is what `/proc` itself calls RSS.
    pub rss_bytes: u64,
}

pub fn parse_status(text: &str) -> Result<RawStatus, MasysError> {
    let mut threads = None;
    let mut rss_bytes = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        match key {
            "Threads" => {
                threads = value.trim().parse().ok();
            }
            "VmRSS" => {
                // "    7720 kB" - always kB, per Documentation/filesystems/proc.rst.
                let number = value.split_whitespace().next().unwrap_or_default();
                rss_bytes = number.parse::<u64>().ok().map(|kb| kb * 1024);
            }
            _ => {}
        }
    }
    Ok(RawStatus {
        threads: threads.ok_or_else(|| MasysError::System("status: no Threads".into()))?,
        // A kernel thread has no VmRSS line at all - zero, not an error.
        rss_bytes: rss_bytes.unwrap_or(0),
    })
}
