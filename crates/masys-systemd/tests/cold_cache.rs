use masys_systemd::units::{ColdCache, ColdProperties};

const TTL: u64 = 60_000;

fn props(enabled: bool) -> ColdProperties {
    ColdProperties { enabled, cgroup: None, slice: None, triggers: Vec::new() }
}

#[test]
fn an_entry_is_reused_until_the_ttl_expires() {
    let mut cache = ColdCache::default();
    cache.expire_if_stale(0, TTL);
    cache.insert("a.service".to_string(), props(true));

    cache.expire_if_stale(TTL - 1, TTL);
    assert_eq!(cache.get("a.service"), Some(&props(true)), "still fresh one millisecond short of the ttl");
}

/// The reason this exists: `App` takes ownership of the `SystemService`, so
/// nothing above can call a refresh method. The cache has to notice its own
/// staleness or `enabled` would be wrong until the process restarts.
#[test]
fn the_whole_cache_is_dropped_once_the_ttl_passes() {
    let mut cache = ColdCache::default();
    cache.expire_if_stale(0, TTL);
    cache.insert("a.service".to_string(), props(true));

    assert!(cache.expire_if_stale(TTL, TTL), "reaching the ttl expires the cache");
    assert_eq!(cache.get("a.service"), None, "the entry is gone and will be re-read");
}

#[test]
fn expiring_restarts_the_clock_rather_than_expiring_every_call() {
    let mut cache = ColdCache::default();
    cache.expire_if_stale(0, TTL);
    cache.insert("a.service".to_string(), props(true));

    assert!(cache.expire_if_stale(TTL, TTL));
    cache.insert("a.service".to_string(), props(false));
    assert!(!cache.expire_if_stale(TTL + 1, TTL), "the ttl runs from the last expiry, not from zero");
    assert_eq!(cache.get("a.service"), Some(&props(false)));
}

/// Units that vanish must not accumulate between expiries.
#[test]
fn units_that_disappear_are_forgotten() {
    let mut cache = ColdCache::default();
    cache.expire_if_stale(0, TTL);
    cache.insert("gone.service".to_string(), props(true));
    cache.insert("kept.service".to_string(), props(true));

    cache.retain_units(&std::collections::HashSet::from(["kept.service"]));
    assert_eq!(cache.get("gone.service"), None);
    assert_eq!(cache.get("kept.service"), Some(&props(true)));
}
