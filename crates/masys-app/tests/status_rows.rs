use masys_app::status::build_status_rows;
use masys_domain::finding::Finding;
use masys_domain::sample::SystemState;
use masys_view::{Node, Overview, SectionKind};

fn overview() -> Overview {
    Overview {
        machine: None,
        system_state: SystemState::Running,
        unit_count: 47,
        load_1: None,
        load_5: None,
        load_15: None,
        uptime_secs: None,
        mem_used_bytes: None,
        mem_total_bytes: None,
        zram_percent: None,
        swap_free_bytes: None,
        clock_synced: true,
        smart_ok: None,
        pending_reboot: None,
    }
}

#[test]
fn a_healthy_system_shows_only_the_system_section() {
    let rows = build_status_rows(&[], overview());
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert!(matches!(&rows[0], Node::SectionHeader { kind: SectionKind::System, .. }));
    assert!(matches!(&rows[1], Node::Overview(_)));
}

#[test]
fn a_failed_unit_gets_its_own_section() {
    let findings =
        vec![Finding::FailedUnit { unit: "restic-backup.service".to_string(), exit_code: Some(1), since_ms: 10_800_000, reason: None }];
    let rows = build_status_rows(&findings, overview());
    // System, its overview, a spacer, then the finding section.
    assert_eq!(rows.len(), 5, "{rows:#?}");
    assert!(matches!(&rows[0], Node::SectionHeader { kind: SectionKind::System, .. }));
    assert!(matches!(&rows[3], Node::SectionHeader { kind: SectionKind::FailedUnits, count: Some(1), .. }));
    assert!(matches!(&rows[4], Node::Finding(Finding::FailedUnit { .. })));
}

#[test]
fn disk_findings_of_every_kind_share_one_section() {
    let findings = vec![
        Finding::DiskCapacity { mount_point: "/nix".to_string(), used_percent: 91.0, free_bytes: 1, generations: None },
        Finding::InodeExhaustion { mount_point: "/var".to_string(), inode_used_percent: 95.0 },
        Finding::ReadOnlyFilesystem { mount_point: "/".to_string() },
    ];
    let rows = build_status_rows(&findings, overview());
    let header_count = rows.iter().filter(|r| matches!(r, Node::SectionHeader { kind: SectionKind::Disk, .. })).count();
    assert_eq!(header_count, 1, "all three disk-shaped findings share one section: {rows:#?}");
    let finding_count = rows.iter().filter(|r| matches!(r, Node::Finding(_))).count();
    assert_eq!(finding_count, 3);
}

#[test]
fn sections_appear_in_a_fixed_order() {
    let findings = vec![
        Finding::SystemDegraded { failed_units: 1 },
        Finding::FailedUnit { unit: "a.service".to_string(), exit_code: None, since_ms: 0, reason: None },
    ];
    let rows = build_status_rows(&findings, overview());
    let kinds: Vec<SectionKind> = rows
        .iter()
        .filter_map(|r| match r {
            Node::SectionHeader { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    // System leads: what the host *is* frames every finding under it.
    assert_eq!(kinds, vec![SectionKind::System, SectionKind::FailedUnits, SectionKind::Degraded], "{kinds:?}");
}
