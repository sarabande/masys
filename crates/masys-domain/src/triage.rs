use std::collections::BTreeMap;

use crate::finding::{Finding, FindingKind, PressureResource, Severity, Thresholds};
use crate::journal::{Entry, Origin, Priority};
use crate::platform::BootPressure;
use crate::sample::{Filesystem, OomKill, Pressure, Snapshot, SystemState};
use crate::unit::{ActiveState, Unit};

pub fn failed_units(units: &[Unit], now_ms: u64) -> Vec<Finding> {
    units
        .iter()
        .filter(|u| u.active_state == ActiveState::Failed)
        .map(|u| {
            Finding::new(FindingKind::FailedUnit {
                unit: u.name.clone(),
                exit_code: u.exit_code,
                since_ms: now_ms.saturating_sub(u.since_ms),
                // Filled by the session, which has journal access.
                reason: None,
            })
        })
        .collect()
}

pub fn flapping_units(units: &[Unit], thresholds: &Thresholds, now_ms: u64) -> Vec<Finding> {
    let window_start = now_ms.saturating_sub(thresholds.flapping_window_ms);
    units
        .iter()
        .filter_map(|u| {
            let restarts = u
                .restart_timestamps_ms
                .iter()
                .filter(|&&t| t >= window_start && t <= now_ms)
                .count() as u32;
            (restarts >= thresholds.flapping_restart_count).then(|| {
                Finding::new(FindingKind::FlappingUnit {
                    unit: u.name.clone(),
                    restarts,
                    window_ms: thresholds.flapping_window_ms,
                })
            })
        })
        .collect()
}

/// The two PSI figures for one resource: what fraction of the last
/// minute some tasks stalled on it, and - where the kernel reports one -
/// what fraction every task did.
///
/// The only place that knows which lines belong to which resource. It
/// was two places for a day: `pressure` spelled the mapping out in a
/// table and the status buffer spelled it out again to label the figure
/// it drew beside a colour, which is two owners of one question and the
/// arrangement `crate::triage`'s callers exist to avoid. A segment
/// coloured from one resource and captioned with another's figure is a
/// defect nothing but a reader would ever catch.
///
/// `full` is `None` for CPU because the kernel reports no such line at
/// all - a runnable task is never *fully* stalled on CPU - and not
/// because it happens to be zero.
pub fn psi_lines(pressure: &Pressure, resource: PressureResource) -> (f32, Option<f32>) {
    match resource {
        PressureResource::Cpu => (pressure.cpu_some.avg60, None),
        PressureResource::Io => (pressure.io_some.avg60, Some(pressure.io_full.avg60)),
        PressureResource::Memory => (pressure.memory_some.avg60, Some(pressure.memory_full.avg60)),
    }
}

/// Every resource PSI is read for, in the order the status buffer shows
/// them: worst class of problem first.
pub const PRESSURE_RESOURCES: [PressureResource; 3] = [
    PressureResource::Memory,
    PressureResource::Io,
    PressureResource::Cpu,
];

/// How bad one resource's pressure is, from the same two thresholds that
/// decide whether it produces a finding.
///
/// The single rule behind both. `pressure` below is written in terms of
/// it rather than restating the comparisons, so a threshold moved in
/// config moves the finding and the overview's colour together - two
/// copies of this rule would agree until the first time somebody edited
/// one of them.
///
/// `full` is worse than `some` because they are different claims: `some`
/// is the fraction of wall-clock time *at least one* task spent stalled,
/// `full` the fraction during which *nothing* could run. The second is a
/// machine that has stopped, not one that is working hard.
///
/// `full_avg60` is `None` for CPU, where the kernel reports no full line
/// at all - a runnable task is never fully stalled on CPU. Absent, not
/// zero: a zero would be a reading that happens to clear the urgent
/// threshold, and this function must not invent one.
///
/// `Unknown` for a figure that is not a number. A comparison against NaN
/// is false in both directions, so an unusable reading would otherwise
/// clear every threshold and be reported as `Normal` - a verdict of
/// "fine" drawn from a value that means nothing, which is the shape this
/// codebase keeps producing. Otherwise never `Unknown`: having figures to
/// compare is what it means to have been measured, and the caller with no
/// `Pressure` at all is the one that answers `Unknown`, because only it
/// knows.
pub fn pressure_severity(
    some_avg60: f32,
    full_avg60: Option<f32>,
    thresholds: &Thresholds,
) -> Severity {
    if !some_avg60.is_finite() || full_avg60.is_some_and(|full| !full.is_finite()) {
        return Severity::Unknown;
    }
    match full_avg60 {
        Some(full) if full >= thresholds.psi_full_avg60_percent => Severity::Urgent,
        _ if some_avg60 >= thresholds.psi_some_avg60_percent => Severity::Warning,
        _ => Severity::Normal,
    }
}

