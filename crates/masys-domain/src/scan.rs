//! What is using the disk: the shape of a directory scan's answer.
//!
//! The IO buffer says how full a filesystem is. This says why. Summing a
//! tree is trivial logic and expensive I/O - 10 s against a 2 s tick on
//! the development host - so the port is deliberately not a
//! `Result<T, E>` that a caller waits on. It is start, poll, cancel, and
//! the adapter behind it is free to use a thread pool without the layers
//! above knowing.

use std::path::{Path, PathBuf};

/// One directory the scan retained, with everything beneath it counted.
///
/// `bytes` includes the whole subtree, folded-away children included, so
/// a parent's total is right whether or not its children were kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirSize {
    pub path: PathBuf,
    pub bytes: u64,
}

/// What a scan has found so far.
///
/// Deliberately a snapshot rather than a stream of deltas: the caller
/// rebuilds its rows every tick regardless, and a snapshot cannot get out
/// of step with what is on screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanProgress {
    /// What was asked for, or `None` when no scan has been started.
    pub root: Option<PathBuf>,
    /// Retained directories, in no particular order. Bounded by a size
    /// floor - see `masys-scan` - because a real filesystem holds
    /// hundreds of thousands of directories and almost none of them are
    /// ever the answer.
    pub dirs: Vec<DirSize>,
    /// Bytes counted so far. Reported instead of a percentage: a walk and
    /// `statvfs` legitimately disagree, so a bar computed from the two
    /// would stall short of the end on a filesystem the user cannot
    /// wholly read, and a bar that never fills reads as a hang.
    pub counted_bytes: u64,
    /// Directories visited, retained or not.
    pub dirs_seen: u64,
    /// Directories that could not be opened. Not an error: an
    /// unprivileged scan of `/` meets plenty, and this count is the
    /// honest explanation for a total that looks too small.
    pub unreadable: u64,
    /// Whether the walk has finished. A scan announces completion by
    /// finishing, never by reaching a percentage.
    pub done: bool,
}

impl ScanProgress {
    /// The retained children of `parent`, largest first.
    ///
    /// Only direct children: the tree is flat on the wire, and the view
    /// asks one level at a time as rows are opened.
    pub fn children_of(&self, parent: &Path) -> Vec<&DirSize> {
        let mut children: Vec<&DirSize> = self.dirs.iter().filter(|d| d.path.parent() == Some(parent)).collect();
        children.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
        children
    }
}

/// Summing a directory tree, without blocking the caller.
///
/// The one port in masys that is not request/response, because the work
/// it fronts cannot be: `masys-design.md` allows exactly this - "the fix
/// is moving *that one source* behind a thread and channel, not making
/// everything async" - and a scan is the source that earns it.
pub trait DirScanner {
    /// Begin scanning `root`, abandoning any scan already in flight.
    fn start(&self, root: &Path);
    /// Everything counted so far. Never blocks.
    fn poll(&self) -> ScanProgress;
    /// Stop, and forget what was found.
    fn cancel(&self);
}
