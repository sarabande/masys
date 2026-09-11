//! Builds the status buffer's rows from a triage pass - the masys-specific
//! analogue of wagit-app's `outline::build`.

use std::collections::{BTreeMap, HashMap};

use crate::nix_buffer::keep_last_good;
use masys_domain::error::MasysError;
use masys_domain::finding::{
    Finding, FindingKind, PressureResource, Severity, Thresholds, UnreadableSource,
};
use masys_domain::journal::{Entry, Priority};
use masys_domain::platform::PendingReboot;
use masys_domain::rate::{self, Throughput};
use masys_domain::sample::Snapshot;
use masys_domain::service::{PlatformService, SystemService};
use masys_domain::triage;
use masys_domain::unit::Unit;
use masys_view::presentation::Presentation;
use masys_view::{Node, Overview, Reading, SectionKind};

/// How far back to look for a failed unit's last words. Small: the line
/// that explains a failure is almost always among the last few the unit
/// printed, and this runs once per newly-failed unit.
const FAILURE_LOG_LINES: usize = 30;

/// How long a SMART answer is kept before the port is asked again.
///
/// **The one read here that is not taken every tick.** `App::tick` argues
/// for one interval for everything "until a buffer needs otherwise", and
/// defers the one genuinely expensive read it already has; this is the
/// second. Every other reading this buffer takes is a file in `/proc` or
/// `/sys`. A SMART check spawns one `smartctl` per disk, and the disks
/// are asked one after another.
///
/// **Timed, on 2026-09-01**, as medians over seven runs on one host: 19 ms
/// for its SATA SSD with smartmontools installed and running as root;
/// 14 ms where the tool is present and the process cannot open the
/// device; 0.3 ms where there is no `smartctl` at all, which is most
/// development machines and is why this cost has never been felt.
///
/// Put against the tick rather than against nothing, which is the
/// comparison worth having: the readings a tick takes anyway are 45 ms
/// here - 24 ms for the sample, 20 ms for the units - so the one tick in
/// 150 that also takes the SMART read costs 64 ms instead of 45. That is
/// not a stall at a two-second cadence, so the read stays on the tick
/// thread rather than being gated on visibility the way the Log and
/// Packages buffers gate theirs.
///
/// What the measurement does not cover, for whoever doubts this next: a
/// host with more than one disk, and a disk with platters - this one has
/// neither. The cost is per-disk and sequential, so four disks is four
/// times one, and `smart::args_for` passes `-n standby`, so a spun-down
/// disk answers without being woken. That last is the case that would
/// have cost seconds rather than milliseconds, and it is the one still
/// untested.
///
/// Hardcoded rather than config, which needs no exception: `masys-design`
/// puts thresholds in config and records that sample cadences are still
/// hardcoded, and this is a cadence. Not `JOURNAL_FLOOR`'s case, which is
/// a knob whose wrong setting would silently disable a feature - every
/// setting of this one works, and only the cost changes.
///
/// Five minutes because a disk's self-assessment changes on the timescale
/// of a disk dying rather than of a screen refreshing. Measured on
/// `Snapshot::taken_at_ms`, the clock that never steps.
const SMART_INTERVAL_MS: u64 = 300_000;

