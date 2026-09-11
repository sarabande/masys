//! The IO buffer: what the machine's pipes are carrying, and how full
//! its storage is.
//!
//! Devices and filesystems together because they are the same question
//! asked at two levels - a disk saturated at 100% busy and a filesystem
//! at 99% full are both "storage is the problem", and finding out which
//! should not mean visiting two buffers. Interfaces are here for the same
//! reason one level out: "the machine feels slow" is answered by the
//! disk, the filesystem or the link, and which one it is should not
//! decide which buffer you are on.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use masys_domain::rate::{self, DiskRate, NetRate, Throughput};
use masys_domain::sample::{Disk, Filesystem, Interface, Snapshot};
use masys_domain::scan::{DirScanner, ScanProgress};
use masys_view::{Node, SectionKind};

/// The IO buffer: the walk it tracks, and the throughput it derives.
///
/// Four of these were fields of `App` and two lived in its `Facts`, which
/// is what made the row builder a seven-parameter call: state on one side
/// of a seam and the behaviour over it on the other. What crosses now is
/// only what the session genuinely shares - the filesystems, devices and
/// links that arrive in the one sample every buffer reads.
///
/// The `DirScanner` is a **port**, so it is handed to the two methods
/// that ask it something rather than held here - the same arrangement
/// `NixBuffer` has with `DeclarativeService`. Ports belong to the
/// composition root, and a buffer that owned one would be a buffer only
/// the composition root could build; passing it is what makes this type
/// `Default` and its layout testable with no scanner at all. It is still
/// the one source too slow for the tick's duty cycle - summing a
/// directory tree is seconds - which is why it runs on its own threads
/// and this type only drains it.
///
/// The fields are public for `NixBuffer`'s reason: they are readings and
/// scan state rather than invariants this type alone can state. The rules
/// over them - at most one scan, and its tree forgotten when it stops -
/// belong to [`IoBuffer::refresh`] and [`IoBuffer::open_filesystem`],
/// which are the only writers in the program.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct IoBuffer {
    /// The filesystem whose contents are open, if any. At most one: a
    /// scan is expensive and two would compete for the same disk.
    pub scan_root: Option<PathBuf>,
    /// What the scan has found, refreshed every tick while it runs.
    pub scan: ScanProgress,
    /// Directories opened inside the scan, by path.
    pub open_dirs: HashSet<PathBuf>,
    /// Per-device and per-link throughput. Derivatives, so both are empty
    /// until a second sample exists to derive against - `0 B/s` for a
    /// device nobody has measured twice would be a reading masys did not
    /// take.
    pub disk_rates: Vec<(String, DiskRate)>,
    pub net_rates: Vec<(String, NetRate)>,
    /// The same rates, added up - what the whole machine is moving.
    ///
    /// Here rather than computed by whoever wants a total, so that the
    /// figure the Status header shows and the rows in this buffer are one
    /// reading rather than two that agree today. The network total in
    /// particular cannot be recovered from `net_rates` alone: it is the
    /// sum over the links this buffer *shows*, and `is_worth_showing`
    /// drops loopback, which has moved 3.8 MB on the development host.
    ///
    /// `None` until a second sample exists, like the rates they come
    /// from, and cleared rather than carried when there is no second
    /// sample - a total held over from a previous tick would be a figure
    /// with a timestamp nobody could see.
    pub disk_total: Option<Throughput>,
    pub net_total: Option<Throughput>,
}

impl IoBuffer {
    /// Throughput against the previous sample, and whatever the walk has
    /// counted since the last tick.
    ///
    /// `previous` is `None` on the first tick, and both rates stay empty
    /// rather than defaulting to zero.
    ///
    /// The walk is drained, never waited on - it is on its own threads
    /// and this is the seam where what it has found so far crosses back -
    /// and only while a scan is actually running, because `poll` on an
    /// idle scanner would overwrite what the last one found with nothing.
    pub fn refresh(
        &mut self,
        scanner: &dyn DirScanner,
        previous: Option<&Snapshot>,
        current: &Snapshot,
    ) {
        if let Some(previous) = previous {
            self.disk_rates = rate::derive_disks(previous, current);
            self.net_rates = rate::derive_nets(previous, current);
            self.disk_total = Some(Throughput::of_disks(&self.disk_rates));
            // Summed over the links this buffer shows, not over every
            // link the kernel lists: `lo` is not a network path, and a
            // total that counted it would disagree with the rows
            // underneath it - which is the one thing a second copy of a
            // figure must never do.
            let shown: Vec<(String, NetRate)> = self
                .net_rates
                .iter()
                .filter(|(name, _)| {
                    current
                        .interfaces
                        .iter()
                        .any(|i| &i.name == name && is_worth_showing(i))
                })
                .cloned()
                .collect();
            self.net_total = Some(Throughput::of_nets(&shown));
        } else {
            // No second sample, so no rate - and nothing kept from
            // whenever there last was one.
            self.disk_total = None;
            self.net_total = None;
        }
        if self.scan_root.is_some() {
            self.scan = scanner.poll();
        }
    }

