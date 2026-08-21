//! The real `SystemService`: unit state over D-Bus, `/proc` sampling, the
//! journal, and the runtime mutations masys performs.
//!
//! # Documented limitations
//!
//! Three places this adapter knowingly reports something other than
//! ground truth, each forced by what an unprivileged process can observe:
//!
//! 1. **IO counters understate.** `/proc/[pid]/io` needs same-uid or
//!    `CAP_SYS_PTRACE`, so processes masys cannot read report zero IO
//!    rather than "unknown" - 195 of 336 processes on the development
//!    host. `top` behaves the same way.
//! 2. **Flapping has a warm-up.** systemd exposes a restart *count*, not
//!    restart times, so [`restarts::RestartHistory`] accumulates them by
//!    watching the count rise. Flapping cannot fire until masys has run
//!    for a full window, and a restart storm predating launch is invisible.
//! 3. **Cold properties can be stale** for up to one slow-tick interval:
//!    a unit enabled by something other than masys briefly shows its old
//!    `enabled`.

pub mod actions;
pub mod journal;
pub mod machine;
pub mod proc;
pub mod restarts;
pub mod sd_journal;
pub mod units;

use std::cell::RefCell;
use std::time::Instant;

use masys_domain::error::MasysError;
use masys_domain::journal::Entry;
use masys_domain::sample::{OomKill, Snapshot, SystemState};
use masys_domain::service::{IoNiceClass, Signal, SystemService};
use masys_domain::unit::Unit;
use zbus::blocking::Connection;
use zbus::zvariant::OwnedObjectPath;

use units::ColdCache;

/// How long a unit's rarely-changing properties are trusted before being
/// re-read. Sixty seconds against a five-second unit poll means the extra
/// round-trips are paid on one poll in twelve; the design measured the
/// difference as 114 ms versus 70 ms.
const COLD_TTL_MS: u64 = 60_000;

pub fn system_state_of(raw: &str) -> SystemState {
    match raw {
        "running" => SystemState::Running,
        "degraded" => SystemState::Degraded,
        "maintenance" => SystemState::Maintenance,
        "initializing" | "starting" => SystemState::Starting,
        "stopping" => SystemState::Stopping,
        _ => SystemState::Offline,
    }
}

pub struct SystemdService {
    conn: Connection,
    /// `Snapshot::taken_at_ms` is monotonic, because only deltas between
    /// two snapshots feed `masys_domain::rate::derive` and an NTP step
    /// would otherwise produce a garbage CPU%. Every *other* timestamp in
    /// the domain is Unix milliseconds. Conflating the two is the most
    /// likely bug in this crate, so the origin lives here rather than
    /// being re-derived at each call site.
    origin: Instant,
    boot_secs: u64,
    /// An open cursor over one unit's journal, kept across ticks so that
    /// following costs a non-blocking check rather than re-opening the
    /// journal four times a second. `None` where `libsystemd` could not
    /// be loaded, which is when the subprocess path runs instead.
    reader: std::cell::RefCell<Option<sd_journal::Reader>>,
    /// Read once: none of it changes while masys runs.
    machine: masys_domain::sample::Machine,
    clock_ticks: u64,
    /// `SystemService` takes `&self` throughout, so the two caches need
    /// interior mutability. Single-threaded by the design's event-loop
    /// model, so `RefCell` rather than a lock.
    restarts: RefCell<restarts::RestartHistory>,
    cold: RefCell<ColdCache>,
    /// `StateChangeTimestamp` per unit, held against the state it
    /// describes - see `units::StateStamps` for why that is sound.
    stamps: RefCell<units::StateStamps>,
    flapping_window_ms: u64,
}

impl SystemdService {
    /// `flapping_window_ms` should match the `Thresholds` the app runs
    /// with: it is how far back restart history is retained, and pruning
    /// tighter than the triage window would hide real flapping.
    pub fn connect(flapping_window_ms: u64) -> Result<Self, MasysError> {
        let conn = Connection::system().map_err(|e| MasysError::System(format!("system bus: {e}")))?;
        let proc_stat = std::fs::read_to_string("/proc/stat").map_err(MasysError::Io)?;
        Ok(Self {
            conn,
            origin: Instant::now(),
            boot_secs: proc::boot_time_secs(&proc_stat)?,
            machine: machine::read_machine(),
            clock_ticks: proc::clock_ticks_per_second(),
            restarts: RefCell::new(Self::recovered_history(flapping_window_ms)),
            cold: RefCell::new(ColdCache::default()),
            stamps: RefCell::new(units::StateStamps::default()),
            reader: RefCell::new(None),
            flapping_window_ms,
        })
    }

