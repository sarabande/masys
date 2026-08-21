pub mod cgroup;
pub mod detail;
pub mod diskstats;
pub mod io;
pub mod loadavg;
pub mod meminfo;
pub mod mounts;
pub mod net;
pub mod net_dev;
pub mod pressure;
pub mod stat;
pub mod status;

use std::fs;

use masys_domain::error::MasysError;
use masys_domain::sample::{Disk, Filesystem, LoadAverage, Memory, Pressure, Proc};

/// Clock ticks per second, from `sysconf(_SC_CLK_TCK)` rather than a
/// hardcoded 100: it is 100 on every mainstream Linux build, but it is a
/// kernel configuration option and `/proc/[pid]/stat` is denominated in it.
pub fn clock_ticks_per_second() -> u64 {
    // SAFETY: sysconf with a valid name is always safe; it only reads a
    // kernel-provided constant.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks > 0 { ticks as u64 } else { 100 }
}

/// Unix seconds at boot, from `/proc/stat`'s `btime` line. Needed because
/// `/proc/[pid]/stat` dates a process in ticks since boot, while
/// `Proc::started_at_ms` is Unix milliseconds.
pub fn boot_time_secs(proc_stat: &str) -> Result<u64, MasysError> {
    proc_stat
        .lines()
        .find_map(|line| line.strip_prefix("btime ")?.trim().parse::<u64>().ok())
        .ok_or_else(|| MasysError::System("/proc/stat: no btime".into()))
}

/// Every process currently in `/proc`.
///
/// Two degradations are deliberate and documented. A process that exits
/// mid-sweep yields `ENOENT` and is skipped, never failing the sample -
/// on a busy host that happens most polls. And `/proc/[pid]/io` requires
/// same-uid or `CAP_SYS_PTRACE`, so it is unreadable for most processes
/// when masys runs unprivileged (195 of 336 on the development host);
/// those report **zero** IO, matching how `top` behaves and understating
/// other users' processes.
pub fn read_procs(boot_secs: u64, ticks: u64) -> Result<Vec<Proc>, MasysError> {
    let entries = fs::read_dir("/proc").map_err(MasysError::Io)?;
    let mut procs = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Ok(raw_stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
        let Ok(stat) = stat::parse_stat(&raw_stat) else { continue };
        let Ok(raw_status) = fs::read_to_string(format!("/proc/{pid}/status")) else { continue };
        let Ok(status) = status::parse_status(&raw_status) else { continue };
        // Permission denied here is the common case, not an error.
        let (io_read_bytes, io_write_bytes) =
            fs::read_to_string(format!("/proc/{pid}/io")).ok().and_then(|text| io::parse_io(&text).ok()).unwrap_or((0, 0));
        procs.push(Proc {
            pid,
            comm: stat.comm,
            cgroup: fs::read_to_string(format!("/proc/{pid}/cgroup")).ok().and_then(|text| cgroup::parse_cgroup(&text)),
            cpu_ticks: stat.cpu_ticks,
            rss_bytes: status.rss_bytes,
            io_read_bytes,
            io_write_bytes,
            state: stat.state,
            nice: stat.nice,
            oom_score: fs::read_to_string(format!("/proc/{pid}/oom_score")).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0),
            threads: status.threads,
            started_at_ms: (boot_secs * 1000) + (stat.starttime_ticks * 1000 / ticks),
        });
    }
    Ok(procs)
}

/// PSI for all three resources, or `None` on a kernel that does not
/// report it.
///
/// Absence is detected on `cpu` alone: the three files appear and vanish
/// together, since they come from one kernel feature. Reporting zeroes
/// instead would make an unmeasurable machine look idle.
pub fn read_pressure() -> Option<Pressure> {
    let read = |resource: &str| -> Option<(masys_domain::sample::PsiLine, masys_domain::sample::PsiLine)> {
        fs::read_to_string(format!("/proc/pressure/{resource}")).ok().and_then(|text| pressure::parse_pressure(&text).ok())
    };
    // The kernel always reports cpu's `full` as zero - a task stalled on
    // cpu is by definition not the only runnable one - so it is dropped
    // rather than surfaced as a permanent 0.0%.
    let (cpu_some, _) = read("cpu")?;
    let (io_some, io_full) = read("io").unwrap_or_default();
    let (memory_some, memory_full) = read("memory").unwrap_or_default();
    Some(Pressure { cpu_some, io_some, io_full, memory_some, memory_full })
}