/// The Status buffer: one triage pass, and what frames it.
///
/// The last two fields of `App`'s `Facts` plus two of `App`'s own, which
/// is what `Facts` was reduced to before it was deleted. This buffer is
/// the aggregator by nature - it reads the sample, the units and both
/// platform answers - so it is the one whose `refresh` takes the most,
/// and the one whose `rows` takes nothing at all.
///
/// `SystemService` and `PlatformService` are ports and are handed to
/// [`StatusBuffer::refresh`], the way `IoBuffer` takes its `DirScanner`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct StatusBuffer {
    /// What the last triage pass found. Empty on a healthy machine, which
    /// is the buffer's whole premise.
    pub findings: Vec<Finding>,
    /// What a host *is*, which frames everything below it. `None` before
    /// the first tick - the state only a test can observe, since the
    /// binary ticks once before its first draw.
    pub overview: Option<Overview>,
    /// Triage's tunable inputs.
    ///
    /// The design says thresholds live in config rather than in code, and
    /// this is where the config loader puts them: `masys::load_config`
    /// parses the `[thresholds]` table and the composition root hands
    /// them over through `App::set_thresholds`.
    ///
    /// They are `Thresholds::default()` only where no config file
    /// supplies them, which is a host with nothing to say rather than a
    /// feature that is missing. This comment claimed "nothing loads one
    /// yet" until 2026-08-30, having outlived the loader by some
    /// distance - and cited a README passage that had gone stale too.
    pub thresholds: Thresholds,
    /// Why each failed unit failed, keyed by unit and roughly when.
    ///
    /// Here rather than on the systemd buffer, though it is read out of a
    /// unit's journal: nothing draws it but a `FindingKind::FailedUnit`, and
    /// findings are this buffer's.
    pub reasons: HashMap<(String, u64), Option<String>>,
    /// When each source's current run of failed reads began, on
    /// `Snapshot::taken_at_ms`'s clock rather than the wall clock.
    ///
    /// The instant rather than a count, because what the finding shows
    /// is an age and an age needs a start. Cleared by a read that
    /// succeeds, so a recovered outage does not go on being counted and
    /// the next failure starts its own clock.
    ///
    /// **Monotonic, and that is load-bearing.** This is the only age in
    /// masys measured between two of its own ticks rather than against a
    /// timestamp something else recorded, so it is the only one that can
    /// use a clock that never steps. `now_ms` is wall-clock, and an NTP
    /// correction - the very thing this buffer reports as
    /// `ClockUnsynchronized` - would otherwise make the row claim an age
    /// nobody measured: backwards reads as "just now" for as long as the
    /// step lasted, forwards adds the whole step at once.
    pub failing_since: BTreeMap<UnreadableSource, u64>,
    /// The last pending-reboot answer this buffer got.
    ///
    /// Held rather than fetched-and-dropped, because absence here is a
    /// claim: the overview draws no reboot segment when there is none,
    /// so a failed read that became `None` would render as a host with
    /// nothing pending.
    pub pending_reboot: Option<PendingReboot>,
    /// The journal window this buffer last read.
    ///
    /// Held rather than fetched-and-dropped so that a tick whose read
    /// fails keeps the last good one. They are re-judged against the new
    /// clock every tick, so a persistent failure empties the sections by
    /// the entries ageing out of the window - which is slower than
    /// blanking them and says something true the whole way down.
    pub entries: Vec<Entry>,
    /// The last SMART answer, and the reading that produced it.
    ///
    /// Held rather than fetched every tick, because a disk's
    /// self-assessment changes on the timescale of a disk dying rather
    /// than of a screen refreshing: it is re-read every five minutes,
    /// measured on `Snapshot::taken_at_ms`.
    ///
    /// `None` inside the `Some` is a real answer and the common one -
    /// masys could not tell - so the outer `Option` means only "not
    /// asked yet", and the two must not be collapsed.
    ///
    /// The interval used to be cited here as a link to the private
    /// constant that holds it. A `pub` field's doc is read by somebody
    /// who has the crate from crates.io and nothing else, and rustdoc
    /// does not publish a private item, so the link resolved to nothing
    /// for exactly the reader it was written for - the same defect
    /// `citations.rs` catches for documents, in the one shape it does
    /// not look at.
    pub smart: Option<Option<bool>>,
    /// When [`Self::smart`] was last read, on `Snapshot::taken_at_ms`.
    pub smart_read_at_ms: Option<u64>,
}

/// What one tick presents to this buffer.
///
/// A struct rather than six more parameters, and it earns its keep twice
/// over: `refresh` had grown past what anybody can read at a call site,
/// and these six always travel together because they all describe the
/// same instant. The two `Throughput`s in particular are meaningless
/// apart from the pair of snapshots they were derived from.
pub struct Tick<'a> {
    /// The previous tick's sample, or `None` on the first. Every rate on
    /// the overview is derived against it, and absent means no rate -
    /// never a zero one.
    pub previous: Option<&'a Snapshot>,
    pub snapshot: &'a Snapshot,
    pub units: &'a [Unit],
    /// Why the unit list could not be re-read this tick, if it could
    /// not - in which case `units` above is the last good one.
    ///
    /// Carried rather than read here because `units()` is the session's
    /// call, made once for every buffer. An empty list would render as a
    /// host with zero units and nothing failed, which is the shape this
    /// finding exists to prevent.
    pub units_unreadable: Option<&'a MasysError>,
    /// Derived by the IO buffer and handed over, not summed here. Two
    /// sums over one pair of samples would agree until the first time
    /// one of them learned to skip a device - and one of them does skip
    /// loopback.
    pub net_throughput: Option<Throughput>,
    pub disk_throughput: Option<Throughput>,
    pub now_ms: u64,
}

