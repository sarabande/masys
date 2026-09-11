//! Filtering rows by a typed string.
//!
//! Applied to whatever the open buffer built, rather than being written
//! per buffer, so `/` behaves the same everywhere. That matters more than
//! the per-buffer precision a bespoke filter would buy: htop, lnav, k9s
//! and less all use `/` and all mean "show me the lines with this in
//! them".

use masys_view::Node;

/// A row's searchable text, or `None` for a row that is structure rather
/// than content.
fn text_of(node: &Node) -> Option<String> {
    match node {
        Node::Proc { proc, .. } => Some(format!("{} {}", proc.pid, proc.comm)),
        Node::ProcGroup { name, .. } => Some(name.clone()),
        Node::Unit { unit, .. } => Some(format!("{} {}", unit.name, unit.sub_state)),
        Node::Timer {
            name, activates, ..
        } => Some(format!("{name} {activates}")),
        Node::Filesystem { filesystem, .. } => Some(filesystem.mount_point.clone()),
        Node::DirEntry { path, .. } => Some(path.display().to_string()),
        Node::Disk { disk, .. } => Some(disk.name.clone()),
        Node::Interface { interface, .. } => Some(interface.name.clone()),
        // Name and version both, so `/1.2` finds a version and `/git`
        // finds a package - the search box is the only way through a list
        // this long.
        Node::Package(package) => Some(format!("{} {}", package.name, package.version)),
        Node::JournalEntry(entry) => Some(format!(
            "{} {}",
            entry.unit.clone().unwrap_or_else(|| "kernel".to_string()),
            entry.message
        )),
        Node::Finding { presentation, .. } => Some(presentation.searchable()),
        // The System section is context rather than rows you would search
        // for: machine facts that are always present, so narrowing to them
        // answers nothing.
        //
        // Still `None` now that each line is its own row, and the effect
        // is unchanged - every line is dropped, so the header follows and
        // the section leaves the buffer whole, exactly as it did when one
        // node carried the lot. Giving these lines real search text is a
        // decision about what `/mem` should mean, not a consequence of
        // splitting them, so it is not made here.
        Node::OverviewLine { .. } => None,
        // A generation is found by its number or its version: `437` and
        // `26.11` are both things you would type to reach one, and the
        // number is what the rollback action names.
        Node::Generation { generation, .. } => Some(format!(
            "{} {}",
            generation.id,
            generation.label.clone().unwrap_or_default()
        )),
        Node::Input { input, .. } => Some(format!(
            "{} {}",
            input.name,
            input.origin.clone().unwrap_or_default()
        )),
        // The job and the unit behind it, because either is a reasonable
        // thing to search for: `gc` is what it does, `nix-gc.timer` is
        // what the systemd buffer calls it.
        Node::NixPolicy { job, unit, .. } => Some(format!("{job} {unit}")),
        // A block of machine facts rather than a row you would search
        // for, the same judgement `OverviewLine` gets above: it is always
        // present - the Nix buffer's equivalent of the Status buffer's
        // System section - and narrowing to it answers nothing.
        Node::NixStore { .. } => None,
        // Not a fact block like `NixStore`: this row exists only when
        // `DeclarativeService::reboot()` returned `Some`, the same
        // conditional-alert shape as `FindingKind::ClockUnsynchronized` and
        // `FindingKind::SystemDegraded` above, so it gets the same kind of
        // real search text they do.
        Node::RebootPending {
            kernel_changed,
            initrd_changed,
            ..
        } => Some(match (*kernel_changed, *initrd_changed) {
            (true, true) => "reboot pending kernel initrd changed".to_string(),
            (true, false) => "reboot pending kernel changed".to_string(),
            (false, true) => "reboot pending initrd changed".to_string(),
            (false, false) => "reboot pending".to_string(),
        }),
        // Structure, or a row whose text belongs to another row: neither
        // is matched on its own. `apply` decides what happens to them.
        Node::UnitDetail { .. }
        | Node::ProcDetail { .. }
        | Node::SectionHeader { .. }
        | Node::Spacer => None,
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
    let matches =
        |node: &Node| text_of(node).is_some_and(|text| text.to_lowercase().contains(&needle));

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
            // dropped it, and opening a unit inside a filtered buffer then
            // appeared to do nothing at all.
            Node::UnitDetail { unit, .. } => {
                if matches!(kept.last(), Some(Node::Unit { unit: kept_unit, .. }) if kept_unit.name == unit.name)
                {
                    kept.push(node);
                }
            }
            // Same rule, for the process rows: the block belongs to the
            // row above it, so it survives exactly when that row does.
            Node::ProcDetail { proc, .. } => {
                if matches!(kept.last(), Some(Node::Proc { proc: kept_proc, .. }) if kept_proc.pid == proc.pid)
                {
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
