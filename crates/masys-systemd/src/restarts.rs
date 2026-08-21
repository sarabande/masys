//! Restart history, which systemd cannot supply.
//!
//! `masys_domain::triage::flapping_units` filters
//! `Unit::restart_timestamps_ms` against a sliding window. systemd exposes
//! only `NRestarts` - a count, monotonic since the unit was loaded - and
//! `StateChangeTimestamp`. There is no list of restart times on the bus,
//! so the adapter accumulates one by watching the count rise between
//! polls.

use std::collections::HashMap;

/// One unit's observed restarts. `last_seen` is the `NRestarts` value at
/// the previous poll, which is what makes a rise detectable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct History {
    last_seen: u32,
    stamps: Vec<u64>,
}

#[derive(Debug, Default)]
pub struct RestartHistory {
    units: HashMap<String, History>,
    /// Start times recovered from the journal before any poll happened,
    /// held until the unit is first seen on the bus.
    ///
    /// They cannot go straight into `units`, because the first `observe`
    /// is what establishes the `NRestarts` baseline. An entry already
    /// present there would make that first reading look like a rise from
    /// zero and append one phantom timestamp per restart the unit had
    /// ever had.
    seeded: HashMap<String, Vec<u64>>,
}

impl RestartHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Primes a unit's history from the journal, for time before masys
    /// was watching. Applied when that unit is first observed.
    pub fn seed(&mut self, unit: &str, stamps: Vec<u64>) {
        if stamps.is_empty() {
            return;
        }
        self.seeded.entry(unit.to_string()).or_default().extend(stamps);
    }

    /// Whether `n_restarts` is higher than the last poll's reading.
    ///
    /// Asked before `observe`, which consumes the rise. A unit not seen
    /// before has no previous reading and so has not risen - its first
    /// sighting is a baseline, the same rule `observe` follows.
    pub fn count_rose(&self, unit: &str, n_restarts: u32) -> bool {
        self.units.get(unit).is_some_and(|history| n_restarts > history.last_seen)
    }

    /// Records this poll's `NRestarts` for `unit` and returns every
    /// restart timestamp still inside `window_ms`.
    ///
    /// The first sighting of a unit records no timestamps of its own,
    /// however high its count: those restarts happened before masys was
    /// watching, and dating them to *now* would report a long-stable unit
    /// as flapping the moment masys starts. `seed` is how the real times
    /// get there instead, recovered from the journal.
    ///
    /// A count that *falls* means the unit was reloaded or the daemon
    /// restarted, so the counter reset; re-baseline rather than treating
    /// the drop as negative restarts.
    pub fn observe(&mut self, unit: &str, n_restarts: u32, now_ms: u64, window_ms: u64) -> Vec<u64> {
        let history = match self.units.get_mut(unit) {
            Some(existing) => existing,
            None => {
                // First sighting: `n_restarts` is a baseline, not a rise,
                // so it contributes no timestamps of its own. Whatever
                // the journal recovered stands in for the past instead.
                let mut stamps = self.seeded.remove(unit).unwrap_or_default();
                let cutoff = now_ms.saturating_sub(window_ms);
                stamps.retain(|&t| t >= cutoff);
                stamps.sort_unstable();
                self.units.insert(unit.to_string(), History { last_seen: n_restarts, stamps: stamps.clone() });
                return stamps;
            }
        };
        if n_restarts < history.last_seen {
            history.last_seen = n_restarts;
        } else {
            // One timestamp per restart observed, so three restarts
            // between two polls count as three, not one.
            for _ in history.last_seen..n_restarts {
                history.stamps.push(now_ms);
            }
            history.last_seen = n_restarts;
        }
        let cutoff = now_ms.saturating_sub(window_ms);
        history.stamps.retain(|&t| t >= cutoff);
        history.stamps.clone()
    }

    /// Drops units that no longer exist, so a host that creates many
    /// short-lived transient units does not grow this map without bound.
    pub fn retain_units(&mut self, live: &std::collections::HashSet<&str>) {
        self.units.retain(|name, _| live.contains(name.as_str()));
        self.seeded.retain(|name, _| live.contains(name.as_str()));
    }
}
