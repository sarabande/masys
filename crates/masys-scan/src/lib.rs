//! What is using the disk: a parallel directory walk behind a channel.
//!
//! The one place in masys that spends threads. `masys-design.md` allows
//! exactly this and says when: "the fix is moving *that one source*
//! behind a thread and channel, not making everything async." A scan is
//! 10 s against a 2 s tick - a 500% duty cycle - and is the source that
//! earns it.
//!
//! Parallel because it is the only thing that helps. Measured against
//! `dust`, which does the same: `du -sh /home/user` takes 9.80 s of wall
//! for 10.39 s of CPU, and `dust` takes 3.03 s of wall for 15.25 s of
//! CPU. It is not cleverer - it burns *more* total CPU and wins by
//! spreading it over five cores.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use masys_domain::scan::{DirScanner, DirSize, ScanProgress};

/// A walk in progress, shared between the pool and whoever is polling it.
///
/// Public because it is what makes the walker testable without a thread:
/// `walk` fills one of these synchronously, and `RayonScanner` is only
/// the shell that runs `walk` somewhere else.
#[derive(Debug, Default)]
pub struct ScanState {
    root: Mutex<Option<PathBuf>>,
    dirs: Mutex<HashMap<PathBuf, u64>>,
    /// Files with more than one link, so a hardlinked tree is not counted
    /// twice. Only multiply-linked files go in: on an ordinary tree that
    /// is almost none of them, which keeps this lock cold.
    linked: Mutex<HashSet<(u64, u64)>>,
    counted_bytes: AtomicU64,
    dirs_seen: AtomicU64,
    unreadable: AtomicU64,
    done: AtomicBool,
    cancelled: AtomicBool,
}

impl ScanState {
    pub fn new() -> ScanState {
        ScanState::default()
    }

    /// Everything counted so far. Never blocks on the walk itself.
    pub fn progress(&self) -> ScanProgress {
        let dirs = self.dirs.lock().expect("scan state");
        ScanProgress {
            root: self.root.lock().expect("scan state").clone(),
            dirs: dirs.iter().map(|(path, bytes)| DirSize { path: path.clone(), bytes: *bytes }).collect(),
            counted_bytes: self.counted_bytes.load(Ordering::Relaxed),
            dirs_seen: self.dirs_seen.load(Ordering::Relaxed),
            unreadable: self.unreadable.load(Ordering::Relaxed),
            done: self.done.load(Ordering::Relaxed),
        }
    }

    /// Asks the walk to stop. It checks between directories, so a scan
    /// ends within one `read_dir` rather than instantly.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Whether this file's blocks have already been counted through
    /// another link to the same inode.
    fn already_counted(&self, meta: &std::fs::Metadata) -> bool {
        if meta.nlink() <= 1 {
            return false;
        }
        !self.linked.lock().expect("scan state").insert((meta.dev(), meta.ino()))
    }
}

/// Sums `root`, retaining every directory of at least `floor` bytes.
///
/// Synchronous: it returns when the walk is done, or when `state` has
/// been cancelled. `RayonScanner` is what puts it somewhere else.
///
/// Never leaves the filesystem it started on - `du -x`, enforced by
/// `st_dev`. Not a nicety: the development host has 26 TB of NFS under
/// `/mnt`, and a scan that wandered in would take hours.
pub fn walk(root: &Path, floor: u64, state: &ScanState) {
    *state.root.lock().expect("scan state") = Some(root.to_path_buf());
    let device = std::fs::symlink_metadata(root).map(|m| m.dev()).ok();
    if let Some(device) = device {
        walk_dir(root, device, floor, state);
    }
    state.done.store(true, Ordering::Relaxed);
}

