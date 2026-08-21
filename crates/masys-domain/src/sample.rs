#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcState {
    Running,
    Sleeping,
    UninterruptibleWait,
    Zombie,
    Stopped,
}

/// One process, as `/proc/[pid]/stat` reports it - cumulative counters, no
/// rates. `SystemService::sample` returns these raw; `crate::rate::derive`
/// turns a pair of them into CPU%/IO per second.
#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub comm: String,
    pub cgroup: Option<String>,
    /// Sum of utime+stime, in clock ticks (`/proc/[pid]/stat` fields
    /// 14+15).
    pub cpu_ticks: u64,
    pub rss_bytes: u64,
    /// Bytes actually written to storage (`/proc/[pid]/io`'s
    /// `write_bytes`), not bytes requested - the figure that actually
    /// predicts disk pressure.
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
    pub state: ProcState,
    pub nice: i32,
    pub oom_score: i32,
    pub threads: u32,
    pub started_at_ms: u64,
}

/// One resource's PSI readout - `some` is "at least one task stalled",
/// `full` is "every task stalled" (the kernel always reports `full` as 0
/// for cpu, since a stalled-on-cpu task by definition isn't the only
/// runnable one).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PsiLine {
    pub avg10: f32,
    pub avg60: f32,
    pub avg300: f32,
}

/// `/proc/pressure/{cpu,io,memory}`, read as one struct since a triage
/// pass and the status buffer both want all three resources together.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pressure {
    pub cpu_some: PsiLine,
    pub io_some: PsiLine,
    pub io_full: PsiLine,
    pub memory_some: PsiLine,
    pub memory_full: PsiLine,
}

/// What this host *is*, as opposed to what it is doing: the facts that
/// identify the machine rather than measure it.
///
/// Read once and reused, because none of it changes while masys runs -
/// a kernel upgrade needs a reboot and a distro upgrade needs at least a
/// new `/etc/os-release`, neither of which happens mid-session.
///
/// `distro` is the plain `PRETTY_NAME` string rather than anything
/// derived. Naming the distro is not distro-*specific* work, so it stays
/// on `SystemService` and leaves `PlatformService` for behaviour that
/// genuinely differs - generations, package management, unit ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    /// `/etc/os-release`'s `PRETTY_NAME`, e.g. `NixOS 26.11 (Zokor)`.
    pub distro: String,
    /// The running kernel's release, e.g. `6.18.42`. The *running* one,
    /// not the installed one - they differ exactly when a reboot is
    /// pending, which is a thing masys reports.
    pub kernel: String,
    pub arch: String,
    pub cpu_model: String,
    /// Logical CPUs, hyperthreads included - the number a CPU percentage
    /// is measured against, so a single process can legitimately exceed
    /// 100%.
    pub cpu_cores: u32,
}

/// The kernel's 1/5/15-minute run-queue averages, as `/proc/loadavg`
/// reports them. Grouped rather than three separate optional fields
/// because the source is a single line: an adapter either read it or it
/// didn't, so "1-minute known, 5-minute unknown" is not a state that can
/// occur and the type shouldn't be able to say it.
///
/// Load is context, not a triage signal - it conflates busy with stalled
/// and lags by design, which is why `Pressure` exists and why nothing in
/// `crate::triage` reads this.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadAverage {
    pub one: f32,
    pub five: f32,
    pub fifteen: f32,
}

/// System-wide memory and swap, grouped for the same reason as
/// `LoadAverage` - `/proc/meminfo` is one read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Memory {
    pub used_bytes: u64,
    pub total_bytes: u64,
    /// Zero on a host with swap fully consumed *and* on a host with no
    /// swap at all - the mockup renders both as `swap 0B free`, and
    /// nothing triages on it.
    pub swap_free_bytes: u64,
    /// `None` on a host with no zram swap configured, which is a
    /// different fact from memory not having been sampled - that one is
    /// the enclosing `Option<Memory>` being `None`.
    pub zram_percent: Option<f32>,
}

/// One block device's cumulative IO counters, straight from
/// `/proc/diskstats`. Counters, not rates - `crate::rate` turns a pair of
/// samples into throughput, the same rule `Proc` follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    /// Kernel device name: `sda`, `nvme0n1`, `zram0`.
    pub name: String,
    pub reads: u64,
    pub writes: u64,
    /// Sectors, which are always 512 bytes in `/proc/diskstats`
    /// regardless of the device's real block size - the kernel fixes the
    /// unit there so the field means the same thing on every disk.
    pub read_sectors: u64,
    pub write_sectors: u64,
    /// Milliseconds the device has spent with any request in flight.
    /// Its rate of change is utilisation: 1000ms of IO time per elapsed
    /// second means the device was busy the whole time.
    pub io_ms: u64,
    /// Requests currently queued.
    pub in_flight: u64,
}

