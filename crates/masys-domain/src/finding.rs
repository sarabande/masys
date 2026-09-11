#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureResource {
    Cpu,
    Io,
    Memory,
}

/// How bad a reading is, for a reading that has a rule to be judged
/// against.
///
/// Here rather than in masys-view because it is a *verdict*, of a piece
/// with [`Finding`]: `crate::triage` produces both from the same figures
/// and the same [`Thresholds`], which is what keeps a coloured segment
/// and the finding beneath it from ever disagreeing. masys-view carries
/// the value it is attached to; masys-render decides what colour it
/// draws.
///
/// `Unknown` is the variant the ticket that asked for this type did not
/// name, and it is the one this codebase's own rule requires:
/// `crate::sample::Snapshot::pressure` is an `Option` precisely so that
/// "no PSI on this kernel" cannot render as "a perfectly idle machine".
/// A severity with only three variants would put that back - the memory
/// segment on a host that has never been measured would have to claim
/// `Normal`, which is a health claim nobody made. Absent is not healthy.
/// **Not an ordering.** No `Ord` is derived and none is implied by the
/// declaration order: `Dead` is listed last because it was added last.
/// What puts the worst findings at the top of the status buffer is the
/// order of the sections, not a comparison between these. The day a
/// section needs its own rows sorted is the day to argue for an ordering
/// and derive one; until then an `Ord` here would be an interface
/// promising a judgement nobody has made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Nothing was measured, so there is no verdict. Distinct from
    /// `Normal`, and never a fallback for it.
    Unknown,
    Normal,
    Warning,
    Urgent,
    /// Something has stopped doing its job, as opposed to heading
    /// somewhere bad or needing attention now.
    ///
    /// Only a finding is ever this. A `Reading` cannot be: a reading is a
    /// figure with a threshold behind it, and "stopped" is not a point on
    /// that scale - which is why the renderer drew failed units from a
    /// colour outside `severity_style` for as long as this variant did
    /// not exist. That was the tell, and it stood for months: a fourth
    /// judgement the tree drew, documented in `theme.rs` prose, and
    /// absent from the type that exists to carry judgements.
    Dead,
}

/// A reading that would not answer, named so a row can say which.
///
/// One variant per source rather than one `Finding` per source: the
/// three carry identical fields, and three variants would be three new
/// arms on every match over `Finding` in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnreadableSource {
    /// The host-wide journal read the two journal sections feed on.
    Journal,
    /// What the platform adapter answers about this host.
    ///
    /// In practice the pending-reboot read, and only it. Boot pressure
    /// comes from the same adapter and produces no row, because its
    /// absence is already honest: `None` there means "say nothing about
    /// generations", which is true when the read failed. Only a reading
    /// whose absence would be a *claim* needs reporting.
    Platform,
    /// The unit list. An empty one would render as a host with zero
    /// units and nothing failed.
    Units,
}

impl UnreadableSource {
    /// The word the row is labelled with.
    pub fn label(self) -> &'static str {
        match self {
            UnreadableSource::Journal => "journal",
            UnreadableSource::Platform => "platform",
            UnreadableSource::Units => "units",
        }
    }
}

/// Something worth reporting, and how bad it is.
///
/// The severity is carried rather than worked out by each reader, and
/// [`Finding::new`] is the only way to attach one - the field is private,
/// so no module outside this one can write a `Finding` literal at all.
/// That is the whole point of the type: **how bad a finding is has
/// exactly one statement**, in `FindingKind`'s private `severity`, and
/// the compiler refuses every other. Private deliberately - a caller that
/// could ask a kind for its severity could also ask and then ignore the
/// answer, which is the arrangement this replaces.
///
/// It had four. The renderer picked a `theme.severity_*` field per
/// variant, `theme.rs` documented the mapping again in prose, and the
/// overview drew the same PSI reading through `Reading` - so a memory
/// stall over the full threshold was light-red on the System line and
/// yellow on the finding row directly beneath it, which is the more
/// detailed statement of the same figure being the quieter one.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    severity: Severity,
    pub kind: FindingKind,
}

impl Finding {
    /// A finding of this kind, judged.
    pub fn new(kind: FindingKind) -> Finding {
        Finding {
            severity: kind.severity(),
            kind,
        }
    }

    pub fn severity(&self) -> Severity {
        self.severity
    }
}