impl StatusBuffer {
    /// One triage pass, and the overview that frames it.
    ///
    /// Both platform reads happen here rather than in `App::tick`,
    /// because this is the only buffer that consumes either: a pending
    /// reboot is an overview field, and boot pressure is a triage input.
    /// Neither can abort the tick any more, and this returns nothing:
    /// after the two platform reads learned to degrade, no call in here
    /// can fail. A `Result` that is only ever `Ok` is a lie the compiler
    /// makes every caller handle - and the `?` that produced the freeze
    /// this fixes got there exactly that way. `NixBuffer::refresh` and
    /// `PackagesBuffer::refresh` have always been shaped like this.
    pub fn refresh(
        &mut self,
        system: &dyn SystemService,
        platform: &dyn PlatformService,
        tick: Tick<'_>,
    ) {
        let Tick {
            previous,
            snapshot,
            units,
            units_unreadable,
            net_throughput,
            disk_throughput,
            now_ms,
        } = tick;
        // Neither read aborts the pass any more. A tick that returned
        // here left every finding and the whole overview standing from
        // the previous one, while the header's timestamp kept advancing
        // - a buffer that looked live and was not.
        //
        // How each degrades depends on what its absence already means.
        // `boot_pressure` can simply go missing: `None` there already
        // means "say nothing about generations", which is true when the
        // read failed, and only `/boot` findings are the poorer for it.
        let boot_pressure = platform.boot_pressure().ok().flatten();
        // `pending_reboot` cannot. `None` there renders as *no reboot
        // pending*, which is a claim - so it keeps its last good value
        // and reports the failure. A value with no window never expires
        // on its own, and the row is the only thing that ages it.
        let reboot_read = platform.pending_reboot();
        let mut unreadable: Vec<Finding> = Vec::new();
        unreadable.extend(self.unreadable_finding(
            UnreadableSource::Platform,
            reboot_read.as_ref().err().map(|why| why.to_string()),
            snapshot.taken_at_ms,
        ));
        keep_last_good(&mut self.pending_reboot, reboot_read);
        let pending_reboot = self.pending_reboot.clone();

        // The window this buffer reports on, read once and handed to a
        // rule that performs no I/O of its own.
        //
        // An unreadable journal is not a failed tick: the host still has
        // everything else worth showing, and a Status buffer that
        // vanished because journald was busy would be worse than one
        // missing two sections.
        //
        // But it is not an empty journal either, and this was
        // `unwrap_or_default()` until review caught it. A failed read
        // became `vec![]`, which produced no findings, which hid both
        // sections - so a host whose journal cannot be read looked
        // exactly like one with nothing to report. `keep_last_good` is
        // what this tree already does with a reading that failed: the
        // entries stay, the rule runs over them again with the new
        // clock, and they age out of the window on their own rather than
        // being replaced by a claim that there was nothing there.
        let read = system.journal(
            triage::journal_since(&self.thresholds, now_ms),
            triage::JOURNAL_FLOOR,
        );
        unreadable.extend(self.unreadable_finding(
            UnreadableSource::Journal,
            read.as_ref().err().map(|why| why.to_string()),
            snapshot.taken_at_ms,
        ));
        unreadable.extend(self.unreadable_finding(
            UnreadableSource::Units,
            units_unreadable.map(|why| why.to_string()),
            snapshot.taken_at_ms,
        ));
        keep_last_good(&mut self.entries, read);
        // Taken and put back below, rather than borrowed: `evaluate`
        // wants the entries while `explain_failures` wants `&mut self`,
        // and the two cannot overlap. Free, where cloning the window
        // would not be.
        let entries = std::mem::take(&mut self.entries);
        let smart = self.smart_health(system, snapshot.taken_at_ms);
        // No previous sample is no reading, which is the first tick of
        // every session: a counter read once says how long this machine
        // has been throttled since boot, and nothing at all about the
        // interval that just passed.
        let thermal_throttled_percent =
            previous.and_then(|previous| rate::derive_thermal(previous, snapshot));
        let mut findings = triage::evaluate(&triage::Tick {
            snapshot,
            units,
            boot_pressure: boot_pressure.as_ref(),
            entries: &entries,
            smart,
            thermal_throttled_percent,
            thresholds: &self.thresholds,
            now_ms,
        });
        self.explain_failures(system, &mut findings, now_ms);
        findings.extend(unreadable);
        self.findings = findings;
        self.entries = entries;

        self.overview = Some(Overview {
            machine: snapshot.machine.clone(),
            system_state: snapshot.system_state,
            // Absent only when nothing has ever counted them: an empty
            // list plus a failed read is a host masys has not managed to
            // ask, where an empty list on its own is a host with no
            // units - which is barely possible, and still an answer.
            unit_count: match units.is_empty() && units_unreadable.is_some() {
                true => None,
                false => Some(units.len() as u32),
            },
            load_1: snapshot.load.map(|l| l.one),
            load_5: snapshot.load.map(|l| l.five),
            load_15: snapshot.load.map(|l| l.fifteen),
            uptime_secs: snapshot.uptime_secs,
            net_throughput,
            disk_throughput,
            // The figure and the verdict come from different readings on
            // purpose: utilisation says what the CPU is doing, PSI says
            // whether that is a problem. A busy machine keeping up is not
            // a finding, and must not be a colour either.
            cpu_percent: previous
                .and_then(|prev| rate::derive_cpu(prev, snapshot))
                .map(|value| Reading {
                    value,
                    severity: severity_of(snapshot, PressureResource::Cpu, &self.thresholds),
                }),
            cpu_pressure: pressure_reading(snapshot, PressureResource::Cpu, &self.thresholds),
            io_pressure: pressure_reading(snapshot, PressureResource::Io, &self.thresholds),
            memory_pressure: pressure_reading(snapshot, PressureResource::Memory, &self.thresholds),
            mem_used_bytes: snapshot.memory.map(|m| Reading {
                value: m.used_bytes,
                severity: severity_of(snapshot, PressureResource::Memory, &self.thresholds),
            }),
            mem_total_bytes: snapshot.memory.map(|m| m.total_bytes),
            zram_percent: snapshot.memory.and_then(|m| m.zram_percent),
            swap_free_bytes: snapshot.memory.map(|m| m.swap_free_bytes),
            clock_synced: Reading {
                value: snapshot.clock_synced,
                // Urgent, and the same verdict `FindingKind::ClockUnsynchronized`
                // already draws: drift breaks TLS validation and journal
                // ordering without anything announcing it.
                severity: match snapshot.clock_synced {
                    true => Severity::Normal,
                    false => Severity::Urgent,
                },
            },
            // Three states, and the absent one is not a severity. A host
            // masys cannot ask - no smartctl, no root, a virtual disk -
            // answers `None` here and draws no segment at all, which is
            // what keeps it distinguishable from a disk that answered and
            // is fine. Rendering unknown as healthy would be a claim
            // about a reading nobody took, and the one an operator would
            // act on.
            smart_ok: smart.map(|value| Reading {
                value,
                // Urgent, and the same verdict `FindingKind::SmartFailing`
                // draws: a disk saying it expects to fail is the one
                // report whose useful response is to stop reading this
                // tool and go and copy something.
                severity: match value {
                    true => Severity::Normal,
                    false => Severity::Urgent,
                },
            }),
            // The reason is the finding; that one exists at all is the
            // judgement. A pending reboot is a standing condition rather
            // than a fault, which is `Warning` and not `Urgent` - the
            // machine is working, it is just not running what it is
            // configured to run.
            pending_reboot: pending_reboot.map(|value| Reading {
                value,
                severity: Severity::Warning,
            }),
        });
    }

