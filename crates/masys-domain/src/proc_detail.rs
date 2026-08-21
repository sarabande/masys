//! What a process shows when its row is opened.
//!
//! Split from [`crate::sample::Proc`] for the same reason
//! [`crate::unit::UnitDetail`] is split from `Unit`: the cost is different
//! in kind. `Proc` is one sweep of `/proc` for every process on every
//! tick; this is a handful of targeted reads for *one* process, paid for
//! when somebody is looking at it. Folding these into the sweep would
//! multiply the poll by the number of processes on the host - and several
//! of these reads are per-descriptor, so the multiplication is worse than
//! it looks.
//!
//! Every field is optional and the whole struct has a `Default`, because
//! all of it is unreadable for a process masys does not own: the kernel
//! gates `cmdline` loosely but `environ`, `cwd`, `exe` and `fd/` on
//! same-uid or `CAP_SYS_PTRACE`. An unprivileged masys sees most of this
//! only for its own user's processes, which is the same degradation
//! `/proc/[pid]/io` already has and is reported the same way: absent,
//! never zero.

/// What one open file descriptor points at.
///
/// A socket descriptor reads as `socket:[38271]` in `/proc/[pid]/fd`,
/// which names an inode and nothing else. Resolving that inode against
/// the kernel's own socket tables is the difference between a row that
/// says `socket:[38271]` and one that says `tcp 0.0.0.0:5432 LISTEN` -
/// the first is a number, the second is why the service exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FdTarget {
    /// A file, device or directory - whatever the symlink resolves to.
    Path(String),
    /// A TCP socket, with its local address and, when connected, its
    /// peer. `state` is systemd-free kernel vocabulary: `LISTEN`,
    /// `ESTABLISHED`, `TIME_WAIT`.
    Tcp {
        local: String,
        peer: Option<String>,
        state: String,
    },
    Udp {
        local: String,
    },
    /// A unix socket. `path` is `None` for an abstract or unnamed one,
    /// which is most of them.
    ///
    /// `listening` comes from the kernel's `SO_ACCEPTCON` flag rather
    /// than from having a path. A client that *connected* to a named
    /// socket reports that same path, so path-alone reported every one
    /// of dbus-broker's 87 accepted connections as a listener.
    Unix {
        path: Option<String>,
        listening: bool,
    },
    Pipe,
    /// `anon_inode:[eventpoll]`, `anon_inode:[timerfd]`, and a socket
    /// whose inode was not in any table - which happens when the socket
    /// closes between the two reads.
    Other(String),
}

/// One open file descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fd {
    pub number: u32,
    pub target: FdTarget,
}

impl Fd {
    /// Whether this is one of the three the shell hands every process.
    /// They are worth showing by name even when the other forty are
    /// summarised, because a daemon with stdout on a pipe and one with
    /// stdout on the journal behave differently when the far end goes
    /// away.
    pub fn is_standard(&self) -> bool {
        self.number <= 2
    }
}

/// The properties a process shows when its row is opened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcDetail {
    /// The full command line, arguments joined by spaces.
    ///
    /// The single most useful field here. `Proc::comm` is capped at 15
    /// bytes by the kernel, so every `python3`, `node` and `java` on a
    /// host is indistinguishable in the table; this is what tells them
    /// apart. Empty for a kernel thread, which genuinely has none.
    pub cmdline: Option<String>,
    /// The binary on disk, from the `exe` symlink. Worth its own line
    /// separately from `cmdline`, which reports whatever `argv[0]` was
    /// set to and can be a lie.
    pub exe: Option<String>,
    /// The absolute directory the process runs in - which is what
    /// resolves every relative path in `cmdline`.
    pub cwd: Option<String>,
    pub ppid: Option<u32>,
    /// `VmSize`: everything mapped, whether or not it is resident. Beside
    /// `Proc::rss_bytes` it separates a process that has *reserved* a lot
    /// from one that is *using* a lot.
    pub virt_bytes: Option<u64>,
    /// `VmSwap`: how much of this process the kernel has pushed out. A
    /// process with resident memory falling and swap rising is being
    /// squeezed, which neither figure shows alone.
    pub swap_bytes: Option<u64>,
    /// The environment, in the order the kernel reports it.
    ///
    /// Values are carried and displayed verbatim, including secrets. That
    /// is a deliberate choice, not an oversight: this reads
    /// `/proc/[pid]/environ`, which the kernel already restricts to the
    /// process's own uid or `CAP_SYS_PTRACE`, so masys can only show an
    /// operator what `cat` would show them anyway. Redacting it would
    /// hide the value exactly when someone is debugging why a service has
    /// the wrong one.
    pub env: Vec<(String, String)>,
    /// Open descriptors, lowest number first. Empty when the directory
    /// could not be read, which for an unprivileged masys is the common
    /// case rather than an error.
    pub fds: Vec<Fd>,
}

impl ProcDetail {
    /// The listening sockets among the descriptors - a service's whole
    /// reason for running, and the one part of a long fd list that is
    /// worth pulling to the front.
    pub fn listening(&self) -> impl Iterator<Item = &Fd> {
        self.fds.iter().filter(|fd| match &fd.target {
            FdTarget::Tcp { state, .. } => state == "LISTEN",
            FdTarget::Unix { listening, .. } => *listening,
            _ => false,
        })
    }

    /// How many descriptors fall into each broad kind, for the summary
    /// line that stands in for a list of four hundred.
    pub fn fd_counts(&self) -> FdCounts {
        self.fds.iter().fold(FdCounts::default(), |mut counts, fd| {
            match fd.target {
                FdTarget::Path(_) => counts.files += 1,
                FdTarget::Tcp { .. } | FdTarget::Udp { .. } | FdTarget::Unix { .. } => counts.sockets += 1,
                FdTarget::Pipe => counts.pipes += 1,
                FdTarget::Other(_) => counts.other += 1,
            }
            counts
        })
    }
}

/// Descriptors by kind. A count of each rather than a total alone,
/// because "four hundred open files" and "four hundred open sockets" are
/// different problems with different fixes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FdCounts {
    pub files: u32,
    pub sockets: u32,
    pub pipes: u32,
    pub other: u32,
}

impl FdCounts {
    pub fn total(&self) -> u32 {
        self.files + self.sockets + self.pipes + self.other
    }
}