pub fn pressure(p: &Pressure, thresholds: &Thresholds) -> Vec<Finding> {
    PRESSURE_RESOURCES
        .into_iter()
        .filter_map(|resource| {
            let (some_avg60, full_avg60) = psi_lines(p, resource);
            let severity = pressure_severity(some_avg60, full_avg60, thresholds);
            // A finding for what masys can act on, which is a threshold
            // crossed - never for `Unknown`, which is the admission that
            // there was nothing to compare.
            //
            // The verdict is then carried rather than discarded, which is
            // the whole difference between a row that says how bad the
            // stall is and one that says only that there is a stall.
            matches!(severity, Severity::Warning | Severity::Urgent).then(|| {
                Finding::new(FindingKind::Pressure {
                    resource,
                    some_avg60,
                    full_avg60,
                    severity,
                })
            })
        })
        .collect()
}

pub fn disk_capacity(
    filesystems: &[Filesystem],
    boot_pressure: Option<&BootPressure>,
    thresholds: &Thresholds,
) -> Vec<Finding> {
    filesystems
        .iter()
        .filter(|fs| fs.used_percent >= thresholds.disk_used_percent)
        .map(|fs| {
            Finding::new(FindingKind::DiskCapacity {
                mount_point: fs.mount_point.clone(),
                used_percent: fs.used_percent,
                free_bytes: fs.free_bytes,
                // `and_then` rather than `map`, now that the port's own
                // `generations` is an `Option`. Two different `None`s
                // deliberately collapse into one here: no boot pressure was
                // read at all, and this host's `/boot` is not
                // generation-based. The renderer appends `. {n} generations`
                // only when the value is present, so both correctly mean
                // "say nothing about generations" - and neither may become a
                // `0`, which would read as a count.
                generations: if fs.mount_point == "/boot" {
                    boot_pressure.and_then(|b| b.generations)
                } else {
                    None
                },
                // Boot pressure is a fact about `/boot`, so it attaches
                // only there - a `/home` over threshold reporting what a
                // kernel cleanup would free would be answering a question
                // nobody asked about the wrong filesystem.
                reclaimable_bytes: match fs.mount_point == "/boot" {
                    true => boot_pressure.and_then(|b| b.reclaimable_bytes),
                    false => None,
                },
            })
        })
        .collect()
}

pub fn inode_exhaustion(filesystems: &[Filesystem], thresholds: &Thresholds) -> Vec<Finding> {
    filesystems
        .iter()
        .filter(|fs| fs.inode_used_percent >= thresholds.inode_used_percent)
        .map(|fs| {
            Finding::new(FindingKind::InodeExhaustion {
                mount_point: fs.mount_point.clone(),
                inode_used_percent: fs.inode_used_percent,
            })
        })
        .collect()
}

pub fn read_only_filesystems(filesystems: &[Filesystem]) -> Vec<Finding> {
    filesystems
        .iter()
        .filter(|fs| fs.read_only)
        .map(|fs| {
            Finding::new(FindingKind::ReadOnlyFilesystem {
                mount_point: fs.mount_point.clone(),
            })
        })
        .collect()
}

/// The instant the Status buffer's journal sections start from.
///
/// One owner, because two would drift: the session asks the port for
/// everything since this, and the rule below filters on it again. A read
/// over one window filtered by another is a section that quietly reports
/// on a different span than the one it names.
pub fn journal_since(thresholds: &Thresholds, now_ms: u64) -> u64 {
    now_ms.saturating_sub(thresholds.journal_window_ms)
}

