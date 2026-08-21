//! Unit state over D-Bus.
//!
//! Two rules, both measured rather than assumed (see the design doc):
//! never call `GetAll` - the `Unit` interface carries ~100 properties,
//! several computed on demand, and costs 48x a targeted `Get` - and never
//! construct a `Proxy` per unit, which adds ~40% per call over
//! `Connection::call_method` on one long-lived connection.

use masys_domain::error::MasysError;
use masys_domain::unit::{ActiveState, Timer, UnitKind};
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

pub const DEST: &str = "org.freedesktop.systemd1";
pub const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
pub const MANAGER_IF: &str = "org.freedesktop.systemd1.Manager";
pub const UNIT_IF: &str = "org.freedesktop.systemd1.Unit";
pub const SERVICE_IF: &str = "org.freedesktop.systemd1.Service";
pub const TIMER_IF: &str = "org.freedesktop.systemd1.Timer";
pub const PROPS_IF: &str = "org.freedesktop.DBus.Properties";

/// `ListUnits`'s `a(ssssssouso)` row. Only four of the ten fields are
/// used, but the tuple must match the signature exactly to deserialize.
pub type ListedUnit = (String, String, String, String, String, String, OwnedObjectPath, u32, String, OwnedObjectPath);

pub fn active_state_of(raw: &str) -> ActiveState {
    match raw {
        "active" => ActiveState::Active,
        "reloading" => ActiveState::Reloading,
        "failed" => ActiveState::Failed,
        "activating" => ActiveState::Activating,
        "deactivating" => ActiveState::Deactivating,
        _ => ActiveState::Inactive,
    }
}

pub fn kind_of(name: &str) -> UnitKind {
    match name.rsplit_once('.').map(|(_, suffix)| suffix) {
        Some("service") => UnitKind::Service,
        Some("socket") => UnitKind::Socket,
        Some("target") => UnitKind::Target,
        Some("device") => UnitKind::Device,
        Some("mount") => UnitKind::Mount,
        Some("automount") => UnitKind::Automount,
        Some("swap") => UnitKind::Swap,
        Some("path") => UnitKind::Path,
        Some("timer") => UnitKind::Timer,
        Some("slice") => UnitKind::Slice,
        Some("scope") => UnitKind::Scope,
        _ => UnitKind::Other,
    }
}

/// systemd reports a unit file as `enabled` or `enabled-runtime`; every
/// other value (`disabled`, `static`, `masked`, `transient`, or empty for
/// a unit with no file at all) is not enabled.
pub fn is_enabled(unit_file_state: &str) -> bool {
    matches!(unit_file_state, "enabled" | "enabled-runtime")
}

/// Whether `ExecMainStatus` is worth fetching.
///
/// Only a failed unit has an exit code worth showing - a healthy service
/// reports 0, which `Unit::exit_code` documents as not an exit code worth
/// showing. It used to be read for every service and then discarded for
/// all but the failed ones: 137 of 138 round trips on this host, thrown
/// away, every poll.
pub fn wants_exit_code(active_state: ActiveState) -> bool {
    active_state == ActiveState::Failed
}

/// `StateChangeTimestamp` per unit, held against the state it describes.
///
/// This is the poll's dominant cost: one round trip per unit, every poll.
/// The value is by definition the moment the unit entered its current
/// state, and `ListUnits` already reports that state for free - so it only
/// needs re-reading when the state changes.
///
/// The gap this leaves is a transition away and back inside one poll,
/// which `ListUnits` cannot see: the cached stamp is then stale until the
/// next real transition. For services the restart counter closes it, since
/// a rise there invalidates the entry too. Nothing else restarts fast
/// enough for a two-second window to hide it.
#[derive(Debug, Default)]
pub struct StateStamps {
    entries: std::collections::HashMap<String, Stamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    active_state: String,
    sub_state: String,
    since_ms: u64,
}

impl StateStamps {
    /// The timestamp for `unit`, if it is still in the state that stamp
    /// was taken in.
    pub fn get(&self, unit: &str, active_state: &str, sub_state: &str) -> Option<u64> {
        let stamp = self.entries.get(unit)?;
        (stamp.active_state == active_state && stamp.sub_state == sub_state).then_some(stamp.since_ms)
    }

    pub fn insert(&mut self, unit: &str, active_state: &str, sub_state: &str, since_ms: u64) {
        let stamp = Stamp { active_state: active_state.to_string(), sub_state: sub_state.to_string(), since_ms };
        self.entries.insert(unit.to_string(), stamp);
    }

    /// Drops a unit's stamp, so the next poll re-reads it. Used when
    /// something other than the state pair says the unit moved.
    pub fn forget(&mut self, unit: &str) {
        self.entries.remove(unit);
    }

    /// Drops units that no longer exist, so a host that churns transient
    /// units does not grow this map without bound.
    pub fn retain_units(&mut self, live: &std::collections::HashSet<&str>) {
        self.entries.retain(|name, _| live.contains(name.as_str()));
    }
}

