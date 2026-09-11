//! Everything about a finding that is not the finding.
//!
//! Which section it belongs under, what its label column says, which
//! glyph heads it, what text follows, where `.` goes from it, and what
//! `/` matches on it. **Six answers, one `match`, one file.**
//!
//! They were five exhaustive matches in three crates - `section_of` and
//! `jump_target` and `finding_text` in masys-app, `finding_label` and
//! `finding_tail` in masys-render - and nothing related them. Adding a
//! kind meant five answers in four files, and the only check that all
//! five had been given was somebody reading four files and noticing an
//! absence. Twice in the fortnight before this was written a new kind
//! cost six touched files, four of them pure restatement.
//!
//! **This is not a table keyed by variant, and the difference is the
//! whole justification.** Elsewhere in this tree `NixOp` is matched
//! exhaustively in five places and stays that way deliberately: each
//! cascade asks a question whose wrong answer is invisible at runtime,
//! and a table of defaults would let a new operation inherit an answer
//! nobody chose. [`Presentation::of`] is a `match` returning a record,
//! so a fifteenth kind fails to compile until every field is filled in -
//! the compiler still forces the answer, it is just forced once instead
//! of five times.
//!
//! That holds only while **no field here has a default, and no field is
//! `Option` unless absent is a real answer about a finding** - which it
//! is for `label`, where the column exists and the row has nothing to
//! put in it, and for `jump`, where the finding names nothing masys has
//! a row for. A field that acquires a sensible fallback has turned this
//! into the arrangement the paragraph above rejects.

use masys_domain::finding::{FindingKind, PressureResource};

use crate::ProcSort;
use crate::buffer::Buffer;
use crate::format::{human_bytes, human_duration, pressure_word};
use crate::jump::{Jump, RowTarget};
use crate::node::SectionKind;

/// One finding's rendering, computed once.
///
/// Fields are private and [`Presentation::of`] is the only constructor,
/// so a `Node::Finding` cannot be built carrying a presentation that
/// disagrees with its finding. That is the whole guarantee: the two
/// travel together and only one thing ever produces the second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presentation {
    section: SectionKind,
    label: Option<String>,
    glyph: &'static str,
    tail: String,
    keywords: &'static [&'static str],
    jump: Option<Jump>,
}

impl Presentation {
    /// Which section this finding belongs under.
    ///
    /// A finding belongs to exactly one, which is why this is a
    /// `SectionKind` rather than a set. Measured before it was written:
    /// all fourteen kinds were claimed by exactly one section already, so
    /// the relation was a function before it was spelled as one.
    pub fn section(&self) -> SectionKind {
        self.section
    }