/// The priority floor both journal sections read at.
///
/// Fixed rather than configurable. Below `Error` the journal stops being
/// a list of things that went wrong: warnings are constant on a working
/// host, and a section full of them is one an operator learns to skip -
/// which costs more than it could ever report. The window and the row
/// count are the knobs; what counts as a problem is not.
pub const JOURNAL_FLOOR: Priority = Priority::Error;

/// What a message looks like with the parts that vary per occurrence
/// taken out, so that repeats of one event can be counted as one.
///
/// The whole risk of deduplication lives here. Mask too little and a
/// retry storm is forty rows; mask too much and two different failures
/// become one row whose message speaks for both - a claim masys did not
/// measure, and the worse of the two mistakes.
///
/// Two rules, in this order:
///
/// * a run of four or more hex characters containing a digit becomes
///   `N`. These are identifiers - `85dffffb`, a build hash, one group of
///   a uuid - and they are meaningless to compare. A digit is required,
///   so `deadbeef` and `cafebabe` stay the words they are.
/// * every remaining run of digits becomes `N`, which covers pids,
///   ports, addresses and line numbers.
///
/// Hex first, because digits are a subset of hex: masking digits first
/// would cut `85dffffb` into `Ndffffb` and leave two occurrences of one
/// identifier looking different.
///
/// **Four, not eight.** Eight was the first guess and it did not do the
/// job it was written for: a uuid is dash-separated groups of 8-4-4-4-12,
/// so only two of its five groups reached the threshold and two lines
/// bearing different uuids stayed two rows - which is the retry storm
/// this exists to collapse, logged the way services actually log it.
/// Four still leaves device names alone, because `sda1` is one hex
/// character short of qualifying and `sdb1` with it.
///
/// What survives is the wording, which is what distinguishes one failure
/// from another. `sda1` becomes `sdaN` rather than vanishing, so two
/// devices stay two findings.
pub fn message_shape(message: &str) -> String {
    let hexed = mask_runs(message, 4, |c| c.is_ascii_hexdigit());
    mask_runs(&hexed, 1, |c| c.is_ascii_digit())
}

