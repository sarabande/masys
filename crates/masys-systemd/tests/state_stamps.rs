//! `StateChangeTimestamp` is the single most expensive thing a unit poll
//! does - one D-Bus round trip per unit, every poll, 277 of them measured
//! at 263 ms against a 2 s tick that cannot draw while it runs.
//!
//! It is also, by definition, the moment the unit entered the state it is
//! in. `ListUnits` already reports that state for free, so the timestamp
//! only has to be re-read when the state it describes has changed.

use masys_systemd::units::StateStamps;

#[test]
fn a_unit_in_the_same_state_keeps_its_timestamp() {
    let mut stamps = StateStamps::default();
    stamps.insert("sshd.service", "active", "running", 1_000);
    assert_eq!(stamps.get("sshd.service", "active", "running"), Some(1_000));
}

#[test]
fn a_changed_sub_state_invalidates_it() {
    let mut stamps = StateStamps::default();
    stamps.insert("sshd.service", "active", "running", 1_000);
    // active -> active, but running -> reload is still a state change and
    // systemd stamps it, so the cached value is stale.
    assert_eq!(stamps.get("sshd.service", "active", "reload"), None);
}

#[test]
fn a_changed_active_state_invalidates_it() {
    let mut stamps = StateStamps::default();
    stamps.insert("sshd.service", "active", "running", 1_000);
    assert_eq!(stamps.get("sshd.service", "failed", "failed"), None);
}

#[test]
fn a_unit_never_seen_has_nothing() {
    assert_eq!(StateStamps::default().get("nope.service", "active", "running"), None);
}

/// A host that churns transient units must not grow this map without
/// bound - the same rule `ColdCache` and `RestartHistory` follow.
#[test]
fn units_that_are_gone_are_dropped() {
    let mut stamps = StateStamps::default();
    stamps.insert("a.service", "active", "running", 1);
    stamps.insert("b.service", "active", "running", 2);
    stamps.retain_units(&std::collections::HashSet::from(["a.service"]));
    assert_eq!(stamps.get("a.service", "active", "running"), Some(1));
    assert_eq!(stamps.get("b.service", "active", "running"), None);
}