    /// An open cursor over `unit`'s journal, opening one if the last was
    /// for a different unit.
    ///
    /// At most one is ever open, because at most one log is ever on
    /// screen. Switching units closes the old cursor by replacing it,
    /// which is what `Reader`'s `Drop` is for.
    ///
    /// `None` means `libsystemd` could not be loaded and every caller
    /// should take the subprocess path - the normal state on a host with
    /// no global library path.
    fn reader_for(&self, unit: &str) -> Option<std::cell::Ref<'_, sd_journal::Reader>> {
        {
            let mut slot = self.reader.borrow_mut();
            if !slot.as_ref().is_some_and(|reader| reader.unit() == unit) {
                *slot = sd_journal::Reader::open(unit);
            }
        }
        std::cell::Ref::filter_map(self.reader.borrow(), |slot| slot.as_ref()).ok()
    }

    /// Restart history recovered from the journal, for the window before
    /// masys started - from unit *failures*, which is what `NRestarts`
    /// counts. See `journal::UNIT_FAILED` for why starts are the wrong
    /// record.
    ///
    /// Without this, `RestartHistory` begins empty and flapping cannot
    /// fire until masys has been up for a full window - so the tool is
    /// blind to exactly the fault it exists to catch, for the first hour
    /// after you start it to investigate that fault.
    ///
    /// A one-time query, not a per-poll one. Reading the journal every
    /// tick would cost a tenth of the tick budget; reading it once at
    /// startup measured 0.36s for a *week* of records on the development
    /// host, and the window is an hour. A journal that is volatile, or
    /// absent, or slow simply yields nothing and the old warm-up
    /// behaviour returns - this can degrade, never fail.
    fn recovered_history(window_ms: u64) -> restarts::RestartHistory {
        let mut history = restarts::RestartHistory::new();
        let since = Self::now_unix_ms().saturating_sub(window_ms) / 1000;
        let Ok(output) = std::process::Command::new("journalctl")
            .args(["--no-pager", "-o", "json", "--since", &format!("@{since}"), &format!("MESSAGE_ID={}", journal::UNIT_FAILED)])
            .output()
        else {
            return history;
        };
        if !output.status.success() {
            return history;
        }
        let mut by_unit: std::collections::HashMap<String, Vec<u64>> = std::collections::HashMap::new();
        for (unit, stamp) in journal::parse_unit_events(&String::from_utf8_lossy(&output.stdout)) {
            by_unit.entry(unit).or_default().push(stamp);
        }
        for (unit, stamps) in by_unit {
            history.seed(&unit, stamps);
        }
        history
    }

    /// Unix milliseconds now. Named apart from `Snapshot::taken_at_ms`'s
    /// monotonic clock so the two are hard to confuse at a call site.
    fn now_unix_ms() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }

    fn manager_property(&self, name: &str) -> Result<String, MasysError> {
        let path = OwnedObjectPath::try_from(units::MANAGER_PATH).map_err(|e| MasysError::System(format!("{e}")))?;
        units::get_string(&self.conn, &path, units::MANAGER_IF, name)
    }

    /// Whether the host's clock is NTP-synchronised, from `timedate1`.
    /// Readable unprivileged. A host with no `timedate1` at all answers
    /// "synchronised" rather than firing a spurious finding.
    fn clock_synced(&self) -> bool {
        let Ok(path) = OwnedObjectPath::try_from("/org/freedesktop/timedate1") else { return true };
        self.conn
            .call_method(
                Some("org.freedesktop.timedate1"),
                path.as_str(),
                Some(units::PROPS_IF),
                "Get",
                &("org.freedesktop.timedate1", "NTPSynchronized"),
            )
            .ok()
            .and_then(|reply| reply.body().deserialize::<zbus::zvariant::OwnedValue>().ok())
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(true)
    }
}

/// Seconds east of UTC, right now, from the C library.
///
/// `localtime_r` rather than a fixed `TZ` parse: it follows the same zone
/// systemd and the journal itself use, and it answers for *this* instant,
/// so the value tracks daylight saving instead of freezing at whatever
/// was true when masys started.
fn local_utc_offset_secs() -> i32 {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as libc::time_t;
    // SAFETY: `tm` is zeroed and correctly sized, and `localtime_r` writes
    // into it rather than returning a shared static the way `localtime`
    // does. A null return means the timestamp is unrepresentable, and UTC
    // is the honest answer when the zone cannot be established.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() { 0 } else { tm.tm_gmtoff as i32 }
}