/// Capacity for every real filesystem, deduplicated by device so a bind
/// mount (`/nix/store` and `/` share `/dev/sda2` here) is reported once.
pub fn read_filesystems() -> Result<Vec<Filesystem>, MasysError> {
    let text = fs::read_to_string("/proc/mounts").map_err(MasysError::Io)?;
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for mount in mounts::parse_mounts(&text) {
        if seen.contains(&mount.device) {
            continue;
        }
        let Some(stats) = statvfs(&mount.mount_point) else { continue };
        let (blocks, bfree, bavail, files, ffree) = stats;
        // Zero-capacity mounts - rpc_pipefs, nfsd, gvfs, the flatpak doc
        // portal - have nothing to report and would divide by zero.
        if blocks == 0 {
            continue;
        }
        // `read_only` must mean *the kernel remounted this after an I/O
        // error*, which is the silent failure the Disk section exists to
        // catch. A read-only NFS export or a `/run/credentials` tmpfs is
        // a configuration choice, and reporting it would fire five false
        // findings on the development host alone. Only a local,
        // block-device-backed filesystem can be remounted read-only in
        // the sense triage means.
        let read_only = mounts::is_kernel_remount(&mount);
        seen.push(mount.device);
        // Percent used counts what a non-root user can actually reach:
        // the reserved-block margin is unavailable, so measuring against
        // `blocks` alone reports a full disk as 95%.
        let usable = blocks.saturating_sub(bfree.saturating_sub(bavail));
        out.push(Filesystem {
            used_percent: if usable > 0 { (usable - bavail) as f32 * 100.0 / usable as f32 } else { 0.0 },
            free_bytes: bavail,
            inode_used_percent: if files > 0 { (files - ffree) as f32 * 100.0 / files as f32 } else { 0.0 },
            read_only,
            mount_point: mount.mount_point,
        });
    }
    Ok(out)
}

/// `(blocks, bfree, bavail, files, ffree)` in bytes for the block counts.
/// `None` when the path cannot be stat'd at all, which is normal for a
/// mount that disappeared between reading `/proc/mounts` and this call.
fn statvfs(path: &str) -> Option<(u64, u64, u64, u64, u64)> {
    let c_path = std::ffi::CString::new(path).ok()?;
    let mut buf: libc::statvfs64 = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated string and `buf` is a
    // correctly sized, zeroed statvfs64 this call fills in.
    if unsafe { libc::statvfs64(c_path.as_ptr(), &mut buf) } != 0 {
        return None;
    }
    let unit = buf.f_frsize as u64;
    Some((buf.f_blocks as u64 * unit, buf.f_bfree as u64 * unit, buf.f_bavail as u64 * unit, buf.f_files as u64, buf.f_ffree as u64))
}

pub fn read_load() -> Option<LoadAverage> {
    fs::read_to_string("/proc/loadavg").ok().and_then(|t| loadavg::parse_loadavg(&t).ok())
}

pub fn read_uptime() -> Option<u64> {
    fs::read_to_string("/proc/uptime").ok().and_then(|t| loadavg::parse_uptime(&t).ok())
}

pub fn read_memory() -> Option<Memory> {
    let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
    let swaps = fs::read_to_string("/proc/swaps").unwrap_or_default();
    meminfo::parse_meminfo(&meminfo, &swaps).ok()
}

pub fn read_disks() -> Vec<Disk> {
    fs::read_to_string("/proc/diskstats").map(|text| diskstats::parse_diskstats(&text)).unwrap_or_default()
}