/// Triage's output vocabulary - what the status buffer renders one row
/// per. Mostly produced by `crate::triage`'s functions from
/// `Unit`/`Snapshot` facts plus `Thresholds`.
///
/// Mostly, because [`FindingKind::Unreadable`] is not about the host
/// and cannot be: it reports a read that failed, which this crate never
/// performs. The session builds that one and appends it, the way it
/// already fills in a failed unit's last words. Everything downstream -
/// the sections, the glyphs, the filter, the jump - treats it as any
/// other finding, which is the point of it being one.
#[derive(Debug, Clone, PartialEq)]
pub enum FindingKind {
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
    /// The one kind whose severity is a matter of degree, so the one
    /// that carries its own.
    ///
    /// A figure beside the verdict that figure earned, which is what
    /// `masys_view::Reading` is - and the shape the overview's pressure
    /// segment has always had. `crate::triage::pressure` computed this
    /// and threw it away, keeping only the fact that *some* threshold had
    /// been crossed; the row then drew a fixed colour that could not tell
    /// the two apart.
    Pressure {
        resource: PressureResource,
        some_avg60: f32,
        full_avg60: Option<f32>,
        /// What [`crate::triage::pressure_severity`] made of the two
        /// figures above, against the thresholds in force when they were
        /// read. Never recomputed downstream: nothing outside
        /// `crate::triage` has the thresholds, and a second comparison
        /// against a guessed one is how the row and the segment came to
        /// disagree.
        severity: Severity,
    },
    /// Both extra fields are `Some` only for `/boot`, and only when
    /// `PlatformService::boot_pressure` answered - they are what "why
    /// /boot is full" needs that a plain percentage doesn't carry.
    ///
    /// `generations` is additionally `None` on a host whose `/boot` is not
    /// generation-based, which is every host but NixOS. `Some(0)` is a
    /// different claim and a real finding: zero generations means there is
    /// nothing to roll back to.
    ///
    /// `reclaimable_bytes` is the actionable half and the only half a
    /// Debian host has - what `apt autoremove` there, or a garbage
    /// collection on NixOS, would free.
    DiskCapacity {
        mount_point: String,
        used_percent: f32,
        free_bytes: u64,
        generations: Option<u32>,
        reclaimable_bytes: Option<u64>,
    },
    InodeExhaustion {
        mount_point: String,
        inode_used_percent: f32,
    },
    ReadOnlyFilesystem {
        mount_point: String,
    },
    /// Something the kernel logged at error priority, with every
    /// occurrence of the same line counted into one.
    ///
    /// No unit, because a kernel record has none - it did not come from
    /// anybody's cgroup. That is also why this cannot jump anywhere: it
    /// names nothing masys has a row for.
    KernelError {
        /// The most recent occurrence, verbatim. The *shape* is what
        /// grouped them; the message shown is one that really appeared,
        /// rather than a masked pattern nobody logged.
        message: String,
        /// How many lines of this shape are in the window. `1` is the
        /// common case and reads as an ordinary finding.
        count: u32,
        /// How long ago the most recent one was, not when it happened -
        /// the renderer has no clock, the same reason
        /// `FailedUnit::since_ms` is an age.
        age_ms: u64,
    },
    /// An error-priority line from userspace, counted the same way.
    ///
    /// `unit` is `None` for a sender outside any unit - a syslog
    /// forwarder, a login session - which is a row that reports but
    /// cannot be jumped from.
    RecentError {
        unit: Option<String>,
        message: String,
        count: u32,
        age_ms: u64,
    },
    /// A source that would not answer.
    ///
    /// The glossary's *unreadable*, and not its *unmeasured*: a CPU rate
    /// before the second sample is unmeasured and nothing went wrong,
    /// where this is a read that failed and an operator can act on it.
    /// A finding about the instrument rather than the machine, and the
    /// only one here that is.
    ///
    /// It exists because this buffer's premise - no rows means nothing
    /// is wrong - is a promise it can only keep if it can also say when
    /// it was unable to look. Readings that fail are kept at their last
    /// good value, so what they feed stays populated for a while; once
    /// that goes stale, silence would otherwise be indistinguishable
    /// from a quiet host.
    Unreadable {
        /// Which reading failed. An enum rather than a string: the row
        /// draws it as its label, and a renderer must not be handed free
        /// text to align a column on.
        source: UnreadableSource,
        /// What the read said went wrong, so there is something to act
        /// on rather than only that something failed.
        reason: String,
        /// How long this source has been failing, from the first of the
        /// current run rather than the latest. One failed read on a busy
        /// host is noise and four minutes of them is a problem; an age
        /// is what tells them apart. Reset by a read that succeeds, so a
        /// recovered outage is not still being counted.
        since_ms: u64,
    },
    ClockUnsynchronized,
    /// A disk reports its own SMART self-assessment as failing.
    ///
    /// No fields, the way `ClockUnsynchronized` has none: the port answers
    /// one `Option<bool>` for the host, so there is no device name to
    /// carry. Naming the disk would need a per-device read, which is a
    /// buffer rather than a field and nobody has asked for one.
    ///
    /// Absent where masys could not tell, which is most hosts. A finding
    /// that appeared whenever `smartctl` was missing would be a report
    /// about masys, not about the machine.
    SmartFailing,
    /// A core spent a meaningful share of the last interval held below
    /// the clock it asked for, by the thermal governor.
    ///
    /// The share, not the temperature. A hot machine that is keeping up
    /// is doing its job, and the reading that matters is the one where
    /// the heat has started costing work - which is what the kernel's
    /// throttle accounting measures directly and a thermometer only
    /// implies. It is why this carries no degrees: masys would have to
    /// know each machine's design limit to say whether a number was
    /// bad, and the governor already knows.
    ///
    /// Absent where the kernel does not account for throttling, which is
    /// every non-x86 host - the same rule `SmartFailing` follows.
    ThermalThrottling {
        percent: f32,
    },
    OomKill {
        pid: u32,
        comm: String,
        timestamp_ms: u64,
    },
    SystemDegraded {
        failed_units: u32,
    },
}