    /// The SMART answer for this tick, asking the port only when the held
    /// one has aged out.
    ///
    /// Measured on the sample's clock rather than the wall clock, for the
    /// reason [`Self::failing_since`] gives: a wall clock that steps
    /// backwards under an NTP correction would hold a stale answer for as
    /// long as the step lasted, and one that steps forwards would throw
    /// away a good one.
    fn smart_health(&mut self, system: &dyn SystemService, taken_at_ms: u64) -> Option<bool> {
        let due = match self.smart_read_at_ms {
            None => true,
            Some(last) => taken_at_ms.saturating_sub(last) >= SMART_INTERVAL_MS,
        };
        if due {
            self.smart = Some(system.smart_health());
            self.smart_read_at_ms = Some(taken_at_ms);
        }
        self.smart.flatten()
    }

    /// Records how one source's read went, and reports it if it is
    /// failing.
    ///
    /// One place for all three, because the rule is the same wherever a
    /// reading comes from: `None` clears the run, `Some(why)` starts or
    /// continues it, and what comes back is the row - if there is one -
    /// carrying how long it has been going.
    ///
    /// The age counts from the first failure *observed*, which is not
    /// quite the same as the first failure. A tick that could not sample
    /// never reaches here at all, so a run that spans one reports the
    /// whole span - including the part where nothing asked. That
    /// overstates rather than understates, which is the safe direction
    /// for a caveat.
    ///
    /// `at_ms` is the sample's clock, not the wall clock, for the reason
    /// [`Self::failing_since`] gives at length.
    fn unreadable_finding(
        &mut self,
        source: UnreadableSource,
        reason: Option<String>,
        at_ms: u64,
    ) -> Option<Finding> {
        let Some(reason) = reason else {
            self.failing_since.remove(&source);
            return None;
        };
        let since = *self.failing_since.entry(source).or_insert(at_ms);
        Some(Finding::new(FindingKind::Unreadable {
            source,
            reason,
            since_ms: at_ms.saturating_sub(since),
        }))
    }