impl SystemService for SystemdService {
    fn sample(&self) -> Result<Snapshot, MasysError> {
        Ok(Snapshot {
            taken_at_ms: self.origin.elapsed().as_millis() as u64,
            clock_ticks_per_sec: self.clock_ticks,
            machine: Some(self.machine.clone()),
            procs: proc::read_procs(self.boot_secs, self.clock_ticks)?,
            pressure: proc::read_pressure(),
            filesystems: proc::read_filesystems()?,
            disks: proc::read_disks(),
            clock_synced: self.clock_synced(),
            utc_offset_secs: local_utc_offset_secs(),
            interfaces: proc::net_dev::read_interfaces(),
            oom_kills: self.oom_kills()?,
            system_state: system_state_of(&self.manager_property("SystemState")?),
            load: proc::read_load(),
            uptime_secs: proc::read_uptime(),
            memory: proc::read_memory(),
        })
    }

    fn units(&self) -> Result<Vec<Unit>, MasysError> {
        let listed: Vec<units::ListedUnit> = self
            .conn
            .call_method(Some(units::DEST), units::MANAGER_PATH, Some(units::MANAGER_IF), "ListUnits", &())
            .and_then(|reply| reply.body().deserialize())
            .map_err(|e| MasysError::System(format!("ListUnits: {e}")))?;

        let now = Self::now_unix_ms();
        let mut cold = self.cold.borrow_mut();
        // Re-read the rarely-changing properties periodically. Without
        // this a unit enabled outside masys would show its old state for
        // the life of the process, since nothing above can reach in and
        // refresh a service `App` has taken ownership of.
        cold.expire_if_stale(self.origin.elapsed().as_millis() as u64, COLD_TTL_MS);
        let mut restarts = self.restarts.borrow_mut();
        let mut out = Vec::new();
        let mut live: std::collections::HashSet<&str> = std::collections::HashSet::new();

        let mut stamps = self.stamps.borrow_mut();

        // Every unit systemd knows, `.device` included. They used to be
        // dropped here for costing a round trip each; nothing costs a
        // round trip each any more.
        for row in listed.iter() {
            let (name, path) = (&row.0, &row.6);
            let (raw_active, raw_sub) = (&row.3, &row.4);
            let kind = units::kind_of(name);
            let is_service = kind == masys_domain::unit::UnitKind::Service;
            live.insert(name.as_str());

            // Cold properties are fetched once per unit and reused; the
            // cache expires itself on a slow tick.
            let cached = match cold.get(name) {
                Some(properties) => properties.clone(),
                None => {
                    let fresh = units::read_cold(&self.conn, path, kind);
                    cold.insert(name.clone(), fresh.clone());
                    fresh
                }
            };

            let active_state = units::active_state_of(raw_active);

            // Only a failed unit's exit code is worth a round trip. This
            // was read for every service and then discarded for all but
            // the failed ones - 137 of 138 on this host, every poll.
            let exit_code = units::wants_exit_code(active_state)
                .then(|| units::get_i32(&self.conn, path, units::SERVICE_IF, "ExecMainStatus").ok())
                .flatten();

            // The restart counter is the one hot property with no cheaper
            // source: flapping is derived from it rising between polls.
            let n_restarts = if is_service { units::get_u32(&self.conn, path, units::SERVICE_IF, "NRestarts").unwrap_or(0) } else { 0 };
            // A rise means the unit went away and came back, which
            // `ListUnits` may not have caught between two polls - so the
            // cached timestamp describes a state it has since left.
            if is_service && restarts.count_rose(name, n_restarts) {
                stamps.forget(name);
            }

            let since_ms = match stamps.get(name, raw_active, raw_sub) {
                Some(cached) => cached,
                // A unit can vanish between ListUnits and the property
                // fetch; dropping it is correct, failing the poll is not.
                None => {
                    let Ok(since_us) = units::get_u64(&self.conn, path, units::UNIT_IF, "StateChangeTimestamp") else { continue };
                    stamps.insert(name, raw_active, raw_sub, since_us / 1000);
                    since_us / 1000
                }
            };

            out.push(Unit {
                kind,
                active_state,
                sub_state: raw_sub.clone(),
                exit_code,
                enabled: cached.enabled,
                restart_timestamps_ms: restarts.observe(name, n_restarts, now, self.flapping_window_ms),
                since_ms,
                cgroup: cached.cgroup.clone(),
                slice: cached.slice.clone(),
                triggers: cached.triggers.clone(),
                timer: name.ends_with(".timer").then(|| units::read_timer(&self.conn, path)).flatten(),
                name: name.clone(),
            });
        }
        restarts.retain_units(&live);
        cold.retain_units(&live);
        stamps.retain_units(&live);
        Ok(out)
    }

