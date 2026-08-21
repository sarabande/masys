//! The outline's row model: a flat, ordered list of what a buffer shows.
//! Structure only - no styling; masys-render's own row type adds that.

use masys_domain::finding::Finding;
use masys_domain::journal::Entry;
use masys_domain::proc_detail::ProcDetail;
use masys_domain::rate::DiskRate;
use masys_domain::rate::ProcRate;
use masys_domain::sample::{Disk, Filesystem, Proc};
use masys_domain::unit::{Unit, UnitDetail};

use crate::Overview;

/// Which section a run of rows belongs to - a machine-readable tag
/// alongside the rendered title, so fold/toggle logic (masys-app) can
/// identify a section without pattern-matching on title text, which would
/// silently drift out of sync with whatever builds the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionKind {
    /// Network interfaces in the IO buffer, beside the block devices -
    /// the same question about a different kind of pipe.
    Network,
    FailedUnits,
    Flapping,
    Pressure,
    /// `Finding::DiskCapacity`, `Finding::InodeExhaustion`, and
    /// `Finding::ReadOnlyFilesystem` all share this section - three
    /// distinct checks, but all filesystem capacity/health problems.
    Disk,
    Clock,
    OomKills,
    Degraded,
    /// Kernel-origin journal entries (no owning unit) - dmesg-class
    /// events like a GPU hang.
    Kernel,
    RecentErrors,
    /// The Units buffer's per-state groups. One kind for all of them:
    /// `kind` is a semantic tag for dispatch, and "a group of units" is
    /// the same thing whichever state it holds.
    Units,
    /// The IO buffer's filesystems.
    Filesystems,
    /// The IO buffer's block devices.
    Devices,
    /// The log view's entries.
    Journal,
    /// Always present, never hidden even with nothing else to show - it
    /// doubles as the "checks actually ran" indicator.
    System,
}