    /// Every row this buffer shows, and nothing crosses to build them.
    ///
    /// Empty before the first tick, because there is genuinely nothing to
    /// show - not a System section reporting a machine nobody sampled.
    pub fn rows(&self) -> Vec<Node> {
        match self.overview.clone() {
            Some(overview) => layout(
                &self.findings,
                overview,
                self.thresholds.journal_rows_per_section,
            ),
            None => Vec::new(),
        }
    }

    /// Fills in *why* each failed unit failed, from its own journal.
    ///
    /// The status buffer could say a unit had failed and with what exit
    /// code, but not what went wrong - and "exit 6" against "curl: (6)
    /// Could not resolve host" is the difference between knowing
    /// something broke and knowing what to do about it.
    ///
    /// Answers are cached against roughly when the unit failed, so a unit
    /// that has been failed for hours costs one query rather than one
    /// every two seconds. A unit that fails again gets a newer age and so
    /// a fresh answer.
    fn explain_failures(
        &mut self,
        system: &dyn SystemService,
        findings: &mut [Finding],
        now_ms: u64,
    ) {
        for finding in findings.iter_mut() {
            let FindingKind::FailedUnit {
                unit,
                since_ms,
                reason,
                ..
            } = &mut finding.kind
            else {
                continue;
            };
            // `since_ms` on a finding is an *age*, not a timestamp - it
            // advances every tick - so keying on it directly gave a fresh
            // key every two seconds and the cache never hit once. Undoing
            // the subtraction `triage::failed_units` did recovers the
            // instant the unit failed, which is the thing that stays put
            // while a unit stays broken and moves when it breaks again.
            let key = (unit.clone(), now_ms.saturating_sub(*since_ms) / 1000);
            if let Some(cached) = self.reasons.get(&key) {
                *reason = cached.clone();
                continue;
            }
            let found = system
                .unit_journal(unit, FAILURE_LOG_LINES)
                .ok()
                .and_then(|entries| last_words(&entries, unit));
            self.reasons.insert(key, found.clone());
            *reason = found;
        }
        self.forget_stale_reasons();
    }

    /// Drops the whole cache once it passes its bound.
    ///
    /// Whole rather than trimmed. There is no least-useful entry to
    /// evict, since every one of them is a unit that failed, and
    /// re-reading a journal costs one query per still-failed unit -
    /// which is what this cache exists to make rare rather than
    /// impossible.
    fn forget_stale_reasons(&mut self) {
        if self.reasons.len() > 64 {
            self.reasons.clear();
        }
    }
}

