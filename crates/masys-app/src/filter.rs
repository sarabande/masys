//! Filtering rows by a typed string.
//!
//! Applied to whatever the open buffer built, rather than being written
//! per buffer, so `/` behaves the same everywhere. That matters more than
//! the per-buffer precision a bespoke filter would buy: htop, lnav, k9s
//! and less all use `/` and all mean "show me the lines with this in
//! them".

use masys_domain::finding::{Finding, PressureResource};
use masys_view::Node;

/// What a finding is *about*, in the words it is displayed under.
///
/// The status buffer's rows are findings, so without this `/` could never
/// match anything there and the key emptied the whole buffer for every
/// search an operator would actually type. The strings mirror what
/// `masys_render::view` prints for each variant - searching for what is
/// on the screen has to work, and the renderer is where "what is on the
/// screen" is decided.
fn finding_text(finding: &Finding) -> String {
    match finding {
        // The reason is free text from the unit's own log, and it is the
        // most specific thing on the row - `curl: (6) could not resolve
        // host` is worth being able to search for.
        Finding::FailedUnit { unit, reason, .. } => match reason {
            Some(reason) => format!("{unit} {reason}"),
            None => unit.clone(),
        },
        Finding::FlappingUnit { unit, .. } => unit.clone(),
        Finding::Pressure { resource, .. } => match resource {
            PressureResource::Cpu => "cpu pressure".to_string(),
            PressureResource::Io => "io pressure".to_string(),
            PressureResource::Memory => "memory pressure".to_string(),
        },
        Finding::DiskCapacity { mount_point, .. } | Finding::InodeExhaustion { mount_point, .. } => mount_point.clone(),
        Finding::ReadOnlyFilesystem { mount_point } => format!("{mount_point} read-only"),
        Finding::ClockUnsynchronized => "clock not synchronized".to_string(),
        Finding::OomKill { comm, pid, .. } => format!("{pid} {comm} oom"),
        Finding::SystemDegraded { .. } => "system degraded".to_string(),
    }
}

/// A row's searchable text, or `None` for a row that is structure rather
/// than content.
fn text_of(node: &Node) -> Option<String> {
    match node {
        Node::Proc { proc, .. } => Some(format!("{} {}", proc.pid, proc.comm)),
        Node::ProcGroup { name, .. } => Some(name.clone()),
        Node::Unit { unit, .. } => Some(format!("{} {}", unit.name, unit.sub_state)),
        Node::Timer { name, activates, .. } => Some(format!("{name} {activates}")),
        Node::Filesystem { filesystem, .. } => Some(filesystem.mount_point.clone()),
        Node::DirEntry { path, .. } => Some(path.display().to_string()),
        Node::Disk { disk, .. } => Some(disk.name.clone()),
        Node::Interface { interface, .. } => Some(interface.name.clone()),
        Node::JournalEntry(entry) => Some(format!("{} {}", entry.unit.clone().unwrap_or_else(|| "kernel".to_string()), entry.message)),
        Node::Finding(finding) => Some(finding_text(finding)),
        // The System section's body genuinely is context rather than a
        // row you would search for: it is one block of machine facts,
        // always present, and narrowing to it answers nothing.
        Node::Overview(_) => None,
        // Structure, or a row whose text belongs to another row: neither
        // is matched on its own. `apply` decides what happens to them.
        Node::UnitDetail { .. } | Node::ProcDetail { .. } | Node::SectionHeader { .. } | Node::Spacer => None,
    }
}

/// Keeps rows matching `needle`, case-insensitively, along with the
/// section headers that still have something under them.
///
/// A header whose whole section was filtered out goes too - an empty
/// heading is worse than no heading, the rule every buffer already
/// follows for sections with nothing in them.
///
/// Returns the kept rows and how many of them *matched*, which is a
/// smaller number: a section header kept because something under it
/// survived, and a group row kept because one of its processes did, are
/// both structure rather than results. Counting the returned rows would
/// report a search for a single process as three hits.
///
/// Counted here rather than in a second pass, so the number cannot
/// disagree with the rows it describes.
pub fn apply(rows: Vec<Node>, needle: &str) -> (Vec<Node>, usize) {
    let needle = needle.to_lowercase();
    let matches = |node: &Node| text_of(node).is_some_and(|text| text.to_lowercase().contains(&needle));

    let mut kept: Vec<Node> = Vec::new();
    let mut matched = 0usize;
    // Where the open group row sits, and whether it earned its place on
    // its own. A group is kept when *either* it matches or something under
    // it does - a process row cannot say which unit owns it, so dropping
    // the group leaves the one fact the match was for floating loose.
    let mut group: Option<(usize, bool)> = None;
    for node in rows {
        match &node {
            // A header is kept provisionally and dropped later if nothing
            // followed it.
            Node::SectionHeader { .. } => {
                drop_empty_group(&mut kept, &mut group);
                if matches!(kept.last(), Some(Node::SectionHeader { .. })) {
                    kept.pop();
                }
                kept.push(node);
            }
            // Same rule, one level in: provisional until something under
            // it survives, unless the group's own name matched.
            Node::ProcGroup { .. } => {
                drop_empty_group(&mut kept, &mut group);
                let earned = matches(&node);
                // A group that matched on its own name is a result; one
                // kept because a process under it matched is not, and
                // counting it would report one hit as two.
                matched += usize::from(earned);
                group = Some((kept.len(), earned));
                kept.push(node);
            }
            // A detail row has no text of its own - it is the row above
            // it, spelled out - so it rides along with that row rather
            // than being matched independently. Filtering it on its own
            // dropped it, and opening a unit inside a filtered view then
            // appeared to do nothing at all.
            Node::UnitDetail { unit, .. } => {
                if matches!(kept.last(), Some(Node::Unit { unit: kept_unit, .. }) if kept_unit.name == unit.name) {
                    kept.push(node);
                }
            }
            // Same rule, for the process rows: the block belongs to the
            // row above it, so it survives exactly when that row does.
            Node::ProcDetail { proc, .. } => {
                if matches!(kept.last(), Some(Node::Proc { proc: kept_proc, .. }) if kept_proc.pid == proc.pid) {
                    kept.push(node);
                }
            }
            // Structure with no text of its own rides along with whatever
            // it belongs to rather than being filtered on.
            Node::Spacer => {}
            _ if matches(&node) => {
                if let Some((_, earned)) = group.as_mut() {
                    *earned = true;
                }
                matched += 1;
                kept.push(node);
            }
            _ => {}
        }
    }
    drop_empty_group(&mut kept, &mut group);
    if matches!(kept.last(), Some(Node::SectionHeader { .. })) {
        kept.pop();
    }
    (kept, matched)
}

/// Removes the open group row when nothing justified it - neither its own
/// name nor anything the filter kept beneath it. An empty heading is worse
/// than no heading, the rule every section already follows.
fn drop_empty_group(kept: &mut Vec<Node>, group: &mut Option<(usize, bool)>) {
    if let Some((at, earned)) = group.take()
        && !earned
        && at < kept.len()
    {
        kept.remove(at);
    }
}
