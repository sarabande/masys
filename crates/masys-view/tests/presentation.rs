//! Everything a finding says about itself, pinned in one table.
//!
//! **The point of the table is that it is one table.** Six answers -
//! which section, what label, which glyph, what text, where `.` goes,
//! what `/` matches - used to be five functions in three crates, and the
//! only way to check that a new finding kind had been given all six was
//! to read four files and notice an absence. Here a kind that is added
//! and not described fails one test, loudly, in the crate that owns the
//! question.
//!
//! Expected values are the ones the tree already drew: they were lifted
//! from `masys-render`'s `every_finding_variant_maps_onto_its_severity_tier`,
//! from `masys-app`'s jump tests, and from the section assertions in
//! `status_rows`. Nothing here was computed by running the code it
//! checks.

use masys_domain::finding::{FindingKind, PressureResource, UnreadableSource};
use masys_view::ProcSort;
use masys_view::buffer::Buffer;
use masys_view::jump::{Jump, RowTarget};
use masys_view::node::SectionKind;
use masys_view::presentation::Presentation;

/// One kind, and everything it should answer.
struct Case {
    kind: FindingKind,
    section: SectionKind,
    label: Option<&'static str>,
    glyph: &'static str,
    tail: &'static str,
    jump: Option<Jump>,
    /// Words `/` must find on this row. Some are on screen and some are
    /// declared keywords; the test does not care which, because an
    /// operator does not either.
    searchable: &'static [&'static str],
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            kind: FindingKind::FailedUnit {
                unit: "a.service".to_string(),
                exit_code: Some(2),
                since_ms: 60_000,
                reason: None,
            },
            section: SectionKind::FailedUnits,
            label: Some("a.service"),
            glyph: "x",
            tail: "  failed (exit 2)  1m ago",
            jump: Some(Jump {
                buffer: Buffer::Systemd,
                row: RowTarget::Unit("a.service".to_string()),
            }),
            searchable: &["a.service", "failed"],
        },
        Case {
            kind: FindingKind::FlappingUnit {
                unit: "a.service".to_string(),
                restarts: 5,
                window_ms: 3_600_000,
            },
            section: SectionKind::Flapping,
            label: Some("a.service"),
            glyph: "~",
            tail: "  5 restarts in 1h",
            jump: Some(Jump {
                buffer: Buffer::Systemd,
                row: RowTarget::Unit("a.service".to_string()),
            }),
            searchable: &["a.service", "restarts"],
        },
        Case {
            kind: FindingKind::Pressure {
                resource: PressureResource::Cpu,
                some_avg60: 30.0,
                full_avg60: None,
                severity: masys_domain::finding::Severity::Warning,
            },
            section: SectionKind::Pressure,
            label: Some("cpu"),
            glyph: "^",
            tail: "  some avg60  30.0%",
            jump: Some(Jump {
                buffer: Buffer::Procs,
                row: RowTarget::TopOfProcs(ProcSort::Cpu),
            }),
            // "pressure" is nowhere on the row and is the obvious thing
            // to type. The one declared keyword in the whole table.
            searchable: &["cpu", "pressure"],
        },
        Case {
            kind: FindingKind::DiskCapacity {
                mount_point: "/".to_string(),
                used_percent: 90.0,
                free_bytes: 1u64 << 30,
                generations: None,
                reclaimable_bytes: None,
            },
            section: SectionKind::Disk,
            label: Some("/"),
            glyph: "^",
            tail: "  90%   1.0G free",
            jump: Some(Jump {
                buffer: Buffer::Io,
                row: RowTarget::Filesystem("/".to_string()),
            }),
            searchable: &["/"],
        },
        Case {
            kind: FindingKind::InodeExhaustion {
                mount_point: "/var".to_string(),
                inode_used_percent: 94.0,
            },
            section: SectionKind::Disk,
            label: Some("/var"),
            glyph: "^",
            tail: "  94% inodes used",
            jump: Some(Jump {
                buffer: Buffer::Io,
                row: RowTarget::Filesystem("/var".to_string()),
            }),
            searchable: &["/var", "inodes"],
        },
        Case {
            kind: FindingKind::ReadOnlyFilesystem {
                mount_point: "/home".to_string(),
            },
            section: SectionKind::Disk,
            label: Some("/home"),
            glyph: "^",
            tail: "  remounted read-only",
            jump: Some(Jump {
                buffer: Buffer::Io,
                row: RowTarget::Filesystem("/home".to_string()),
            }),
            searchable: &["/home", "read-only"],
        },
        Case {
            kind: FindingKind::ClockUnsynchronized,
            section: SectionKind::Clock,
            label: None,
            glyph: "!",
            tail: "clock not synchronized",
            jump: None,
            searchable: &["clock not synchronized"],
        },
        Case {
            kind: FindingKind::SmartFailing,
            section: SectionKind::Disk,
            label: None,
            glyph: "!",
            tail: "disk smart self-assessment failing",
            jump: None,
            searchable: &["smart"],
        },
        Case {
            kind: FindingKind::ThermalThrottling { percent: 12.5 },
            section: SectionKind::Pressure,
            label: None,
            glyph: "^",
            tail: "cpu thermal throttling  12.5% of interval",
            jump: None,
            searchable: &["thermal"],
        },
        Case {
            kind: FindingKind::Unreadable {
                source: UnreadableSource::Journal,
                reason: "journalctl: exited 1".to_string(),
                since_ms: 240_000,
            },
            section: SectionKind::Unreadable,
            label: Some("journal"),
            glyph: "~",
            tail: "  unreadable  journalctl: exited 1  4m",
            jump: None,
            searchable: &["journal", "unreadable"],
        },
        Case {
            kind: FindingKind::KernelError {
                message: "EXT4-fs error".to_string(),
                count: 1,
                age_ms: 60_000,
            },
            section: SectionKind::Kernel,
            label: None,
            glyph: "!",
            tail: "EXT4-fs error  1m",
            jump: None,
            searchable: &["ext4"],
        },
        Case {
            kind: FindingKind::RecentError {
                unit: Some("sshd.service".to_string()),
                message: "auth failure".to_string(),
                count: 3,
                age_ms: 60_000,
            },
            section: SectionKind::RecentErrors,
            label: Some("sshd.service"),
            glyph: "!",
            tail: "  auth failure  x3  1m",
            jump: Some(Jump {
                buffer: Buffer::Log,
                row: RowTarget::UnitLog("sshd.service".to_string()),
            }),
            searchable: &["sshd.service", "auth failure"],
        },
        Case {
            kind: FindingKind::OomKill {
                pid: 4213,
                comm: "firefox".to_string(),
                timestamp_ms: 1_000,
            },
            section: SectionKind::OomKills,
            label: Some("firefox"),
            glyph: "!",
            tail: "  killed by OOM (pid 4213)",
            jump: None,
            // Lower-case, because `/` lower-cases both sides and an
            // operator types `oom`. The row says `OOM`.
            searchable: &["firefox", "oom", "4213"],
        },
        Case {
            kind: FindingKind::SystemDegraded { failed_units: 3 },
            section: SectionKind::Degraded,
            label: None,
            glyph: "!",
            tail: "system degraded (3 failed units)",
            jump: None,
            searchable: &["degraded"],
        },
    ]
}