/// How much trouble one resource is in, for the segment that reports it.
///
/// From PSI rather than from any percentage the segment happens to show,
/// and by way of `triage::pressure_severity` rather than a comparison
/// written here - this is the same call, against the same `Thresholds`,
/// that decides whether a `FindingKind::Pressure` for that resource exists at
/// all. That is what makes the colour and the finding two views of one
/// judgement instead of two judgements that happen to agree today.
///
/// The resource is a parameter so that each segment is coloured by its
/// own: CPU pressure has nothing to say about memory, and a single
/// "system health" verdict spread across three readings would be a claim
/// none of them made.
///
/// `Unknown` where the kernel reports no PSI. The alternative is
/// `Normal`, which would be masys calling a resource fine on the strength
/// of a reading it never took. This is the one place that turns an absent
/// reading into a severity, so it is the one place that decision can be
/// got wrong.
///
/// The judgement itself comes from [`pressure_reading`] rather than being
/// made again here. Both were once written out against
/// `snapshot.pressure`: the same `psi_lines` call, the same
/// `pressure_severity` call. That is one rule with two statements of it,
/// and the way two statements disagree is exactly the failure the
/// paragraph above argues against.
fn severity_of(
    snapshot: &Snapshot,
    resource: PressureResource,
    thresholds: &Thresholds,
) -> Severity {
    pressure_reading(snapshot, resource, thresholds)
        .map_or(Severity::Unknown, |reading| reading.severity)
}

/// One resource's PSI figure, judged by itself.
///
/// `None` on a kernel with no PSI, which is what drops the whole pressure
/// line from the overview rather than printing three zeroes - the figure
/// is the reading, so an absent reading has nothing to show.
fn pressure_reading(
    snapshot: &Snapshot,
    resource: PressureResource,
    thresholds: &Thresholds,
) -> Option<Reading<f32>> {
    let psi = snapshot.pressure.as_ref()?;
    let (some_avg60, full_avg60) = triage::psi_lines(psi, resource);
    Some(Reading {
        value: some_avg60,
        severity: triage::pressure_severity(some_avg60, full_avg60, thresholds),
    })
}

/// One finding category: its section title, its `SectionKind`, which
/// findings belong under it, and whether it bounds how many it draws.
///
/// **The title stays here, and `SectionKind::title()` is not a method
/// that can exist.** It looks like one - a section's name is a property
/// of its kind, the way `Buffer::title` is a property of a buffer - and
/// it was proposed on exactly that reading. `SectionKind` has twenty-one
/// variants across every buffer, and several have no static name at all:
/// the Log buffer heads its sections with `day_label(day)` and with
/// `format!("{unit} - nothing in the journal")`, and the Nix buffer's
/// three come from profile names read off the host. A `title()` covering
/// those would have to return an `Option` that is `None` for a third of
/// the enum, which is not a property, or take arguments that only some
/// arms use, which is not a method.
///
/// So the ten *status* sections keep their names beside the two other
/// things this table decides about them. Recorded because the move reads
/// as obviously right until you count the variants.
struct Section {
    title: &'static str,
    kind: SectionKind,
    /// Whether this section shows at most
    /// `Thresholds::journal_rows_per_section` rows.
    ///
    /// Only the two journal sections do. Every other section's length is
    /// bounded by the machine - a host has so many filesystems and so
    /// many units - while a journal can produce distinct error shapes
    /// without limit, and one service having a bad day must not push
    /// every other finding off the buffer.
    ///
    /// The count in the heading stays the true total when this bites, so
    /// `Recent errors (40)` above five rows says both what there is and
    /// that you are not seeing all of it. A count of the survivors would
    /// be a number nobody measured.
    capped: bool,
}

