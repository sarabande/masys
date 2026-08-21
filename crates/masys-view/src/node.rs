//! The outline's row model: a flat, ordered list of what a buffer shows.
//! Structure only - no styling; masys-render's own row type adds that.

use masys_domain::declarative::ProfileKind;
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
    /// A profile's generations.
    ///
    /// The profile is part of the tag, unlike `Units`, which covers every
    /// state group with one variant. The difference is `fold_key`: three
    /// generation sections are on screen at once and each folds
    /// separately, so the tag has to tell them apart. A unit group's title
    /// already does that job for `Units` - `Services`, `Sockets` - while a
    /// generations heading carries a count and, for home-manager, how it
    /// is deployed, neither of which is an identity.
    Generations(ProfileKind),
    /// What pins the configuration - flake inputs or channels.
    Inputs,
    /// Store capacity and the declared maintenance policy.
    NixStore,
    /// Nix's own units, filtered out of the units masys already polls.
    NixUnits,
}

impl SectionKind {
    /// Whether a section of this kind folds under `TAB`.
    ///
    /// Named once, here, rather than restated as a match arm in each of
    /// `App::cycle_section` and `App::group_names` - which is how those
    /// two came to carry a comment asking the next reader to keep them in
    /// step by hand. `NixStore` and `System` are the deliberate absences:
    /// both always render, as the "checks actually ran" indicator of
    /// their view, so neither has a fold to cycle into.
    pub fn folds(self) -> bool {
        matches!(
            self,
            SectionKind::Units | SectionKind::Journal | SectionKind::Generations(_) | SectionKind::Inputs | SectionKind::NixUnits
        )
    }

    /// The key this section folds under, given its rendered title.
    ///
    /// **Not the title**, which is what it used to be everywhere. A title
    /// is display text, and the Nix view's titles carry readings: the
    /// Inputs heading names both revisions and the drift verdict, and the
    /// Home-manager heading says whether `nixos-rebuild` activates it.
    /// Fold Inputs, rebuild in another terminal, and the heading turns
    /// from `drift - not rebuilt` into `in sync` - which silently reopens
    /// the section the operator folded and strands the old key in the set
    /// for the rest of the session. That is the exact workflow the view
    /// exists for.
    ///
    /// `build_nix_rows` only ever argued that its titles are unique
    /// *within one call*. A fold set outlives the call, so what it
    /// actually needs is stability *across* calls, which is a different
    /// and stronger property.
    ///
    /// So the kind supplies the key wherever the kind is enough, and the
    /// `nix:` prefix keeps those keys out of the space of titles - the
    /// set is one set for the whole session, not one per buffer.
    /// `Units` and `Journal` still key on their titles, because that is
    /// where their distinguisher lives: a unit type and a calendar day
    /// both name the section rather than describe what is in it.
    pub fn fold_key(self, title: &str) -> String {
        match self {
            SectionKind::Generations(ProfileKind::System) => "nix:generations:system".to_string(),
            SectionKind::Generations(ProfileKind::Home) => "nix:generations:home".to_string(),
            SectionKind::Generations(ProfileKind::Channels) => "nix:generations:channels".to_string(),
            SectionKind::Inputs => "nix:inputs".to_string(),
            SectionKind::NixUnits => "nix:units".to_string(),
            _ => title.to_string(),
        }
    }
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
    /// One generation of one profile.
    Generation {
        generation: masys_domain::declarative::Generation,
        kind: masys_domain::declarative::ProfileKind,
        /// How long ago it was created, in milliseconds. Precomputed
        /// because the renderer has no clock - the same reason
        /// `Node::Timer` carries durations rather than timestamps.
        /// `None` where the generation's mtime could not be read, which
        /// renders as `-` rather than as an age counted from 1970.
        age_ms: Option<u64>,
    },
    /// One pinned input.
    Input {
        input: masys_domain::declarative::Input,
        /// How many days old the input is. `None` when
        /// `masys_domain::declarative::Input::last_modified_secs` is
        /// itself `None` - a channel records no such timestamp at all,
        /// and the view renders `-` rather than an age counted from
        /// 1970, the same convention `Node::Generation::age_ms` follows.
        age_days: Option<u32>,
    },
    /// A reboot the current generation needs.
    RebootPending {
        booted: Option<u64>,
        current: Option<u64>,
        kernel_changed: bool,
        initrd_changed: bool,
    },
    /// Store capacity and what is retained in it.
    NixStore {
        /// `None` before `/nix` has been matched among the sampled
        /// filesystems - reachable before the first tick, when
        /// `facts.filesystems` is still empty. Renders as `-`, not as a
        /// store measured at zero.
        used_percent: Option<f32>,
        /// Same reasoning as `used_percent`: unmeasured, not empty.
        free_bytes: Option<u64>,
        /// `None` until the profiles have been read once, and for the
        /// whole session on a host whose profile directories keep
        /// refusing. `0` is a count somebody would act on - it says this
        /// host has no generations to roll back to - and it must not be
        /// what a read failure looks like. Absent profiles do still count
        /// as zero: a host with no home-manager genuinely has no home
        /// generations, and that is an answer rather than a gap.
        system_generations: Option<u32>,
        /// Same reasoning as `system_generations`.
        home_generations: Option<u32>,
        /// `None` until the roots have been counted once. `0` reads as
        /// "nothing is pinning your store", which is the actionable claim
        /// `DeclarativeService::gc_roots` returns `Err` rather than `0`
        /// to avoid making - and the view must not put it back.
        gc_roots: Option<u32>,
    },
    /// One declared maintenance job, from its timer and its configuration.
    ///
    /// `retention` is the only field this cannot get from systemd, and is
    /// why the declarative port answers `gc_retention` at all.
    NixPolicy {
        job: String,
        unit: String,
        /// Three states, because there are three facts. `None` is "not
        /// read" and renders as `-`. `Some(None)` is "read, and this job
        /// retains nothing" - which is the whole truth for `optimise`,
        /// and for a `nix-gc.service` whose generated script sets no
        /// `--delete-older-than`. `Some(Some(..))` is the declared
        /// policy.
        ///
        /// The middle state is a finding: a gc job with no retention will
        /// collect everything but the current generation. That is exactly
        /// why it must not be what a failed read looks like.
        retention: Option<Option<String>>,
        next_in_ms: Option<u64>,
        last_ago_ms: Option<u64>,
        enabled: bool,
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