    /// Every row this buffer shows.
    ///
    /// Three parameters, and each is a reading the session shares rather
    /// than one this buffer holds: devices, filesystems and links all
    /// arrive in the single sample.
    pub fn rows(
        &self,
        filesystems: &[Filesystem],
        disks: &[Disk],
        interfaces: &[Interface],
    ) -> Vec<Node> {
        layout(
            filesystems,
            disks,
            &self.disk_rates,
            interfaces,
            &self.net_rates,
            &self.scan,
            &self.open_dirs,
        )
    }

    /// `enter` on a filesystem row: the question "what is using this".
    ///
    /// The only thing that starts a scan - masys never walks a filesystem
    /// nobody asked about - and pressing it again on the one already open
    /// stops it, because a scan nobody is looking at is CPU nobody asked
    /// to spend, and on a pool that is several cores of it.
    pub fn open_filesystem(&mut self, scanner: &dyn DirScanner, mount: PathBuf) {
        if self.scan_root.as_deref() == Some(mount.as_path()) {
            self.close(scanner);
            return;
        }
        scanner.start(&mount);
        self.scan = ScanProgress::default();
        self.open_dirs.clear();
        self.scan_root = Some(mount);
    }

    /// `enter` on a directory row.
    ///
    /// Opens from the tree the scan already built, so drilling in costs
    /// nothing further - the same reason `dust` can show any depth after
    /// one pass.
    pub fn toggle_dir(&mut self, path: PathBuf) {
        if !self.open_dirs.remove(&path) {
            self.open_dirs.insert(path);
        }
    }

    /// Stops the scan and forgets what it found.
    fn close(&mut self, scanner: &dyn DirScanner) {
        scanner.cancel();
        self.scan = ScanProgress::default();
        self.scan_root = None;
        self.open_dirs.clear();
    }
}

/// Devices first, then filesystems.
///
/// Private, and an *internal* seam rather than this module's interface.
/// Four of its seven arguments are [`IoBuffer`]'s own fields; callers
/// reach it through [`IoBuffer::rows`], which names the other three.
///
/// Devices lead because they are the live signal: a filesystem's fullness
/// changes over hours, while a device pinned at 100% busy is what makes
/// the machine feel broken right now.
///
/// Filesystems are ordered by fullness with read-only first - the kernel
/// remounts after an I/O error at any percentage, and that outranks a
/// number. Devices are ordered by how busy they are, falling back to name
/// so the list is stable before any rate exists.
fn layout(
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
            busy(b)
                .total_cmp(&busy(a))
                .then_with(|| a.name.cmp(&b.name))
        });
        rows.push(Node::SectionHeader {
            title: "Devices".to_string(),
            kind: SectionKind::Devices,
            count: Some(ordered.len() as u32),
        });
        rows.extend(ordered.into_iter().map(|disk| Node::Disk {
            rate: rate_of(&disk.name),
            disk: disk.clone(),
        }));
        rows.push(Node::Spacer);
    }

    // Between the devices and the filesystems: it is the same question
    // as a disk - what is this pipe carrying, and is it in trouble -
    // asked of a different kind of pipe.
    let visible: Vec<&Interface> = interfaces.iter().filter(|i| is_worth_showing(i)).collect();
    if !visible.is_empty() {
        let mut ordered = visible;
        ordered.sort_by(|a, b| {
            let throughput = |i: &Interface| {
                net_rate_of(&i.name)
                    .map(|r| r.rx_bytes_per_sec + r.tx_bytes_per_sec)
                    .unwrap_or(0.0)
            };
            throughput(b)
                .total_cmp(&throughput(a))
                .then_with(|| a.name.cmp(&b.name))
        });
        rows.push(Node::SectionHeader {
            title: "Network".to_string(),
            kind: SectionKind::Network,
            count: Some(ordered.len() as u32),
        });
        rows.extend(ordered.into_iter().map(|interface| Node::Interface {
            rate: net_rate_of(&interface.name),
            interface: interface.clone(),
        }));
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
            rows.push(Node::Filesystem {
                filesystem: filesystem.clone(),
                expanded: scanned,
            });
            if scanned {
                let root = PathBuf::from(&filesystem.mount_point);
                push_dirs(&mut rows, scan, &root, open_dirs, 1);
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
fn push_dirs(
    rows: &mut Vec<Node>,
    scan: &ScanProgress,
    parent: &Path,
    open_dirs: &HashSet<PathBuf>,
    depth: u32,
) {
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
        let share = if largest > 0 {
            child.bytes as f32 / largest as f32
        } else {
            0.0
        };
        rows.push(Node::DirEntry {
            path: child.path.clone(),
            bytes: child.bytes,
            depth,
            share,
            expanded,
            has_children: !scan.children_of(&child.path).is_empty(),
        });
        if expanded {
            push_dirs(rows, scan, &child.path, open_dirs, depth + 1);
        }
    }
}