#[derive(Debug)]
pub enum Node {
    SectionHeader {
        title: String,
        kind: SectionKind,
        count: Option<u32>,
    },
    /// A triage finding, straight from masys-domain. Wrapped rather than
    /// re-declared field-by-field (unlike wagit's `Node::Commit`) because
    /// `Finding` has nine variants - duplicating that enum's shape here
    /// would be pure restatement with nothing for the view layer to add.
    Finding(Finding),
    /// One systemd unit, in the Units buffer.
    Unit {
        unit: Unit,
        /// How long it has been in its current state.
        ///
        /// An age rather than `Unit::since_ms`'s Unix timestamp, because
        /// the renderer has no clock - the same conversion
        /// `masys_domain::triage` already does before a `Finding`
        /// reaches the view.
        ///
        /// `None` when systemd recorded no transition at all. It reports
        /// an empty `StateChangeTimestamp` for units that were already in
        /// place before it started - `-.mount` is mounted by the initrd -
        /// which arrives as 0 and would otherwise date them to the Unix
        /// epoch and render as `20684d`.
        age_ms: Option<u64>,
        /// Whether this unit's detail row follows it - drawn as the
        /// `v`/`>` marker, the same way `ProcGroup` shows its fold.
        expanded: bool,
        /// How far this row is indented. Non-zero only in the Slices
        /// section, where systemd's own containment hierarchy nests -
        /// `-.slice` holds `system.slice` holds `sshd.service`.
        depth: u32,
        /// Whether this row owns a subtree, open or shut. Drawn as the
        /// fold marker, and it is what tells `tab` there is something here
        /// to collapse.
        children: bool,
    },
    /// One open unit's properties, as its own row.
    ///
    /// A separate row rather than extra lines on `Node::Unit`, for two
    /// reasons. The selection highlight covers a whole list item, so a
    /// unit and its detail sharing one item lit the entire block. And the
    /// cursor belongs on the *unit*: every verb in the view acts on the
    /// row under it, and a cursor parked on a property line would have
    /// nothing to act on. This row is never selectable.
    UnitDetail {
        unit: Unit,
        /// `None` when the read failed or has not happened. The row still
        /// appears, showing what `Unit` alone carries, rather than
        /// vanishing and making the fold look broken.
        ///
        /// Boxed because it is the largest thing any `Node` carries, and
        /// every row in the vector pays for the biggest variant: unboxed
        /// it made all 366 of them 496 bytes, twice a second, for a field
        /// at most one row has.
        detail: Option<Box<UnitDetail>>,
    },
    /// One filesystem, in the IO buffer.
    /// One mounted filesystem. `expanded` when its contents are being
    /// scanned or shown, so the row draws the `v`/`>` marker every other
    /// foldable row in masys draws.
    Filesystem {
        filesystem: Filesystem,
        expanded: bool,
    },
    /// One block device's throughput, in the IO buffer. `rate` is `None`
    /// until two samples exist - throughput is a derivative, and the
    /// buffer shows `-` rather than a zero it has not measured.
    Disk {
        disk: Disk,
        rate: Option<DiskRate>,
    },
    /// One `.timer` unit and its schedule.
    Timer {
        name: String,
        activates: String,
        /// How long until it next fires, and how long since it last did.
        /// Durations rather than timestamps, because the renderer has no
        /// clock - the same conversion `Node::Unit` and
        /// `masys_domain::triage` already do.
        next_in_ms: Option<u64>,
        last_ago_ms: Option<u64>,
        /// Whether the unit this timer runs is listed beneath it.
        children: bool,
        /// Whether the unit this timer activates is currently failed.
        ///
        /// Resolved here rather than drawn from the timer itself: a timer
        /// is `active` while the job it runs is broken, so its own state
        /// says nothing about whether the schedule is working. This is
        /// the column the whole section exists for.
        activated_failed: bool,
    },
    /// One journal line - backs both the Kernel and Recent errors
    /// sections; which one is which comes from the enclosing
    /// `SectionHeader`, not from anything on `Entry` itself.
    JournalEntry(Entry),
    /// One directory inside an opened filesystem, with everything
    /// beneath it counted.
    ///
    /// `depth` is levels below the filesystem row, for the indent.
    /// `share` is this directory's fraction of its parent, which is what
    /// the bar draws - a share of the *parent* rather than of the whole
    /// filesystem, so a bar means the same thing at every level.
    DirEntry {
        path: std::path::PathBuf,
        bytes: u64,
        depth: u32,
        share: f32,
        expanded: bool,
        has_children: bool,
    },
    /// One network interface, with its throughput when two samples have
    /// been taken. `None` means not measured yet, which the view draws
    /// differently from a measured zero.
    Interface {
        interface: masys_domain::sample::Interface,
        rate: Option<masys_domain::rate::NetRate>,
    },
    /// The System section's body - one row, rendered as several lines.
    Overview(Overview),
    /// A cgroup/unit's aggregate row in the Procs buffer - CPU/mem/io
    /// summed over everything folded beneath it, plus its sparkline
    /// history.
    ProcGroup {
        name: String,
        depth: u32,
        expanded: bool,
        cpu_percent: f32,
        mem_bytes: u64,
        /// Read and write kept apart rather than summed, because the two
        /// answer different questions - a group reading 200MB/s is warming
        /// a cache, one writing 200MB/s is filling a disk - and a renderer
        /// too narrow to show both can always add them back, while one
        /// handed the sum can never take it apart again.
        read_bytes_per_sec: f64,
        write_bytes_per_sec: f64,
        proc_count: u32,
        /// Recent history samples, oldest first - render draws these as a
        /// sparkline. Empty until the sample ring has history.
        sparkline: Vec<f32>,
    },
    /// One process. `expanded` asks the renderer to also draw the detail
    /// sub-lines (cgroup, started-at, threads, state, nice, oom_score) -
    /// all pulled from `proc`, since `Proc` already carries them. Not
    /// shown: `cmdline` and open-fd count - `masys_domain::sample::Proc`
    /// doesn't have them yet; that's masys-systemd's call to add when it's
    /// clear what's cheap to read from `/proc/[pid]`.
    Proc {
        proc: Proc,
        /// `None` until two samples exist for this pid, or when its
        /// counters went backwards because the pid was reused. The
        /// renderer draws `-` for that, never `0.0%` - "not yet measured"
        /// and "genuinely idle" have to stay distinguishable, the same
        /// rule `masys_domain::rate::derive` states for its own output.
        rate: Option<ProcRate>,
        /// Cumulative CPU seconds this process has used - `top`'s `TIME+`.
        ///
        /// Seconds rather than `Proc::cpu_ticks` because only the app
        /// layer knows the tick rate to divide by: it arrives on the
        /// snapshot, and by the time a row exists the snapshot is gone.
        /// Unlike `rate` this needs no second sample, so it is available
        /// from the first frame.
        cpu_seconds: u64,
        /// How long this process has been running, or `None` when its
        /// start time is unknown. Derived in the app layer against the
        /// same `now_ms` the tick was given, exactly as `Unit`'s `age_ms`
        /// is - the renderer has no clock of its own and must not grow
        /// one.
        ///
        /// Distinct from `cpu_seconds`: a process can be up for a week
        /// and have used four seconds of CPU. Wall-clock age is what
        /// exposes a service that keeps restarting.
        age_ms: Option<u64>,
        depth: u32,
        expanded: bool,
    },
    /// The sub-lines an open process row shows beneath itself - command
    /// line, working directory, environment, open descriptors.
    ///
    /// A row of its own rather than fields on `Proc`, mirroring
    /// `UnitDetail`: the cursor cannot rest here, and the block is
    /// several lines that are rebuilt only while the row is open.
    ///
    /// `detail` is `None` when the read failed, which for an unprivileged
    /// masys looking at another user's process is the common case. The
    /// block still draws what `Proc` alone carries.
    ProcDetail {
        proc: Proc,
        detail: Option<Box<ProcDetail>>,
    },
    Spacer,
}