const SECTIONS: &[Section] = &[
    // First, because it qualifies everything below it. If the unit list
    // could not be read, `Failed units` being absent means nothing, and
    // an operator scanning top-down should meet the caveat before the
    // claims it applies to rather than six sections after them.
    Section {
        title: "Unreadable",
        kind: SectionKind::Unreadable,
        capped: false,
    },
    Section {
        title: "Failed units",
        kind: SectionKind::FailedUnits,
        capped: false,
    },
    Section {
        title: "Flapping",
        kind: SectionKind::Flapping,
        capped: false,
    },
    Section {
        title: "Pressure",
        kind: SectionKind::Pressure,
        capped: false,
    },
    Section {
        title: "Disk",
        kind: SectionKind::Disk,
        capped: false,
    },
    Section {
        title: "Clock",
        kind: SectionKind::Clock,
        capped: false,
    },
    Section {
        title: "Kernel",
        kind: SectionKind::Kernel,
        capped: true,
    },
    // **The complement of Kernel, not a claim of userspace origin.**
    // Journald attributes a record's transport or it does not, and
    // `Origin::Unknown` is what the third case is called. Those records
    // land here, which is why the section is named for *when* the errors
    // happened rather than for where they came from: "Userspace" would
    // be a claim about a record whose origin nobody could establish, and
    // a third section for the handful that cannot be attributed would be
    // a heading an operator learns to ignore. `triage::journal_findings`
    // makes the same argument at the split.
    Section {
        title: "Recent errors",
        kind: SectionKind::RecentErrors,
        capped: true,
    },
    Section {
        title: "OOM kills",
        kind: SectionKind::OomKills,
        capped: false,
    },
    Section {
        title: "Degraded",
        kind: SectionKind::Degraded,
        capped: false,
    },
];

/// Turns one triage pass into the status buffer's rows: the always-present
/// System section, then one `SectionHeader` + `Finding` rows per non-empty
/// category. A healthy machine has none of the latter - "sections with
/// zero rows are hidden entirely" - so this buffer is empty when there is
/// nothing to say, which is the whole premise.
///
/// The two journal sections are bounded, and the rest are not: a host
/// has however many filesystems it has, while a journal can produce
/// distinct error shapes without limit. What the heading counts is the
/// total either way - a count of the rows that survived a cap would be a
/// number nobody measured.
fn layout(findings: &[Finding], overview: Overview, rows_per_section: u32) -> Vec<Node> {
    // System first: it names the machine, and what a host *is* frames
    // every finding below it - a full `/boot` reads differently on a
    // NixOS box than on a Debian one. It also stays the "checks actually
    // ran" marker the design asks for, which works at least as well at
    // the top as at the bottom.
    // One row per line the overview has to draw, rather than one row
    // holding all of them. `Overview::parts` is what decides how many
    // there are; this only walks them, so nothing here has an opinion
    // about whether a host reported PSI.
    let mut rows = vec![Node::SectionHeader {
        title: "System".to_string(),
        kind: SectionKind::System,
        count: None,
    }];
    rows.extend(overview.parts().into_iter().map(|part| Node::OverviewLine {
        overview: overview.clone(),
        part,
    }));
    rows.push(Node::Spacer);

    for section in SECTIONS {
        let matched: Vec<&Finding> = findings
            .iter()
            .filter(|f| Presentation::of(&f.kind).section() == section.kind)
            .collect();
        if matched.is_empty() {
            continue;
        }
        let shown = match section.capped {
            true => rows_per_section as usize,
            false => matched.len(),
        };
        rows.push(Node::SectionHeader {
            title: section.title.to_string(),
            kind: section.kind,
            // What there is, not what fits.
            count: Some(matched.len() as u32),
        });
        rows.extend(matched.into_iter().take(shown).cloned().map(Node::finding));
        rows.push(Node::Spacer);
    }

    // Whatever came last left a trailing Spacer; the buffer should not
    // end on a blank row.
    if matches!(rows.last(), Some(Node::Spacer)) {
        rows.pop();
    }
    rows
}

/// The unit's own last words, rather than systemd's report that it died.
///
/// journald tags a unit's own output with that unit, while systemd's
/// messages *about* it come from pid 1 and are tagged `init.scope`. So
/// "Main process exited, code=exited" and "Failed with result
/// 'exit-code'" are systemd talking, and the line worth showing is the
/// last one the unit itself printed - here, `curl: (6) Could not resolve
/// host`.
///
/// Falls back to the most recent error-priority line when the unit said
/// nothing of its own, which is the case for one that failed before it
/// could exec.
fn last_words(entries: &[Entry], unit: &str) -> Option<String> {
    entries
        .iter()
        .rev()
        .find(|e| e.unit.as_deref() == Some(unit))
        .map(|e| e.message.clone())
        .or_else(|| {
            entries
                .iter()
                .rev()
                .find(|e| {
                    matches!(
                        e.priority,
                        Priority::Emergency
                            | Priority::Alert
                            | Priority::Critical
                            | Priority::Error
                    )
                })
                .map(|e| e.message.clone())
        })
        .map(|message| message.trim().to_string())
        .filter(|message| !message.is_empty())
}
