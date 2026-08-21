#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureResource {
    Cpu,
    Io,
    Memory,
}

/// Triage's output vocabulary - what the status buffer renders one row
/// per. Produced by `crate::triage`'s functions from `Unit`/`Snapshot`
/// facts plus `Thresholds`.
#[derive(Debug, Clone, PartialEq)]
pub enum Finding {
    FailedUnit {
        unit: String,
        exit_code: Option<i32>,
        since_ms: u64,
        /// The unit's own last words - the line it printed before dying,
        /// not systemd's report that it died. `None` until something with
        /// journal access fills it in: `crate::triage` is pure and has
        /// none, so it always produces `None` and the session enriches
        /// afterwards.
        reason: Option<String>,
    },
    FlappingUnit {
        unit: String,
        restarts: u32,
        window_ms: u64,
    },
    Pressure {
        resource: PressureResource,
        some_avg60: f32,
        full_avg60: Option<f32>,
    },
    /// `generations` is `Some` only for `/boot`, and only when
    /// `PlatformService::boot_pressure` answered - it's what "why /boot is
    /// full" needs that a plain percentage doesn't carry.
    DiskCapacity {
        mount_point: String,
        used_percent: f32,
        free_bytes: u64,
        generations: Option<u32>,
    },
    InodeExhaustion {
        mount_point: String,
        inode_used_percent: f32,
    },
    ReadOnlyFilesystem {
        mount_point: String,
    },
    ClockUnsynchronized,
    OomKill {
        pid: u32,
        comm: String,
        timestamp_ms: u64,
    },
    SystemDegraded {
        failed_units: u32,
    },
}

/// Triage's tunable inputs. The design's Keymap and config section says
/// thresholds live in config, not in code - this is that seam: masys-app
/// (a later plan) owns loading `~/.config/masys/config.toml` and building
/// one of these, and every function below takes it as a parameter rather
/// than hardcoding a number. `Default` supplies the values used until
/// config loading exists.
#[derive(Debug, Clone, PartialEq)]
pub struct Thresholds {
    pub flapping_restart_count: u32,
    pub flapping_window_ms: u64,
    pub psi_some_avg60_percent: f32,
    pub psi_full_avg60_percent: f32,
    pub disk_used_percent: f32,
    pub inode_used_percent: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            flapping_restart_count: 3,
            flapping_window_ms: 3_600_000,
            psi_some_avg60_percent: 20.0,
            psi_full_avg60_percent: 5.0,
            disk_used_percent: 85.0,
            inode_used_percent: 90.0,
        }
    }
}
