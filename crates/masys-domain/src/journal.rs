/// Ordered by syslog's own numbering, worst first, which is the order the
/// variants are written in - so `Emergency < Debug` and the derive is the
/// right one. Callers should say what they mean with
/// [`Priority::at_or_worse_than`] rather than comparing directly: `<=`
/// meaning "more severe" is backwards to every reader who has not just
/// read this comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Emergency,
    Alert,
    Critical,
    Error,
    Warning,
    Notice,
    Info,
    Debug,
}

impl Priority {
    /// Whether this is `floor` or worse - `Error` passes a floor of
    /// `Error`, and so does `Critical`.
    pub fn at_or_worse_than(self, floor: Priority) -> bool {
        self <= floor
    }
}

/// One line from `SystemService::journal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub timestamp_ms: u64,
    pub unit: Option<String>,
    pub priority: Priority,
    pub message: String,
    /// Where this record came from, as journald recorded it.
    pub origin: Origin,
}

/// How a record reached journald - `_TRANSPORT`, which journald stamps
/// itself and a sender cannot forge.
///
/// **Not `unit.is_none()`.** That test looks equivalent and is not:
/// syslog forwarders, login sessions and anything else outside a unit's
/// cgroup carry no unit either, so a Kernel section built on it would
/// file a failed cron job under dmesg.
///
/// Three states rather than a `bool`, and the third is the point. A
/// record whose transport this could not read is not a userspace
/// record; it is one nobody has attributed. Collapsing that into `false`
/// would have the type assert an origin that was never measured - the
/// same reason `Severity` carries `Unknown` and `Snapshot::pressure` is
/// an `Option`.
///
/// Ordered so a `BTreeMap` can key on it; the order carries no meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// `_TRANSPORT=kernel` - out of the kernel's own ring buffer.
    Kernel,
    /// One of journald's userspace transports: `stdout`, `syslog`,
    /// `journal`, `audit`.
    Userspace,
    /// journald recorded no transport this reader could make sense of.
    /// Never assumed to be either of the above.
    Unknown,
}