/// Every kind, and all six answers for each.
#[test]
fn a_finding_describes_itself_completely() {
    for case in cases() {
        let p = Presentation::of(&case.kind);
        let what = format!("{:?}", case.kind);
        assert_eq!(p.section(), case.section, "section of {what}");
        assert_eq!(p.label(), case.label, "label of {what}");
        assert_eq!(p.glyph(), case.glyph, "glyph of {what}");
        assert_eq!(p.tail(), case.tail, "tail of {what}");
        assert_eq!(p.jump(), case.jump.as_ref(), "jump of {what}");
    }
}

/// What `/` finds on each row.
///
/// Separate from the table above because it asserts a different kind of
/// thing: not "this field equals that" but "typing this reaches this
/// row". The searchable text is the label and the tail plus whatever
/// keywords a kind declares, and which half a given word comes from is
/// deliberately not asserted - it is not a distinction an operator can
/// see, and pinning it would make the test refuse a harmless rewording.
#[test]
fn every_word_an_operator_would_type_reaches_its_row() {
    for case in cases() {
        let text = Presentation::of(&case.kind).searchable().to_lowercase();
        for needle in case.searchable {
            assert!(
                text.contains(&needle.to_lowercase()),
                "`/{needle}` must reach {:?}, whose searchable text is {text:?}",
                case.kind
            );
        }
    }
}

/// A finding that names no row says so, rather than pointing somewhere
/// plausible.
///
/// The four permanent `None`s and the conditional one, asserted as a set
/// so that a kind quietly *gaining* a jump is as visible as one losing
/// it. `jump_target`'s doc argued this at length before it was folded in
/// here; the argument is now in `Presentation::of`.
#[test]
fn the_findings_that_name_no_row_do_not_jump() {
    let silent: Vec<String> = cases()
        .iter()
        .filter(|case| Presentation::of(&case.kind).jump().is_none())
        .map(|case| {
            format!("{:?}", case.kind)
                .split(' ')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        silent,
        vec![
            "ClockUnsynchronized",
            "SmartFailing",
            "ThermalThrottling",
            "Unreadable",
            "KernelError",
            "OomKill",
            "SystemDegraded",
        ],
        "exactly these name nothing masys has a row for"
    );
}

/// A userspace error from no unit has nowhere to go, where the same
/// error from a unit opens that unit's log.
///
/// The one kind whose jump depends on its own contents, so the table
/// above - one case per kind - cannot cover it.
#[test]
fn a_recent_error_from_no_unit_does_not_jump() {
    let orphan = FindingKind::RecentError {
        unit: None,
        message: "auth failure".to_string(),
        count: 1,
        age_ms: 0,
    };
    assert_eq!(Presentation::of(&orphan).jump(), None);
    // And its label is empty rather than absent: the column exists in a
    // section holding both kinds, and this row has nothing to put in it.
    assert_eq!(Presentation::of(&orphan).label(), Some(""));
}