/// One directory's total, including everything beneath it.
///
/// Recursion rather than an explicit stack: a real tree is tens of levels
/// deep, and rayon's work-stealing wants the recursion to hand it
/// independent subtrees.
fn walk_dir(path: &Path, device: u64, floor: u64, state: &ScanState) -> u64 {
    if state.is_cancelled() {
        return 0;
    }
    state.dirs_seen.fetch_add(1, Ordering::Relaxed);

    // A directory occupies blocks itself, and `du` counts them. Leaving
    // them out understated `/home/user/tools` by 34 MB - 7,716
    // directories at 4 KB each - which is the kind of quiet drift that
    // leaves a total nobody can reconcile against `df`.
    //
    // Read before the directory is opened, because it is true whether or
    // not it can be: an unreadable directory can still be stat'd, and
    // `du` counts its blocks too.
    let mut here = std::fs::symlink_metadata(path).map(|m| m.blocks() * 512).unwrap_or(0);

    let Ok(entries) = std::fs::read_dir(path) else {
        // A permission wall, or a directory that vanished mid-walk.
        // Counted, never fatal: an unprivileged scan of `/` meets plenty,
        // and the count is what explains a total that looks too small.
        state.unreadable.fetch_add(1, Ordering::Relaxed);
        state.counted_bytes.fetch_add(here, Ordering::Relaxed);
        if here >= floor {
            state.dirs.lock().expect("scan state").insert(path.to_path_buf(), here);
        }
        return here;
    };

    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        // `symlink_metadata`, so a symlink is counted as the link it is
        // rather than followed into another tree - or into a cycle.
        let Ok(meta) = entry.metadata() else { continue };
        if meta.dev() != device {
            // A mount point. Its own filesystem is somebody else's scan.
            continue;
        }
        if meta.is_dir() {
            subdirs.push(entry.path());
            continue;
        }
        if state.already_counted(&meta) {
            continue;
        }
        // Blocks, not apparent size - what `du` reports, and the only
        // figure comparable to what `df` says is used.
        here += meta.blocks() * 512;
    }
    state.counted_bytes.fetch_add(here, Ordering::Relaxed);

    // The parallelism, and the whole reason for the crate: independent
    // subtrees go to the pool, and rayon steals work between them.
    let below: u64 = if subdirs.len() > 1 {
        use rayon::prelude::*;
        subdirs.par_iter().map(|child| walk_dir(child, device, floor, state)).sum()
    } else {
        subdirs.iter().map(|child| walk_dir(child, device, floor, state)).sum()
    };

    let total = here + below;
    // Under the floor the directory folds into its parent: its bytes are
    // in `total` either way, so a fold hides a row and never a byte.
    if total >= floor {
        state.dirs.lock().expect("scan state").insert(path.to_path_buf(), total);
    }
    total
}

/// The smallest directory worth a row, for a filesystem holding
/// `used_bytes`.
///
/// A real filesystem has hundreds of thousands of directories - 575,847
/// on this host's root - and retaining them all would cost 70-100 MB,
/// which a 2-second-tick monitor may not become. Almost none of them
/// matter: at a 10 MB floor, 1,397 of `/nix/store`'s 343,211 directories
/// survive.
///
/// Scaled to the filesystem rather than fixed, so the same rule serves a
/// 500 MB tmpfs and a 1 TB root, and never below 1 MB.
pub fn floor_for(used_bytes: u64) -> u64 {
    (used_bytes / 10_000).max(1 << 20)
}

/// The `DirScanner` a real machine gets: `walk` on a rayon pool, behind a
/// handle the event loop can poll without blocking.
pub struct RayonScanner {
    state: Mutex<Arc<ScanState>>,
    pool: rayon::ThreadPool,
    floor: u64,
}

/// How many threads the pool gets.
///
/// Capped rather than sized to the machine: eight threads help on an SSD
/// and hurt on a spinning or network-backed mount, where concurrent seeks
/// are slower than sequential ones.
const MAX_THREADS: usize = 8;

impl RayonScanner {
    /// `used_bytes` sets the retention floor - see `floor_for`. It comes
    /// from `statvfs`, so it is known before the walk starts.
    pub fn new(used_bytes: u64) -> Result<RayonScanner, String> {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).min(MAX_THREADS);
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().map_err(|e| e.to_string())?;
        Ok(RayonScanner { state: Mutex::new(Arc::new(ScanState::new())), pool, floor: floor_for(used_bytes) })
    }
}

impl DirScanner for RayonScanner {
    fn start(&self, root: &Path) {
        // The old scan is cancelled and dropped rather than waited on:
        // its threads notice between directories and unwind, and nothing
        // that arrives late can reach the new state.
        let fresh = Arc::new(ScanState::new());
        {
            let mut slot = self.state.lock().expect("scanner");
            slot.cancel();
            *slot = Arc::clone(&fresh);
        }
        let root = root.to_path_buf();
        let floor = self.floor;
        // `spawn` rather than `install`: this returns immediately, which
        // is the whole contract of the port.
        self.pool.spawn(move || walk(&root, floor, &fresh));
    }

    fn poll(&self) -> ScanProgress {
        self.state.lock().expect("scanner").progress()
    }

    fn cancel(&self) {
        let mut slot = self.state.lock().expect("scanner");
        slot.cancel();
        *slot = Arc::new(ScanState::new());
    }
}