/// One network interface's cumulative counters, from `/proc/net/dev`.
///
/// Counters rather than rates, for the reason `Disk` carries counters: a
/// rate is a difference over a known interval, and only `crate::rate`
/// knows the interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    /// Kernel interface name: `enp0s31f6`, `wlan0`, `tailscale0`.
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    /// Frames the driver rejected outright, and frames dropped for want
    /// of buffer space. Cumulative since boot, so what matters is that
    /// they *move* - which is why they are counters here and a total in
    /// the view rather than a rate.
    pub rx_errs: u64,
    pub tx_errs: u64,
    pub rx_drop: u64,
    pub tx_drop: u64,
    /// From `operstate`, where only `down` means down.
    ///
    /// `unknown` is what a tun device or a bridge with no carrier
    /// semantics reports while working perfectly - `tailscale0` on the
    /// development host reports it and carries real traffic. Treating
    /// `unknown` as down would hide a working VPN.
    pub up: bool,
    /// `ARPHRD_LOOPBACK`, rather than a test for the name `lo`.
    ///
    /// The machine talking to itself is not a network path, and on this
    /// host loopback has moved 3.8 MB - enough that "has it ever moved a
    /// byte" would not hide it.
    pub loopback: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Filesystem {
    pub mount_point: String,
    pub used_percent: f32,
    pub free_bytes: u64,
    pub inode_used_percent: f32,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OomKill {
    pub pid: u32,
    pub comm: String,
    pub timestamp_ms: u64,
}

/// Mirrors `systemctl is-system-running`'s states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemState {
    Running,
    Degraded,
    Maintenance,
    Starting,
    Stopping,
    Offline,
}

/// Raw cumulative counters and system-wide facts for one point in time.
/// No rates - see `crate::rate`. `taken_at_ms` is on some monotonic clock
/// the adapter chooses; only deltas between two snapshots are meaningful,
/// never the absolute value. Keeping it a bare `u64` (not
/// `std::time::Instant`) is what lets tests build fixture snapshots by
/// hand.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub taken_at_ms: u64,
    pub procs: Vec<Proc>,
    /// `None` when the kernel does not report PSI at all - it needs
    /// 4.20+ with `CONFIG_PSI=y`, and some kernels still want `psi=1` on
    /// the command line.
    ///
    /// Optional rather than defaulted because a triage tool must not
    /// render "not measured" identically to "no pressure": zeroes here
    /// would make a kernel without PSI look like a perfectly idle
    /// machine, which is the most dangerous wrong answer this type can
    /// give.
    pub pressure: Option<Pressure>,
    pub filesystems: Vec<Filesystem>,
    pub disks: Vec<Disk>,
    pub interfaces: Vec<Interface>,
    pub clock_synced: bool,
    /// Seconds east of UTC where this sample was taken - `tm_gmtoff`.
    ///
    /// A machine-clock fact of the sample, beside `clock_synced` and
    /// `clock_ticks_per_sec`, and carried for the same reason they are:
    /// the layers above must not read the environment for it.
    /// `masys-app` groups journal entries by local calendar day, which is
    /// arithmetic once the offset is known and a `TZ` read otherwise -
    /// and the app depends on `masys-domain` and `masys-view` alone.
    ///
    /// Refreshed every sample rather than captured once, so a session
    /// running across a daylight-saving change does not go stale.
    pub utc_offset_secs: i32,
    pub oom_kills: Vec<OomKill>,
    pub system_state: SystemState,
    /// The three below are `Option` because an adapter may be unable to
    /// measure them at all, and the status buffer must render "not
    /// measured" differently from a real zero - the same rule
    /// `crate::rate` follows for rates before two samples exist.
    /// The unit `Proc::cpu_ticks` is denominated in, from
    /// `sysconf(_SC_CLK_TCK)`. Carried on the snapshot rather than assumed
    /// to be 100 because it is a kernel build option, and because
    /// `crate::rate::derive` cannot divide by it correctly without being
    /// told - the adapter is the only layer that can know.
    pub clock_ticks_per_sec: u64,
    /// `None` when the adapter could not identify the host at all. Never
    /// changes between ticks; carried on the snapshot rather than fetched
    /// separately so nothing has to decide when to re-read it.
    pub machine: Option<Machine>,
    pub load: Option<LoadAverage>,
    pub uptime_secs: Option<u64>,
    pub memory: Option<Memory>,
}
