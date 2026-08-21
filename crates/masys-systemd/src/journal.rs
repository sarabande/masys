//! The journal, read by spawning `journalctl -o json`.
//!
//! A subprocess rather than linking `libsystemd` and calling `sd_journal`
//! through FFI: a bounded, priority-filtered query measures 40-70 ms
//! against a 5 s cadence, which does not justify unsafe FFI and a system
//! library at link time.

use masys_domain::error::MasysError;
use masys_domain::journal::{Entry, Priority};
use serde::Deserialize;

/// journalctl's `-o json` fields, of which masys wants four. Every value
/// arrives as a *string*, including the numeric ones - `PRIORITY` is
/// `"3"`, not `3`.
#[derive(Debug, Deserialize)]
struct RawEntry {
    #[serde(rename = "__REALTIME_TIMESTAMP")]
    realtime_timestamp: Option<String>,
    #[serde(rename = "PRIORITY")]
    priority: Option<String>,
    #[serde(rename = "_SYSTEMD_UNIT")]
    unit: Option<String>,
    #[serde(rename = "MESSAGE")]
    message: Option<serde_json::Value>,
}

fn priority_of(raw: Option<&str>) -> Priority {
    match raw {
        Some("0") => Priority::Emergency,
        Some("1") => Priority::Alert,
        Some("2") => Priority::Critical,
        Some("3") => Priority::Error,
        Some("4") => Priority::Warning,
        Some("5") => Priority::Notice,
        Some("7") => Priority::Debug,
        // 6 and anything unexpected. Info is the kernel's own default for
        // a record with no PRIORITY field.
        _ => Priority::Info,
    }
}

/// `MESSAGE` is usually a string, but journalctl emits an **array of
/// byte values** when the message is not valid UTF-8 - a real case for
/// kernel records with embedded NULs. Rendering `[72, 105]` into the
/// Journal buffer would be gibberish, so those bytes are decoded.
fn message_of(value: Option<serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(text)) => text,
        Some(serde_json::Value::Array(bytes)) => {
            let raw: Vec<u8> = bytes.iter().filter_map(|b| b.as_u64()).map(|b| b as u8).collect();
            String::from_utf8_lossy(&raw).into_owned()
        }
        _ => String::new(),
    }
}

/// One `-o json` line. journalctl emits one complete JSON object per
/// line, so the caller parses line by line and a single malformed record
/// costs one entry rather than the whole query.
pub fn parse_entry(line: &str) -> Result<Entry, MasysError> {
    let raw: RawEntry = serde_json::from_str(line).map_err(|e| MasysError::System(format!("journal: {e}")))?;
    Ok(Entry {
        // Journal timestamps are microseconds since the Unix epoch;
        // `Entry::timestamp_ms` is milliseconds on that same clock, not
        // the monotonic one `Snapshot::taken_at_ms` uses.
        timestamp_ms: raw.realtime_timestamp.and_then(|t| t.parse::<u64>().ok()).unwrap_or(0) / 1000,
        unit: raw.unit,
        priority: priority_of(raw.priority.as_deref()),
        message: message_of(raw.message),
    })
}

/// Every parseable record in a `-o json` stream, skipping any line that
/// is not valid JSON. journalctl prints a trailing newline, so empty
/// lines are normal and not an error.
pub fn parse_stream(text: &str) -> Vec<Entry> {
    text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| parse_entry(l).ok()).collect()
}

/// systemd's `MESSAGE_ID` for "unit failed" - `SD_MESSAGE_UNIT_FAILED`.
///
/// Matching on a message id rather than grepping for "Failed " is what
/// makes this survive: the English text changes between systemd versions
/// and is translated, while the id is a stable contract.
///
/// Failures, deliberately, and not `SD_MESSAGE_UNIT_STARTED`. Seeding
/// restart history from *starts* looks right and is badly wrong: a timer
/// firing every five minutes produces twelve starts an hour, and this
/// host reported `smart-metrics.service - 12 restarts in 1h` as flapping
/// the moment the backfill was switched on. In the same 24 hours it
/// logged 188 starts for that unit and exactly one failure, for the unit
/// that is genuinely broken. `NRestarts` - what the live accumulator
/// counts - means "restarted automatically after failing", and this is
/// the record that matches it.
pub const UNIT_FAILED: &str = "be02cf6855d2428ba40df7e9d022f03d";

/// `(unit, unix_ms)` for every unit record in a `-o json` stream that
/// names a unit.
///
/// This is what lets restart history be recovered for time before masys
/// was running. systemd exposes only `NRestarts`, a count with no
/// timestamps, so the adapter otherwise has to watch that count rise and
/// cannot know anything about the past - which meant flapping could not
/// fire until masys had been up for a full window.
pub fn parse_unit_events(text: &str) -> Vec<(String, u64)> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let raw: serde_json::Value = serde_json::from_str(line).ok()?;
            // `UNIT` is set by pid 1 on its own records; `_SYSTEMD_UNIT`
            // is the sender's own unit, which for these is init.scope.
            let unit = raw.get("UNIT")?.as_str()?.to_string();
            let stamp: u64 = raw.get("__REALTIME_TIMESTAMP")?.as_str()?.parse().ok()?;
            Some((unit, stamp / 1000))
        })
        .collect()
}