/// Replaces every maximal run of at least `min_len` characters matching
/// `class` **and containing a digit** with `N`.
///
/// The digit requirement is what keeps this from eating words. It is
/// trivially satisfied by the digit pass, whose class is digits.
///
/// Characters rather than bytes. Both classes are ASCII today, so the
/// two agree - but a length measured in bytes and documented in
/// characters is a trap left for whoever adds a third class.
fn mask_runs(text: &str, min_len: usize, class: fn(char) -> bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.chars().count() >= min_len && run.chars().any(|c| c.is_ascii_digit()) {
            out.push('N');
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if class(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// What makes two lines one finding: where it came from, who sent it -
/// a unit, or nobody in particular - and the shape of what it said.
type GroupKey = (Origin, Option<String>, String);

/// What a group of identical lines amounts to.
struct Occurrences {
    count: u32,
    /// When the most recent one arrived, which is what the section is
    /// ordered by and what the row's age is measured from.
    last_ms: u64,
    /// That most recent one, verbatim.
    message: String,
}

/// The Status buffer's two journal sections, from one read of the
/// journal.
///
/// Pure, like every rule here: the session performs the read and hands
/// the entries in, the same way `boot_pressure` arrives. That is what
/// lets the whole of this - window, floor, masking, counting, ordering
/// and the cap - be tested without a journal.
///
/// Kernel-origin lines and userspace lines are separated by who sent
/// them and are otherwise treated identically, which is why one query
/// serves both sections.
pub fn journal_findings(entries: &[Entry], thresholds: &Thresholds, now_ms: u64) -> Vec<Finding> {
    let since = journal_since(thresholds, now_ms);
    // Grouped by sender and by shape. `BTreeMap` rather than a hash, so
    // that two runs over one journal produce the same order - the rows
    // are re-derived every tick and a set that reshuffled itself under
    // the cursor would be its own defect.
    let mut groups: BTreeMap<GroupKey, Occurrences> = BTreeMap::new();
    for entry in entries
        .iter()
        .filter(|e| e.timestamp_ms >= since)
        .filter(|e| e.priority.at_or_worse_than(JOURNAL_FLOOR))
    {
        let key = (
            entry.origin,
            entry.unit.clone(),
            message_shape(&entry.message),
        );
        let group = groups.entry(key).or_insert(Occurrences {
            count: 0,
            last_ms: entry.timestamp_ms,
            message: entry.message.clone(),
        });
        group.count += 1;
        // The most recent occurrence represents the group, so the row
        // shows a line that was really logged rather than a masked
        // pattern nobody ever saw.
        if entry.timestamp_ms >= group.last_ms {
            group.last_ms = entry.timestamp_ms;
            group.message = entry.message.clone();
        }
    }

    let mut kernel = Vec::new();
    let mut userspace = Vec::new();
    for ((origin, unit, _), group) in groups {
        let Occurrences {
            count,
            last_ms,
            message,
        } = group;
        let age_ms = now_ms.saturating_sub(last_ms);
        // The Kernel section is a positive claim and takes only what
        // journald attributed to the kernel. Everything else lands in
        // `Recent errors`, which is the complement rather than a claim
        // of userspace origin - so a record nobody could attribute is
        // reported without inventing where it came from.
        match origin {
            Origin::Kernel => kernel.push((
                last_ms,
                Finding::new(FindingKind::KernelError {
                    message,
                    count,
                    age_ms,
                }),
            )),
            Origin::Userspace | Origin::Unknown => userspace.push((
                last_ms,
                Finding::new(FindingKind::RecentError {
                    unit,
                    message,
                    count,
                    age_ms,
                }),
            )),
        }
    }

    // Newest first: the row at the top is the thing that just happened.
    //
    // Every group, uncapped. How many rows fit on a screen is not a
    // question this module can answer, and capping here would have left
    // the section header counting survivors - `Kernel (5)` on a host
    // with forty distinct kernel errors, which is a count of what was
    // shown presented as a count of what there is.
    let mut findings = Vec::new();
    for mut section in [kernel, userspace] {
        section.sort_by_key(|(last_ms, _)| std::cmp::Reverse(*last_ms));
        findings.extend(section.into_iter().map(|(_, finding)| finding));
    }
    findings
}

pub fn clock_unsynchronized(clock_synced: bool) -> Vec<Finding> {
    if clock_synced {
        Vec::new()
    } else {
        vec![Finding::new(FindingKind::ClockUnsynchronized)]
    }
}

/// A finding where the last interval was spent throttled past the
/// threshold, and nothing otherwise.
///
/// `None` is a host whose kernel does not account for throttling, and it
/// produces no finding for the reason `SmartFailing` does not appear
/// without `smartctl`: an absent reading is a fact about masys, not
/// about the machine.
///
/// The `is_finite` guard is not decoration. Every comparison against a
/// NaN is false, so a NaN would fall through `>=` and be reported as
/// below the threshold - a machine silently declared cool on the
/// strength of a number that is not one. `pressure_severity` refuses on
/// the same grounds.
pub fn thermal_throttling(percent: Option<f32>, thresholds: &Thresholds) -> Vec<Finding> {
    match percent {
        Some(percent) if percent.is_finite() && percent >= thresholds.thermal_throttled_percent => {
            vec![Finding::new(FindingKind::ThermalThrottling { percent })]
        }
        _ => Vec::new(),
    }
}

pub fn oom_kills(kills: &[OomKill]) -> Vec<Finding> {
    kills
        .iter()
        .map(|k| {
            Finding::new(FindingKind::OomKill {
                pid: k.pid,
                comm: k.comm.clone(),
                timestamp_ms: k.timestamp_ms,
            })
        })
        .collect()
}

pub fn system_degraded(state: SystemState, units: &[Unit]) -> Vec<Finding> {
    if state != SystemState::Degraded {
        return Vec::new();
    }
    let failed_units = units
        .iter()
        .filter(|u| u.active_state == ActiveState::Failed)
        .count() as u32;
    vec![Finding::new(FindingKind::SystemDegraded { failed_units })]
}

/// One instant of the host, and what it is judged against.
///
/// A struct rather than seven positional arguments, on the argument
/// `masys_app::status::Tick` already makes: these all describe the same
/// instant, so they always travel together, and passed positionally
/// their order carried the whole meaning of the call. `evaluate` reached
/// seven parameters when the SMART reading arrived - one under the count
/// that makes clippy say so, which is not the same as being readable at
/// a call site.
///
/// **What may join them**, because a bundle whose rationale is "there
/// were too many" has no way to refuse the next field: something the
/// host presented at [`Self::now_ms`], or the criterion such a reading
/// is judged by. [`Self::thresholds`] is here on the second half of that
/// rule - a finding is a reading *and* a criterion, and neither is a
/// finding on its own. Anything about how triage *runs* - a limit, a
/// preference, a feature switch - is neither, and belongs in the
/// argument list where a reader will see it.
///
/// **What this does not buy**, since it is easy to assume otherwise: no
/// two of the seven parameters shared a type, so transposing a pair
/// never compiled and the struct did not make it stop. What it removes
/// is an order that could be wrong at all - the call site that read
/// `evaluate(&snapshot, &units, None, &[], None, &thresholds, 0)` had two
/// `None`s standing for two different absences, and swapping them was a
/// change no compiler and no reader could see.
///
/// Every reading is borrowed and nothing here is read from the machine:
/// this module performs no I/O, so whatever needs a port is fetched by
/// the session and handed over as a fact. That is also what lets the
/// session lend the journal window it is about to put back.
pub struct Tick<'a> {
    pub snapshot: &'a Snapshot,
    pub units: &'a [Unit],
    pub boot_pressure: Option<&'a BootPressure>,
    /// What the host logged in the window, already read.
    pub entries: &'a [Entry],
    /// Whether every disk that could be asked reports SMART as passing,
    /// or `None` where masys could not tell.
    pub smart: Option<bool>,
    /// What share of the last interval a core spent thermally throttled,
    /// already derived from two samples by `rate::derive_thermal`.
    ///
    /// Arrives computed rather than as the two snapshots, for the reason
    /// `smart` does: the rules here take readings and apply thresholds,
    /// and the caller is the layer that holds the previous sample.
    pub thermal_throttled_percent: Option<f32>,
    pub thresholds: &'a Thresholds,
    pub now_ms: u64,
}

/// Every v1 check, concatenated - what the status buffer calls once per
/// tick to build its whole finding list.
pub fn evaluate(tick: &Tick<'_>) -> Vec<Finding> {
    // Destructured once rather than read through `tick.` throughout: the
    // rules below are the same rules, and each still takes exactly what
    // it needs. Every field is `Copy`, so this moves nothing out of the
    // borrow.
    let Tick {
        snapshot,
        units,
        boot_pressure,
        entries,
        smart,
        thermal_throttled_percent,
        thresholds,
        now_ms,
    } = *tick;
    let mut findings = Vec::new();
    // Only an answered *failing* read is a finding. `None` is masys not
    // knowing, which is a fact about masys and not about the disks, and
    // a row for it would appear on every host without smartctl forever.
    if smart == Some(false) {
        findings.push(Finding::new(FindingKind::SmartFailing));
    }
    findings.extend(failed_units(units, now_ms));
    findings.extend(flapping_units(units, thresholds, now_ms));
    // No PSI means no pressure findings - not findings that say zero.
    if let Some(psi) = &snapshot.pressure {
        findings.extend(pressure(psi, thresholds));
    }
    findings.extend(disk_capacity(
        &snapshot.filesystems,
        boot_pressure,
        thresholds,
    ));
    findings.extend(inode_exhaustion(&snapshot.filesystems, thresholds));
    findings.extend(read_only_filesystems(&snapshot.filesystems));
    findings.extend(clock_unsynchronized(snapshot.clock_synced));
    findings.extend(thermal_throttling(thermal_throttled_percent, thresholds));
    findings.extend(oom_kills(&snapshot.oom_kills));
    findings.extend(system_degraded(snapshot.system_state, units));
    findings.extend(journal_findings(entries, thresholds, now_ms));
    findings
}
