//! The IO buffer: what the machine's pipes are carrying, and how full
//! its storage is.
//!
//! Devices and filesystems together because they are the same question
//! asked at two levels - a disk saturated at 100% busy and a filesystem
//! at 99% full are both "storage is the problem", and finding out which
//! should not mean visiting two screens. Interfaces are here for the same
//! reason one level out: "the machine feels slow" is answered by the
//! disk, the filesystem or the link, and which one it is should not
//! decide which screen you are on.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use masys_domain::rate::{DiskRate, NetRate};
use masys_domain::sample::{Disk, Filesystem, Interface};
use masys_domain::scan::ScanProgress;
use masys_view::{Node, SectionKind};

/// Devices first, then filesystems.
///
/// Devices lead because they are the live signal: a filesystem's fullness
/// changes over hours, while a device pinned at 100% busy is what makes
/// the machine feel broken right now.
///
/// Filesystems are ordered by fullness with read-only first - the kernel
/// remounts after an I/O error at any percentage, and that outranks a
/// number. Devices are ordered by how busy they are, falling back to name
/// so the list is stable before any rate exists.
pub fn build_io_rows(
    filesystems: &[Filesystem],
    disks: &[Disk],
    rates: &[(String, DiskRate)],
    interfaces: &[Interface],
    net_rates: &[(String, NetRate)],
    scan: &ScanProgress,
    open_dirs: &HashSet<PathBuf>,
) -> Vec<Node> {
    let mut rows = Vec::new();
    let rate_of = |name: &str| rates.iter().find(|(n, _)| n == name).map(|(_, r)| *r);
    let net_rate_of = |name: &str| net_rates.iter().find(|(n, _)| n == name).map(|(_, r)| *r);

    if !disks.is_empty() {
        let mut ordered: Vec<&Disk> = disks.iter().collect();
        ordered.sort_by(|a, b| {
            let busy = |d: &Disk| rate_of(&d.name).map(|r| r.busy_percent).unwrap_or(0.0);
            busy(b).total_cmp(&busy(a)).then_with(|| a.name.cmp(&b.name))
        });
        rows.push(Node::SectionHeader { title: "Devices".to_string(), kind: SectionKind::Devices, count: Some(ordered.len() as u32) });
        rows.extend(ordered.into_iter().map(|disk| Node::Disk { rate: rate_of(&disk.name), disk: disk.clone() }));
        rows.push(Node::Spacer);
    }

    // Between the devices and the filesystems: it is the same question
    // as a disk - what is this pipe carrying, and is it in trouble -
    // asked of a different kind of pipe.
    let visible: Vec<&Interface> = interfaces.iter().filter(|i| is_worth_showing(i)).collect();
    if !visible.is_empty() {
        let mut ordered = visible;
        ordered.sort_by(|a, b| {
            let throughput = |i: &Interface| net_rate_of(&i.name).map(|r| r.rx_bytes_per_sec + r.tx_bytes_per_sec).unwrap_or(0.0);
            throughput(b).total_cmp(&throughput(a)).then_with(|| a.name.cmp(&b.name))
        });
        rows.push(Node::SectionHeader { title: "Network".to_string(), kind: SectionKind::Network, count: Some(ordered.len() as u32) });
        rows.extend(
            ordered.into_iter().map(|interface| Node::Interface { rate: net_rate_of(&interface.name), interface: interface.clone() }),
        );
        rows.push(Node::Spacer);
    }

    if !filesystems.is_empty() {
        let mut ordered: Vec<&Filesystem> = filesystems.iter().collect();
        ordered.sort_by(|a, b| {
            b.read_only
                .cmp(&a.read_only)
                .then_with(|| b.used_percent.total_cmp(&a.used_percent))
                .then_with(|| a.mount_point.cmp(&b.mount_point))
        });
        rows.push(Node::SectionHeader {
            title: "Filesystems".to_string(),
            kind: SectionKind::Filesystems,
            count: Some(ordered.len() as u32),
        });
        for filesystem in ordered {
            let scanned = scan.root.as_deref() == Some(Path::new(&filesystem.mount_point));
            rows.push(Node::Filesystem { filesystem: filesystem.clone(), expanded: scanned });
            if scanned {
                let root = PathBuf::from(&filesystem.mount_point);
                let total = scan.dirs.iter().find(|d| d.path == root).map(|d| d.bytes).unwrap_or(scan.counted_bytes);
                push_dirs(&mut rows, scan, &root, open_dirs, 1, total);
            }
        }
    }

    if matches!(rows.last(), Some(Node::Spacer)) {
        rows.pop();
    }
    rows
}

/// Whether an interface can be carrying traffic worth looking at.
///
/// Three rules, each earned on the development host, which lists nine
/// interfaces and has two worth a row:
///
/// - **Not loopback.** The machine talking to itself is not a network
///   path. It cannot be filtered by traffic either: `lo` has moved 3.8 MB
///   here.
/// - **Not down.** `docker0` is down and *has* moved 2,117 bytes, so a
///   traffic test alone would keep it. This is why `up` is a field rather
///   than something inferred from the counters.
/// - **Has moved a byte.** Four bridges and an unplugged USB NIC are up
///   with nothing to say.
fn is_worth_showing(interface: &Interface) -> bool {
    !interface.loopback && interface.up && (interface.rx_bytes > 0 || interface.tx_bytes > 0)
}

/// The retained children of `parent`, and recursively the children of any
/// that are open.
///
/// One level at a time rather than the whole retained tree flattened: a
/// scan of `/` retains thousands of directories, and a dump of all of
/// them is not an answer to "what is using the disk", it is the same
/// problem in a different shape.
fn push_dirs(rows: &mut Vec<Node>, scan: &ScanProgress, parent: &Path, open_dirs: &HashSet<PathBuf>, depth: u32, _parent_bytes: u64) {
    let children = scan.children_of(parent);
    // Against the largest sibling rather than against the parent's total,
    // because a bar drawn against the total is empty for every row but
    // one: `/home/user/tools` is 53G of a 622G tree - 8.5%, which rounds
    // to no blocks - and ten rows of an empty bar say nothing. Against
    // the biggest sibling the column becomes a comparison, which is the
    // question being asked at every level.
    let largest = children.first().map(|d| d.bytes).unwrap_or(0);
    for child in children {
        let expanded = open_dirs.contains(&child.path);
        let share = if largest > 0 { child.bytes as f32 / largest as f32 } else { 0.0 };
        rows.push(Node::DirEntry {
            path: child.path.clone(),
            bytes: child.bytes,
            depth,
            share,
            expanded,
            has_children: !scan.children_of(&child.path).is_empty(),
        });
        if expanded {
            push_dirs(rows, scan, &child.path, open_dirs, depth + 1, child.bytes);
        }
    }
}