impl FindingKind {
    /// How bad a finding of this kind is.
    ///
    /// **Exhaustive, and the only statement of this in the tree.** A
    /// fifteenth kind does not compile until somebody has said how bad it
    /// is, which is the answer no wildcard could supply and no reviewer
    /// reliably notices missing - the renderer's per-variant colour picks
    /// were exactly that, fourteen decisions each of which looked
    /// obviously right beside its neighbours.
    ///
    /// Constant for every kind but one. Whether a unit has failed, or a
    /// clock is adrift, or a disk expects to die, is not a matter of
    /// degree: the finding exists precisely because the thing is true.
    /// `Pressure` is the exception because its verdict comes from a
    /// figure and a threshold, so it carries what
    /// [`crate::triage::pressure_severity`] gave it rather than a
    /// constant named here.
    ///
    /// Never `Normal` and never `Unknown`. Both would be a row on the
    /// status buffer claiming there is nothing to report, and this buffer
    /// draws nothing at all when there is nothing to report - that is its
    /// whole premise. `Unknown` in particular belongs to a reading that
    /// was never taken, and a finding is the opposite: something masys
    /// looked at and did not like.
    fn severity(&self) -> Severity {
        match self {
            // The only kind that is dead rather than heading there. A
            // failed unit has stopped; everything else on this buffer is
            // a machine still running and doing something wrong.
            FindingKind::FailedUnit { .. } => Severity::Dead,
            FindingKind::Pressure { severity, .. } => *severity,
            // Heading somewhere bad, not yet arrived. A flapping unit is
            // still coming back up, a filling disk still has room, a
            // throttled core is still working - slower - and an
            // unreadable source is a fault in the instrument rather than
            // in the machine, which is why it warns rather than alarms.
            FindingKind::FlappingUnit { .. }
            | FindingKind::DiskCapacity { .. }
            | FindingKind::InodeExhaustion { .. }
            | FindingKind::ReadOnlyFilesystem { .. }
            | FindingKind::ThermalThrottling { .. }
            | FindingKind::Unreadable { .. } => Severity::Warning,
            // Needs attention now. An unsynchronised clock breaks TLS
            // validation and journal ordering silently; a disk reporting
            // its own SMART assessment as failing is the one row whose
            // useful response is to stop reading this tool; an error in
            // either journal, an OOM kill and a degraded system are the
            // host saying something already went wrong.
            FindingKind::ClockUnsynchronized
            | FindingKind::SmartFailing
            | FindingKind::KernelError { .. }
            | FindingKind::RecentError { .. }
            | FindingKind::OomKill { .. }
            | FindingKind::SystemDegraded { .. } => Severity::Urgent,
        }
    }
}

/// Triage's tunable inputs. The design's Keymap and config section says
/// thresholds live in config, not in code - this is that seam: the
/// `masys` binary owns loading `~/.config/masys/config.toml` and building
/// one of these, and every function below takes it as a parameter rather
/// than hardcoding a number. `Default` is the base that file overrides
/// key by key, so it is what a host with no config file evaluates
/// against - and what any one threshold falls back to when the file gives
/// it a value masys will not take.
#[derive(Debug, Clone, PartialEq)]
pub struct Thresholds {
    pub flapping_restart_count: u32,
    pub flapping_window_ms: u64,
    pub psi_some_avg60_percent: f32,
    pub psi_full_avg60_percent: f32,
    pub disk_used_percent: f32,
    pub inode_used_percent: f32,
    /// How far back the Status buffer's two journal sections look.
    ///
    /// The design's mockup says fifteen minutes, and that is what makes
    /// them *recent* errors: a window wide enough to include this
    /// morning's noise would report a machine that has since recovered.
    pub journal_window_ms: u64,
    /// How many rows either journal section may show.
    ///
    /// A bound rather than a threshold, and it is here for the same
    /// reason the thresholds are: one service logging forty distinct
    /// failures must not push every other finding off the buffer, and
    /// what counts as too many is a matter of screen height and taste.
    pub journal_rows_per_section: u32,
    /// What share of an interval spent thermally throttled is worth
    /// reporting.
    ///
    /// Ten percent rather than anything at all, because "anything" is
    /// wrong on the hardware this runs on: a laptop that boosts above
    /// its sustained clock throttles in brief bursts as a matter of
    /// normal operation, and a finding on every one of them would be a
    /// row that is always present and therefore never read. Ten percent
    /// of a tick is the machine losing time it would otherwise be
    /// working.
    pub thermal_throttled_percent: f32,
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
            journal_window_ms: 900_000,
            journal_rows_per_section: 5,
            thermal_throttled_percent: 10.0,
        }
    }
}
