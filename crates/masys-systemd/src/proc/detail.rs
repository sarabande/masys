//! The per-process reads that only happen when a row is opened:
//! `cmdline`, `exe`, `cwd`, `environ`, `fd/`, and the `status` fields the
//! per-tick sweep does not carry.
//!
//! Everything here degrades to absent rather than failing. The kernel
//! gates `environ`, `cwd`, `exe` and `fd/` on same-uid or
//! `CAP_SYS_PTRACE`, so an unprivileged masys can read almost none of it
//! for other users' processes - which is the common case, not an error
//! condition. The same rule `/proc/[pid]/io` already follows: report what
//! could be read, leave the rest absent, and never substitute a zero.

use std::fs;

use masys_domain::error::MasysError;
use masys_domain::proc_detail::{Fd, FdTarget, ProcDetail};

use super::net::SocketTable;

/// A `\0`-separated `/proc` field, as one displayable string.
///
/// `cmdline` and `environ` both use NUL as the separator and both end
/// with a trailing one, so the empty tail is dropped. A kernel thread's
/// `cmdline` is genuinely empty and yields `None`, which is the honest
/// answer - it has no command line, rather than a blank one.
fn nul_separated(text: &str) -> Vec<String> {
    text.split('\0').filter(|part| !part.is_empty()).map(str::to_string).collect()
}

/// `KEY=value` pairs in the order the kernel reports them.
///
/// A value containing `=` keeps it: `split_once` rather than `split`, so
/// `LS_COLORS=rs=0:di=01;34` survives intact. An entry with no `=` at all
/// is dropped - the kernel does not produce them, but `environ` is
/// writable by the process itself and nothing guarantees its shape.
fn parse_environ(text: &str) -> Vec<(String, String)> {
    nul_separated(text)
        .into_iter()
        .filter_map(|entry| entry.split_once('=').map(|(key, value)| (key.to_string(), value.to_string())))
        .collect()
}

/// The `status` fields the per-tick sweep does not carry.
///
/// `PPid`, `VmSize` and `VmSwap` come from the same file `super::status`
/// already reads for `Threads` and `VmRSS`; they are parsed here rather
/// than added there because the sweep does not need them and every field
/// added to the sweep is paid for once per process per tick.
fn parse_status_extras(text: &str) -> (Option<u32>, Option<u64>, Option<u64>) {
    let field = |name: &str| -> Option<&str> { text.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix(':').map(str::trim)) };
    // "    7720 kB" - always kB, per Documentation/filesystems/proc.rst.
    let kb = |name: &str| field(name).and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok()).map(|kb| kb * 1024);
    (field("PPid").and_then(|v| v.parse().ok()), kb("VmSize"), kb("VmSwap"))
}

/// `socket:[38271]` -> `38271`.
fn socket_inode(target: &str) -> Option<u64> {
    target.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok()
}

/// One descriptor's symlink target, classified.
///
/// A socket that is not in the table resolves to `Other` rather than
/// being dropped: the tables and the descriptor list are two reads and
/// the socket may close between them, and a descriptor masys cannot name
/// is still a descriptor the process has open.
fn classify(target: &str, sockets: &SocketTable) -> FdTarget {
    match socket_inode(target) {
        Some(inode) => sockets.get(&inode).cloned().unwrap_or_else(|| FdTarget::Other(target.to_string())),
        None if target.starts_with("pipe:[") => FdTarget::Pipe,
        None if target.starts_with("anon_inode:") => FdTarget::Other(target.to_string()),
        None => FdTarget::Path(target.to_string()),
    }
}

/// Every open descriptor, lowest number first.
///
/// Sorted by number rather than by whatever order the directory yields,
/// because 0, 1 and 2 are the three worth reading first and a directory
/// listing puts them wherever it likes.
fn read_fds(pid: u32, sockets: &SocketTable) -> Vec<Fd> {
    let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else { return Vec::new() };
    let mut fds: Vec<Fd> = entries
        .flatten()
        .filter_map(|entry| {
            let number: u32 = entry.file_name().to_str()?.parse().ok()?;
            // A descriptor closing mid-listing yields ENOENT here, which
            // on a busy process happens routinely.
            let target = fs::read_link(entry.path()).ok()?;
            Some(Fd { number, target: classify(&target.to_string_lossy(), sockets) })
        })
        .collect();
    fds.sort_by_key(|fd| fd.number);
    fds
}