    /// The variable-width leading identifier the row is padded on.
    ///
    /// `None` where the row has no label column at all, which skips the
    /// padding entirely. `Some("")` is a different claim and a real one:
    /// the column exists and this row has nothing to put in it. A
    /// userspace error logged outside every unit is that case, and
    /// collapsing it to `None` would start its message a whole column
    /// left of its neighbours'.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// The severity glyph.
    ///
    /// **Not derivable from `Severity`, which is why it is stored.**
    /// `Warning` draws `~` for a flapping unit and an unreadable source
    /// and `^` for pressure, capacity and thermal - a distinction the
    /// screen makes and the model has no word for. Naming it was tried
    /// and abandoned: `ReadOnlyFilesystem` draws `^` and is not a thing
    /// approaching a limit, so the grouping is visual rather than
    /// conceptual, and inventing a domain term to justify a glyph is
    /// backwards.
    pub fn glyph(&self) -> &'static str {
        self.glyph
    }

    /// The text following the label, carrying its own leading separator
    /// so that what gets padded is exactly what [`Self::label`] reports.
    pub fn tail(&self) -> &str {
        &self.tail
    }

    /// Where `.` goes from this row, or `None` where it is about nothing
    /// that has one.
    ///
    /// **Absent is a real answer here and the important one.** An
    /// OOM-killed process has no row because it is dead. An
    /// unsynchronised clock is represented in no buffer masys has. A
    /// degraded system is a rollup of failed units already listed above
    /// it, each with a live jump of its own. `SmartFailing` names no disk
    /// because the port answers once for the host, and
    /// `ThermalThrottling` names no core for the same shape of reason -
    /// it becomes a jump the day the port answers per device, and not
    /// before. A wildcard would hand a new kind "nowhere", which is
    /// indistinguishable from a decision and is the one nobody notices
    /// being wrong.
    pub fn jump(&self) -> Option<&Jump> {
        self.jump.as_ref()
    }

    /// What `/` matches on this row.
    ///
    /// The label and the tail - what is on screen - plus whatever words
    /// the kind declares that are not. Derived rather than written out a
    /// second time: `finding_text` used to be a fifth match whose doc
    /// claimed its strings mirrored the renderer, and they did not. The
    /// mirror is now structural, and the extra words are named as what
    /// they are.
    pub fn searchable(&self) -> String {
        let mut text = String::new();
        if let Some(label) = &self.label {
            text.push_str(label);
            text.push(' ');
        }
        text.push_str(&self.tail);
        for keyword in self.keywords {
            text.push(' ');
            text.push_str(keyword);
        }
        text
    }

    /// The one statement of what a finding says about itself.
    ///
    /// Exhaustive with no wildcard. Each arm answers all six questions in
    /// one place, so what a `DiskCapacity` finding *is* - a Disk-section
    /// row labelled by its mount point, drawn `^`, saying how full it is,
    /// jumping to that filesystem in the IO buffer - reads as one thing
    /// rather than as six entries in four files.
    pub fn of(kind: &FindingKind) -> Presentation {
        match kind {
            FindingKind::FailedUnit {
                unit,
                exit_code,
                since_ms,
                reason,
            } => {
                let state = match exit_code {
                    Some(code) => format!("failed (exit {code})"),
                    None => "failed".to_string(),
                };
                // The reason last and unpadded: it is free text of
                // unknown length, so it cannot be a column, and
                // everything an operator scans for - which unit, how, how
                // long ago - stays in a fixed place to its left.
                let why = match reason {
                    Some(reason) => format!("  {reason}"),
                    None => String::new(),
                };
                Presentation {
                    section: SectionKind::FailedUnits,
                    label: Some(unit.clone()),
                    glyph: "x",
                    tail: format!("  {state}  {} ago{why}", human_duration(*since_ms)),
                    keywords: &[],
                    jump: Some(Jump {
                        buffer: Buffer::Systemd,
                        row: RowTarget::Unit(unit.clone()),
                    }),
                }
            }
            FindingKind::FlappingUnit {
                unit,
                restarts,
                window_ms,
            } => Presentation {
                section: SectionKind::Flapping,
                label: Some(unit.clone()),
                glyph: "~",
                tail: format!("  {restarts} restarts in {}", human_duration(*window_ms)),
                keywords: &[],
                jump: Some(Jump {
                    buffer: Buffer::Systemd,
                    row: RowTarget::Unit(unit.clone()),
                }),
            },
            FindingKind::Pressure {
                resource,
                some_avg60,
                full_avg60,
                ..
            } => {
                let mut tail = format!("  some avg60  {some_avg60:.1}%");
                // A zero full-stall is not news once some-stall has
                // already crossed its threshold, so it earns no column.
                // The guard tests what the value would *render* as rather
                // than the raw float - anything under 0.05 formats as
                // "0.0%", the very string this suppresses. `None`
                // deliberately takes the same path: this tail is a
                // variable inline list, not a fixed column, so there is
                // nowhere to hang a `-` that would tell not-yet-measured
                // apart from measured-zero.
                if let Some(full) = full_avg60
                    && *full >= 0.05
                {
                    tail.push_str(&format!("      full avg60  {full:.1}%"));
                }
                Presentation {
                    section: SectionKind::Pressure,
                    label: Some(pressure_word(*resource).to_string()),
                    glyph: "^",
                    tail,
                    // The only declared keyword in the tree, and it earns
                    // it: "pressure" appears nowhere on the row and is
                    // the first thing an operator types. Every other kind
                    // is found by words it already shows.
                    keywords: &["pressure"],
                    // Pressure names no row of its own - it is a property
                    // of the machine - so the useful destination is
                    // whatever is currently causing it: Procs, in that
                    // order, cursor on the top of it. A pid resolved here
                    // would be a reading dressed as an identity, stale by
                    // the time the cursor moved.
                    jump: Some(Jump {
                        buffer: Buffer::Procs,
                        row: RowTarget::TopOfProcs(match resource {
                            PressureResource::Cpu => ProcSort::Cpu,
                            PressureResource::Io => ProcSort::Io,
                            PressureResource::Memory => ProcSort::Memory,
                        }),
                    }),
                }
            }
            FindingKind::DiskCapacity {
                mount_point,
                used_percent,
                free_bytes,
                generations,
                reclaimable_bytes,
            } => {
                let mut tail = format!("  {used_percent:.0}%   {} free", human_bytes(*free_bytes));
                // Both clauses append only when present, which is what
                // lets a host with no generations say nothing rather than
                // say zero. A Debian `/boot` carries the second and not
                // the first: it has kernels to reclaim and no generations
                // to count.
                if let Some(g) = generations {
                    tail.push_str(&format!("   .  {g} generations"));
                }
                if let Some(bytes) = reclaimable_bytes.filter(|bytes| *bytes > 0) {
                    tail.push_str(&format!("   .  {} reclaimable", human_bytes(bytes)));
                }
                Presentation {
                    section: SectionKind::Disk,
                    label: Some(mount_point.clone()),
                    glyph: "^",
                    tail,
                    keywords: &[],
                    jump: Some(Jump {
                        buffer: Buffer::Io,
                        row: RowTarget::Filesystem(mount_point.clone()),
                    }),
                }
            }
            FindingKind::InodeExhaustion {
                mount_point,
                inode_used_percent,
            } => Presentation {
                section: SectionKind::Disk,
                label: Some(mount_point.clone()),
                glyph: "^",
                tail: format!("  {inode_used_percent:.0}% inodes used"),
                keywords: &[],
                jump: Some(Jump {
                    buffer: Buffer::Io,
                    row: RowTarget::Filesystem(mount_point.clone()),
                }),
            },
            // A read-only root is the case worth having: the finding says
            // the filesystem flipped, and the IO buffer is where its
            // usage, inode count and mount flags are - which is what
            // tells you whether it flipped because it filled up or for
            // some other reason.
            FindingKind::ReadOnlyFilesystem { mount_point } => Presentation {
                section: SectionKind::Disk,
                label: Some(mount_point.clone()),
                glyph: "^",
                tail: "  remounted read-only".to_string(),
                keywords: &[],
                jump: Some(Jump {
                    buffer: Buffer::Io,
                    row: RowTarget::Filesystem(mount_point.clone()),
                }),
            },
            FindingKind::ClockUnsynchronized => Presentation {
                section: SectionKind::Clock,
                label: None,
                glyph: "!",
                tail: "clock not synchronized".to_string(),
                keywords: &[],
                jump: None,
            },
            // Under Disk rather than a section of its own: "what is wrong
            // with storage" is one question, and the device belongs in it
            // beside the mounts. The only finding about hardware rather
            // than about what the host is running - a disk saying it
            // expects to fail is the one report where the useful response
            // is to stop reading this tool and go and copy something.
            FindingKind::SmartFailing => Presentation {
                section: SectionKind::Disk,
                label: None,
                glyph: "!",
                tail: "disk smart self-assessment failing".to_string(),
                keywords: &[],
                jump: None,
            },
            // Under Pressure, and drawn `^`: a stalled task and a
            // held-back clock are two answers to "why is this machine
            // slow", and an operator asks that once. A throttled machine
            // is working, just more slowly, which is the same claim
            // "under pressure" makes. The share is the whole reading - no
            // temperature, because the governor has already judged what
            // is too hot for this silicon.
            FindingKind::ThermalThrottling { percent } => Presentation {
                section: SectionKind::Pressure,
                label: None,
                glyph: "^",
                tail: format!("cpu thermal throttling  {percent:.1}% of interval"),
                keywords: &[],
                jump: None,
            },
            // A warning rather than urgent, and the label says which
            // reading failed - which is what lets one section hold three
            // sources without a row becoming ambiguous.
            FindingKind::Unreadable {
                source,
                reason,
                since_ms,
            } => Presentation {
                section: SectionKind::Unreadable,
                label: Some(source.label().to_string()),
                glyph: "~",
                tail: format!("  unreadable  {reason}  {}", human_duration(*since_ms)),
                keywords: &[],
                jump: None,
            },
            // The kernel section's rows have no label column: every line
            // in it came from the same sender, which the heading names.
            FindingKind::KernelError {
                message,
                count,
                age_ms,
            } => Presentation {
                section: SectionKind::Kernel,
                label: None,
                glyph: "!",
                tail: format!(
                    "{message}{}  {}",
                    repeat_count(*count),
                    human_duration(*age_ms)
                ),
                keywords: &[],
                jump: None,
            },
            FindingKind::RecentError {
                unit,
                message,
                count,
                age_ms,
            } => Presentation {
                section: SectionKind::RecentErrors,
                // Aligns on whoever logged it, which is the column an
                // operator scans. `Some("")` for a sender outside any
                // unit - see [`Self::label`].
                label: Some(unit.clone().unwrap_or_default()),
                glyph: "!",
                tail: format!(
                    "  {message}{}  {}",
                    repeat_count(*count),
                    human_duration(*age_ms)
                ),
                keywords: &[],
                // The message on the row is truncated to fit, so "show me
                // the rest" is the obvious next question and the log is
                // where the answer is. A record from no unit names
                // nothing masys has a row for.
                jump: unit.as_ref().map(|unit| Jump {
                    buffer: Buffer::Log,
                    row: RowTarget::UnitLog(unit.clone()),
                }),
            },
            FindingKind::OomKill { pid, comm, .. } => Presentation {
                section: SectionKind::OomKills,
                label: Some(comm.clone()),
                glyph: "!",
                tail: format!("  killed by OOM (pid {pid})"),
                keywords: &[],
                jump: None,
            },
            FindingKind::SystemDegraded { failed_units } => {
                let plural = if *failed_units == 1 { "unit" } else { "units" };
                Presentation {
                    section: SectionKind::Degraded,
                    label: None,
                    glyph: "!",
                    tail: format!("system degraded ({failed_units} failed {plural})"),
                    keywords: &[],
                    jump: None,
                }
            }
        }
    }
}

/// ` x12` for a line seen twelve times, and nothing at all for one seen
/// once.
///
/// Silent at one because that is the common case: a count on every row
/// would be noise on most of them, and the rows worth noticing are the
/// ones that repeated.
fn repeat_count(count: u32) -> String {
    match count > 1 {
        true => format!("  x{count}"),
        false => String::new(),
    }
}