fn get(conn: &Connection, path: &OwnedObjectPath, iface: &str, prop: &str) -> Result<OwnedValue, MasysError> {
    conn.call_method(Some(DEST), path.as_str(), Some(PROPS_IF), "Get", &(iface, prop))
        .and_then(|reply| reply.body().deserialize())
        .map_err(|e| MasysError::System(format!("{prop}: {e}")))
}

pub fn get_string(conn: &Connection, path: &OwnedObjectPath, iface: &str, prop: &str) -> Result<String, MasysError> {
    String::try_from(get(conn, path, iface, prop)?).map_err(|e| MasysError::System(format!("{prop}: {e}")))
}

pub fn get_u64(conn: &Connection, path: &OwnedObjectPath, iface: &str, prop: &str) -> Result<u64, MasysError> {
    u64::try_from(get(conn, path, iface, prop)?).map_err(|e| MasysError::System(format!("{prop}: {e}")))
}

pub fn get_u32(conn: &Connection, path: &OwnedObjectPath, iface: &str, prop: &str) -> Result<u32, MasysError> {
    u32::try_from(get(conn, path, iface, prop)?).map_err(|e| MasysError::System(format!("{prop}: {e}")))
}

pub fn get_i32(conn: &Connection, path: &OwnedObjectPath, iface: &str, prop: &str) -> Result<i32, MasysError> {
    i32::try_from(get(conn, path, iface, prop)?).map_err(|e| MasysError::System(format!("{prop}: {e}")))
}

/// The properties that change rarely - `UnitFileState` only on
/// enable/disable/mask, `ControlGroup` not at all for a given unit.
/// Caching them and refreshing on a slow tick takes a 275-unit poll from
/// 961 property calls to 549, measured at 114 ms and 65 ms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColdProperties {
    pub enabled: bool,
    pub cgroup: Option<String>,
    pub slice: Option<String>,
    pub triggers: Vec<String>,
}

/// The cold properties for every live unit, plus the clock that decides
/// when to re-read them.
///
/// The expiry lives here rather than being driven from outside because
/// `App` takes ownership of the `SystemService`: once it is boxed into the
/// session, nothing above can call a refresh method on it. A cache that
/// cannot notice its own staleness would serve a unit's `enabled` from
/// process start until process exit.
///
/// Time arrives as a monotonic millisecond count rather than an `Instant`
/// so the expiry rule is testable with plain numbers instead of a sleep.
#[derive(Debug, Default)]
pub struct ColdCache {
    entries: std::collections::HashMap<String, ColdProperties>,
    refreshed_at_ms: u64,
}

impl ColdCache {
    /// Drops every entry if `ttl_ms` has passed, and reports whether it
    /// did. Expiring restarts the clock, so the next window is measured
    /// from this expiry rather than from process start - otherwise every
    /// call past the first ttl would clear the cache and the whole point
    /// of caching would be lost.
    pub fn expire_if_stale(&mut self, now_ms: u64, ttl_ms: u64) -> bool {
        if now_ms.saturating_sub(self.refreshed_at_ms) < ttl_ms {
            return false;
        }
        self.entries.clear();
        self.refreshed_at_ms = now_ms;
        true
    }

    pub fn get(&self, unit: &str) -> Option<&ColdProperties> {
        self.entries.get(unit)
    }

    pub fn insert(&mut self, unit: String, properties: ColdProperties) {
        self.entries.insert(unit, properties);
    }

    /// Drops units that no longer exist, so a host that churns transient
    /// units does not grow this map between expiries.
    pub fn retain_units(&mut self, live: &std::collections::HashSet<&str>) {
        self.entries.retain(|name, _| live.contains(name.as_str()));
    }
}

pub fn read_cold(conn: &Connection, path: &OwnedObjectPath, kind: UnitKind) -> ColdProperties {
    let is_service = kind == UnitKind::Service;
    ColdProperties {
        enabled: get_string(conn, path, UNIT_IF, "UnitFileState").map(|s| is_enabled(&s)).unwrap_or(false),
        cgroup: is_service.then(|| get_string(conn, path, SERVICE_IF, "ControlGroup").ok()).flatten().filter(|c| !c.is_empty()),
        // `Slice` is served by each unit type's own interface, not by
        // `Unit` - `sshd.service` answers on `...systemd1.Service`, and a
        // slice on `...systemd1.Slice`. A type that cannot be in a slice
        // has no such property, and the error is the answer.
        slice: sliceable_interface(kind).and_then(|iface| get_string(conn, path, iface, "Slice").ok()).filter(|s| !s.is_empty()),
        // `Triggers` lives on `Unit`, not on the per-type interface, so
        // one read covers timers, paths and activation sockets alike -
        // and answers empty for everything that starts nothing.
        triggers: get_strings(conn, path, UNIT_IF, "Triggers"),
    }
}