/// Everything one process shows when its row is opened.
///
/// Takes the socket table rather than reading it, so a caller opening
/// several rows - or re-reading one on every tick while it stays open -
/// pays for `/proc/net` once instead of once per process.
pub fn read_proc_detail(pid: u32, sockets: &SocketTable) -> Result<ProcDetail, MasysError> {
    // The one read that must succeed. Not for its contents - a kernel
    // thread's is empty - but because it is the cheapest proof the pid
    // still exists: everything below reports "unreadable" and
    // "gone" identically, so without this a dead pid would open a row
    // full of blanks rather than saying it had gone.
    if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
        return Err(MasysError::System(format!("no process {pid}")));
    }
    let text = |name: &str| fs::read_to_string(format!("/proc/{pid}/{name}")).ok();
    let link = |name: &str| fs::read_link(format!("/proc/{pid}/{name}")).ok().map(|p| p.to_string_lossy().into_owned());

    let (ppid, virt_bytes, swap_bytes) = text("status").map(|t| parse_status_extras(&t)).unwrap_or_default();
    Ok(ProcDetail {
        cmdline: text("cmdline").map(|t| nul_separated(&t).join(" ")).filter(|line| !line.is_empty()),
        exe: link("exe"),
        cwd: link("cwd"),
        ppid,
        virt_bytes,
        swap_bytes,
        env: text("environ").map(|t| parse_environ(&t)).unwrap_or_default(),
        fds: read_fds(pid, sockets),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn a_command_line_joins_its_nul_separated_arguments() {
        assert_eq!(nul_separated("postgres\0-D\0/var/lib/postgresql/16\0").join(" "), "postgres -D /var/lib/postgresql/16");
    }

    /// A kernel thread has no command line at all - which is a different
    /// fact from an empty one, and the reason this returns a `Vec` the
    /// caller can find empty rather than a blank string.
    #[test]
    fn a_kernel_thread_has_no_command_line() {
        assert!(nul_separated("").is_empty());
    }

    /// `LS_COLORS=rs=0:di=01;34` is a real variable and splitting on
    /// every `=` would truncate it at the first one.
    #[test]
    fn an_environment_value_may_contain_equals_signs() {
        let env = parse_environ("PATH=/bin\0LS_COLORS=rs=0:di=01;34\0");
        assert_eq!(env, vec![("PATH".into(), "/bin".into()), ("LS_COLORS".into(), "rs=0:di=01;34".into())]);
    }

    #[test]
    fn status_extras_come_back_in_bytes() {
        let text = "Name:\tpostgres\nPPid:\t1\nVmSize:\t   4300000 kB\nVmRSS:\t   1400000 kB\nVmSwap:\t        0 kB\n";
        assert_eq!(parse_status_extras(text), (Some(1), Some(4_300_000 * 1024), Some(0)));
    }

    /// A kernel thread has no `Vm*` lines whatsoever. Absent, not zero.
    #[test]
    fn a_kernel_thread_has_no_memory_lines() {
        assert_eq!(parse_status_extras("Name:\tkthreadd\nPPid:\t2\n"), (Some(2), None, None));
    }

    #[test]
    fn a_socket_descriptor_resolves_through_the_table() {
        let sockets: SocketTable =
            HashMap::from([(38271, FdTarget::Tcp { local: "0.0.0.0:5432".into(), peer: None, state: "LISTEN".into() })]);
        assert!(matches!(classify("socket:[38271]", &sockets), FdTarget::Tcp { .. }));
    }

    /// The tables and the descriptor list are two separate reads, so a
    /// socket can close between them. Still a descriptor, still shown.
    #[test]
    fn an_unresolvable_socket_is_kept_rather_than_dropped() {
        assert_eq!(classify("socket:[99999]", &HashMap::new()), FdTarget::Other("socket:[99999]".into()));
    }

    #[test]
    fn ordinary_targets_classify_by_their_prefix() {
        let none = HashMap::new();
        assert_eq!(classify("/dev/null", &none), FdTarget::Path("/dev/null".into()));
        assert_eq!(classify("pipe:[4012]", &none), FdTarget::Pipe);
        assert_eq!(classify("anon_inode:[eventpoll]", &none), FdTarget::Other("anon_inode:[eventpoll]".into()));
    }
}
