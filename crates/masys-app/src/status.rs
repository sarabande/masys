//! Builds the status buffer's rows from a triage pass - the masys-specific
//! analogue of wagit-app's `outline::build`.

use masys_domain::finding::Finding;
use masys_view::{Node, Overview, SectionKind};

/// One finding category: its section title, its `SectionKind`, and which
/// findings belong under it.
struct Section {
    title: &'static str,
    kind: SectionKind,
    matches: fn(&Finding) -> bool,
}

const SECTIONS: &[Section] = &[
    Section { title: "Failed units", kind: SectionKind::FailedUnits, matches: |f| matches!(f, Finding::FailedUnit { .. }) },
    Section { title: "Flapping", kind: SectionKind::Flapping, matches: |f| matches!(f, Finding::FlappingUnit { .. }) },
    Section { title: "Pressure", kind: SectionKind::Pressure, matches: |f| matches!(f, Finding::Pressure { .. }) },
    Section {
        title: "Disk",
        kind: SectionKind::Disk,
        matches: |f| matches!(f, Finding::DiskCapacity { .. } | Finding::InodeExhaustion { .. } | Finding::ReadOnlyFilesystem { .. }),
    },
    Section { title: "Clock", kind: SectionKind::Clock, matches: |f| matches!(f, Finding::ClockUnsynchronized) },
    Section { title: "OOM kills", kind: SectionKind::OomKills, matches: |f| matches!(f, Finding::OomKill { .. }) },
    Section { title: "Degraded", kind: SectionKind::Degraded, matches: |f| matches!(f, Finding::SystemDegraded { .. }) },
];

/// Turns one triage pass into the status buffer's rows: the always-present
/// System section, then one `SectionHeader` + `Finding` rows per non-empty
/// category. A healthy machine has none of the latter - "sections with
/// zero rows are hidden entirely" - so this buffer is empty when there is
/// nothing to say, which is the whole premise.
///
/// Kernel-origin and recent-error journal rows aren't built here yet:
/// `SystemService::journal` has no test double that returns real data in
/// this plan, so there is nothing to drive that section with. A later
/// plan adds it once the fake (and the real adapter) can answer.
pub fn build_status_rows(findings: &[Finding], overview: Overview) -> Vec<Node> {
    // System first: it names the machine, and what a host *is* frames
    // every finding below it - a full `/boot` reads differently on a
    // NixOS box than on a Debian one. It also stays the "checks actually
    // ran" marker the design asks for, which works at least as well at
    // the top as at the bottom.
    let mut rows = vec![
        Node::SectionHeader { title: "System".to_string(), kind: SectionKind::System, count: None },
        Node::Overview(overview),
        Node::Spacer,
    ];

    for section in SECTIONS {
        let matched: Vec<&Finding> = findings.iter().filter(|f| (section.matches)(f)).collect();
        if matched.is_empty() {
            continue;
        }
        rows.push(Node::SectionHeader { title: section.title.to_string(), kind: section.kind, count: Some(matched.len() as u32) });
        rows.extend(matched.into_iter().cloned().map(Node::Finding));
        rows.push(Node::Spacer);
    }

    // Whatever came last left a trailing Spacer; the buffer should not
    // end on a blank row.
    if matches!(rows.last(), Some(Node::Spacer)) {
        rows.pop();
    }
    rows
}