/// A `as` property, or empty when the unit does not have one.
pub fn get_strings(conn: &Connection, path: &OwnedObjectPath, iface: &str, prop: &str) -> Vec<String> {
    get(conn, path, iface, prop).ok().and_then(|value| Vec::<String>::try_from(value).ok()).unwrap_or_default()
}

/// The interface carrying `Slice`, for the unit types that can be in one.
///
/// `None` for the types that cannot: a target, timer, path or device is
/// not in the cgroup hierarchy at all, so asking would be one failed
/// round trip per unit per cache refresh, for an answer already known.
fn sliceable_interface(kind: UnitKind) -> Option<&'static str> {
    match kind {
        UnitKind::Service => Some(SERVICE_IF),
        UnitKind::Slice => Some("org.freedesktop.systemd1.Slice"),
        UnitKind::Scope => Some("org.freedesktop.systemd1.Scope"),
        UnitKind::Socket => Some("org.freedesktop.systemd1.Socket"),
        UnitKind::Mount => Some("org.freedesktop.systemd1.Mount"),
        UnitKind::Swap => Some("org.freedesktop.systemd1.Swap"),
        UnitKind::Target | UnitKind::Timer | UnitKind::Path | UnitKind::Device | UnitKind::Automount | UnitKind::Other => None,
    }
}

/// A `.timer` unit's schedule, or `None` for anything else.
///
/// Three extra property reads, and only for timers - this host has ten of
/// them against 276 units, so the cost is noise next to the poll it rides
/// along with. Both timestamps are microseconds since the epoch, and
/// systemd reports 0 rather than absent for "no next elapse" and "never
/// fired", which are genuinely different facts from a time of zero.
pub fn read_timer(conn: &Connection, path: &OwnedObjectPath) -> Option<Timer> {
    let at = |prop: &str| get_u64(conn, path, TIMER_IF, prop).ok().filter(|v| *v > 0).map(|usec| usec / 1000);
    Some(Timer {
        next_ms: at("NextElapseUSecRealtime"),
        last_ms: at("LastTriggerUSec"),
        activates: get_string(conn, path, TIMER_IF, "Unit").ok()?,
    })
}

/// The bus path for one unit, via `GetUnit`.
///
/// `units()` gets these free in the `ListUnits` reply, but a detail read
/// is triggered by a keypress and has no reply to hand - and asking for
/// one path is cheaper than re-listing 366 of them.
pub fn unit_path(conn: &Connection, unit: &str) -> Result<OwnedObjectPath, MasysError> {
    conn.call_method(Some(DEST), MANAGER_PATH, Some(MANAGER_IF), "GetUnit", &(unit))
        .and_then(|reply| reply.body().deserialize())
        .map_err(|e| MasysError::System(format!("GetUnit {unit}: {e}")))
}

/// One unit's on-demand properties.
///
/// Each is read independently and a failure yields `None` rather than
/// failing the lot: a `.target` has no `MainPID`, a `.device` no memory
/// accounting, and the error that says so is the answer, not a fault.
/// `MemoryCurrent` and friends also report `u64::MAX` for "not accounted",
/// which is filtered here so the view never renders 16 exabytes.
pub fn read_detail(conn: &Connection, path: &OwnedObjectPath) -> masys_domain::unit::UnitDetail {
    let counter = |iface: &str, prop: &str| get_u64(conn, path, iface, prop).ok().filter(|v| *v != u64::MAX);
    masys_domain::unit::UnitDetail {
        description: get_string(conn, path, UNIT_IF, "Description").ok().filter(|d| !d.is_empty()),
        // `success` on a healthy unit, and the reason on a broken one.
        result: get_string(conn, path, SERVICE_IF, "Result").ok().filter(|r| !r.is_empty()),
        documentation: get_strings(conn, path, UNIT_IF, "Documentation"),
        memory_peak_bytes: counter(SERVICE_IF, "MemoryPeak"),
        tasks_max: counter(SERVICE_IF, "TasksMax"),
        io_read_bytes: counter(SERVICE_IF, "IOReadBytes"),
        io_write_bytes: counter(SERVICE_IF, "IOWriteBytes"),
        ip_ingress_bytes: counter(SERVICE_IF, "IPIngressBytes"),
        ip_egress_bytes: counter(SERVICE_IF, "IPEgressBytes"),
        fragment_path: get_string(conn, path, UNIT_IF, "FragmentPath").ok().filter(|p| !p.is_empty()),
        // MainPID is 0 for a unit with no running process, which is a
        // different fact from a pid of zero and must not render as one.
        main_pid: get_u32(conn, path, SERVICE_IF, "MainPID").ok().filter(|pid| *pid > 0),
        tasks: counter(SERVICE_IF, "TasksCurrent"),
        memory_bytes: counter(SERVICE_IF, "MemoryCurrent"),
        cpu_nsec: counter(SERVICE_IF, "CPUUsageNSec"),
        cgroup: get_string(conn, path, SERVICE_IF, "ControlGroup").ok().filter(|c| !c.is_empty()),
    }
}
