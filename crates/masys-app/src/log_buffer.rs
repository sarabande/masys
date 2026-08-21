//! The log view: one unit's journal, scrollable and searchable.
//!
//! Always scoped to a unit. There used to be a whole-system tail and a
//! boot list here, and both were dropped: "what happened on this machine"
//! is a different question from "what happened to this unit", and only the
//! second one has a row you can put a cursor on. What is left is the view
//! `l` opens, and it is reached no other way than by naming a unit first.
//!
//! An open unit in the systemd view shows what `systemctl status` leads
//! with, which answers "what is it doing". This is where you go to read
//! what it said.

use masys_domain::journal::Entry;
use masys_view::{Node, SectionKind};

/// How many entries the view holds. A cap rather than everything the unit
/// has ever logged: a busy unit can emit thousands of lines, and a buffer
/// that long is slower to build than it is useful to read.
pub const TAIL: usize = 2_000;

/// Which local day a timestamp falls in, as a count of days since the
/// epoch.
///
/// `div_euclid` rather than `/`, so a pre-1970 timestamp floors to the
/// day it is *in* rather than truncating toward the epoch.
fn local_day(timestamp_ms: u64, utc_offset_secs: i32) -> i64 {
    (timestamp_ms as i64 / 1000 + utc_offset_secs as i64).div_euclid(86_400)
}

/// `YYYY-MM-DD` for a day number, by civil-from-days.
///
/// Pure arithmetic rather than `libc::localtime_r`: the offset has
/// already been applied, and this crate reads no environment - which is
/// what keeps its whole test suite free of a `TZ` the machine happens to
/// be set to. The algorithm is Howard Hinnant's, shifting the epoch to
/// 0000-03-01 so that leap day lands at the end of a cycle and every
/// other month has a fixed length.
fn day_label(day: i64) -> String {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day_of_month = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day_of_month:02}")
}

/// One unit's log, grouped into a section per local calendar day.
///
/// A real `Node::SectionHeader` rather than a divider drawn at the last
/// moment, which buys the view three things it would otherwise need code
/// for: `jump_section` matches any section header, so `n` and `p` move
/// day to day; `tab` folds a day; and the cursor can rest on one.
///
/// `newest_first` reverses *both* levels - the days, and the lines within
/// each day. Half a reversal is a bug rather than a mode.
///
/// The cap is applied to the newest entries and never to the displayed
/// order: this always holds the last `TAIL` of what the unit said, and
/// direction decides only how they are shown. A sort key that quietly
/// changed the window would be a different feature wearing this one's
/// clothes.
///
/// A unit that has logged nothing says so. This view is the answer to a
/// direct request for one unit's log, and replying with a blank screen
/// looks like the key failed rather than like the unit is quiet.
pub fn build_log_rows(
    entries: &[Entry],
    unit: Option<&str>,
    utc_offset_secs: i32,
    newest_first: bool,
    collapsed: &std::collections::HashSet<String>,
) -> Vec<Node> {
    let Some(unit) = unit else {
        return Vec::new();
    };
    if entries.is_empty() {
        return vec![Node::SectionHeader { title: format!("{unit} - nothing in the journal"), kind: SectionKind::Journal, count: None }];
    }

    let mut ordered: Vec<&Entry> = entries.iter().collect();
    ordered.sort_by_key(|e| e.timestamp_ms);
    // Oldest-first through the cap, so the newest `TAIL` are the ones
    // kept whichever way they are about to be read.
    let mut kept: Vec<&Entry> = ordered.into_iter().rev().take(TAIL).rev().collect();
    if newest_first {
        kept.reverse();
    }

    let mut rows: Vec<Node> = Vec::with_capacity(kept.len() + 4);
    let mut open_day: Option<i64> = None;
    let mut count_at = 0usize;
    let mut shut = false;
    let mut counted = 0u32;
    for entry in kept {
        let day = local_day(entry.timestamp_ms, utc_offset_secs);
        if open_day != Some(day) {
            // The count is not known until the day ends, so the header
            // goes in blank and is filled once the next one starts.
            if open_day.is_some() {
                set_count(&mut rows, count_at, counted);
                rows.push(Node::Spacer);
            }
            counted = 0;
            count_at = rows.len();
            rows.push(Node::SectionHeader { title: day_label(day), kind: SectionKind::Journal, count: Some(0) });
            open_day = Some(day);
            shut = collapsed.contains(&day_label(day));
        }
        // Counted whether or not it is drawn: a folded day keeps its
        // heading and its count, which is most of what a folded section
        // is still there to say.
        counted += 1;
        if shut {
            continue;
        }
        rows.push(Node::JournalEntry(entry.clone()));
    }
    if open_day.is_some() {
        set_count(&mut rows, count_at, counted);
    }
    rows
}

/// Fills in the header at `index` with how many entries the day held.
///
/// Counted as they go by rather than recovered from the rows emitted: a
/// folded day emits none, and `12 entries` is most of what its heading is
/// still there to say.
fn set_count(rows: &mut [Node], index: usize, entries: u32) {
    if let Some(Node::SectionHeader { count, .. }) = rows.get_mut(index) {
        *count = Some(entries);
    }
}