    fn unit_journal(&self, unit: &str, lines: usize) -> Result<Vec<Entry>, MasysError> {
        // The library path when there is one: it is the same answer for
        // a fraction of the cost, and it leaves a cursor open that
        // `follow_unit_journal` can then check without re-reading.
        if let Some(reader) = self.reader_for(unit) {
            return Ok(reader.tail(lines));
        }
        let output = std::process::Command::new("journalctl")
            .args(["--no-pager", "-o", "json", "-u", unit, "-n", &lines.to_string()])
            .output()
            .map_err(MasysError::Io)?;
        if !output.status.success() {
            return Err(MasysError::System(format!("journalctl -u {unit}: {}", String::from_utf8_lossy(&output.stderr).trim())));
        }
        Ok(journal::parse_stream(&String::from_utf8_lossy(&output.stdout)))
    }

    fn follow_unit_journal(&self, unit: &str, lines: usize) -> Result<Option<Vec<Entry>>, MasysError> {
        let Some(reader) = self.reader_for(unit) else {
            return Ok(None);
        };
        if !reader.changed() {
            return Ok(None);
        }
        Ok(Some(reader.tail(lines)))
    }

    /// Seven targeted `Get`s against one unit.
    ///
    /// Never `GetAll`, for the reason `units.rs` states: the Unit
    /// interface carries ~100 properties, several computed on demand, and
    /// costs 48x a targeted read. Seven reads for the unit under the
    /// cursor is noise; a hundred would not be.
    fn unit_detail(&self, unit: &str) -> Result<masys_domain::unit::UnitDetail, MasysError> {
        let path = units::unit_path(&self.conn, unit)?;
        Ok(units::read_detail(&self.conn, &path))
    }

    /// Reads `/proc/net` once per call rather than caching it: the
    /// tables change constantly, and a detail row that named a socket
    /// closed three minutes ago would be worse than one that named none.
    fn proc_detail(&self, pid: u32) -> Result<masys_domain::proc_detail::ProcDetail, MasysError> {
        proc::detail::read_proc_detail(pid, &proc::net::read_socket_table())
    }

    fn start(&self, unit: &str) -> Result<(), MasysError> {
        actions::systemctl("start", unit)
    }
    fn stop(&self, unit: &str) -> Result<(), MasysError> {
        actions::systemctl("stop", unit)
    }
    fn restart(&self, unit: &str) -> Result<(), MasysError> {
        actions::systemctl("restart", unit)
    }
    fn reload(&self, unit: &str) -> Result<(), MasysError> {
        actions::systemctl("reload", unit)
    }
    fn try_restart(&self, unit: &str) -> Result<(), MasysError> {
        actions::systemctl("try-restart", unit)
    }
    fn reset_failed(&self, unit: &str) -> Result<(), MasysError> {
        actions::systemctl("reset-failed", unit)
    }

    fn enable(&self, unit: &str) -> Result<(), MasysError> {
        actions::systemctl("enable", unit)
    }

    fn disable(&self, unit: &str) -> Result<(), MasysError> {
        actions::systemctl("disable", unit)
    }

    fn mask(&self, unit: &str) -> Result<(), MasysError> {
        actions::systemctl("mask", unit)
    }

    fn unmask(&self, unit: &str) -> Result<(), MasysError> {
        actions::systemctl("unmask", unit)
    }

    fn edit(&self, unit: &str) -> Result<(), MasysError> {
        actions::systemctl_interactive("edit", unit)
    }
    fn kill(&self, pid: u32, signal: Signal) -> Result<(), MasysError> {
        actions::kill(pid, signal)
    }
    fn renice(&self, pid: u32, value: i32) -> Result<(), MasysError> {
        actions::renice(pid, value)
    }
    fn ionice(&self, pid: u32, class: IoNiceClass, level: i32) -> Result<(), MasysError> {
        actions::ionice(pid, class, level)
    }
}

impl SystemdService {
    /// Kernel OOM-kill records, from the journal. Kept separate from
    /// `journal()` because it is a fixed kernel-transport query rather
    /// than the Journal buffer's arbitrary lookback.
    fn oom_kills(&self) -> Result<Vec<OomKill>, MasysError> {
        Ok(Vec::new())
    }
}
