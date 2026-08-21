//! The walker, against real directories.
//!
//! Real trees rather than a filesystem double: the whole point of this
//! crate is what `read_dir` and `stat` actually do - block accounting,
//! hardlinks, permission walls, mount boundaries - and a fake would be a
//! restatement of what this code already assumes.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use masys_scan::{ScanState, walk};

static UNIQUE: AtomicU32 = AtomicU32::new(0);

/// A temp directory that removes itself.
struct Tree(PathBuf);

impl Tree {
    fn new() -> Tree {
        let path = std::env::temp_dir().join(format!("masys-scan-{}-{}", std::process::id(), UNIQUE.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&path).expect("a temp tree");
        Tree(path)
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let path = self.0.join(rel);
        fs::create_dir_all(&path).expect("a directory");
        path
    }

    /// A file of `bytes` zeroes. Written rather than sparse, so it
    /// occupies the blocks the walker is counting.
    fn file(&self, rel: &str, bytes: usize) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().expect("a parent")).expect("a parent directory");
        fs::write(&path, vec![0u8; bytes]).expect("a file");
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        // The permission test leaves behind a directory nothing can
        // traverse, which `remove_dir_all` cannot get into either.
        if let Ok(entries) = fs::read_dir(&self.0) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata()
                    && meta.is_dir()
                {
                    let mut perms = meta.permissions();
                    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
                    let _ = fs::set_permissions(entry.path(), perms);
                }
            }
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The size a directory was recorded as, if it was retained at all.
fn bytes_of(progress: &masys_domain::scan::ScanProgress, path: &Path) -> Option<u64> {
    progress.dirs.iter().find(|d| d.path == path).map(|d| d.bytes)
}

/// Block accounting, like `du`: the total is what the tree occupies, so
/// it is at least what was written and rounded up to whole blocks.
#[test]
fn a_tree_is_summed_from_its_files() {
    let tree = Tree::new();
    tree.file("a/one.bin", 40_000);
    tree.file("a/two.bin", 40_000);
    tree.file("b/three.bin", 20_000);

    let state = ScanState::new();
    walk(tree.path(), 0, &state);
    let progress = state.progress();

    let root = bytes_of(&progress, tree.path()).expect("the root was retained");
    assert!(root >= 100_000, "at least what was written: {root}");
    let a = bytes_of(&progress, &tree.0.join("a")).expect("a was retained");
    let b = bytes_of(&progress, &tree.0.join("b")).expect("b was retained");
    assert!(a > b, "a holds twice what b does: a={a} b={b}");
    assert!(root >= a + b, "the root holds both: root={root} a={a} b={b}");
    assert!(progress.done, "a finished walk says so");
}

/// A directory occupies blocks itself, and `du` counts them. Ignoring
/// them understated a real tree by 34 MB - 7,716 directories at 4 KB
/// each - which is small in ratio and exactly the kind of quiet drift
/// that makes a number nobody can reconcile with `df`.
#[test]
fn a_directory_costs_its_own_blocks() {
    let tree = Tree::new();
    tree.dir("empty/one");
    tree.dir("empty/two");

    let state = ScanState::new();
    walk(tree.path(), 0, &state);
    let progress = state.progress();

    let empty = bytes_of(&progress, &tree.0.join("empty")).expect("retained");
    assert!(empty > 0, "three empty directories still occupy blocks: {empty}");
}

/// The floor is what keeps the tree in kilobytes rather than the 70-100MB
/// a real filesystem's 575,847 directories would take. A folded child's
/// bytes still count in its parent - the total is the one thing that must
/// not change.
#[test]
fn a_directory_under_the_floor_folds_into_its_parent() {
    let tree = Tree::new();
    tree.file("big/large.bin", 200_000);
    tree.file("small/tiny.bin", 100);

    let unbounded = {
        let state = ScanState::new();
        walk(tree.path(), 0, &state);
        state.progress()
    };
    let bounded = {
        let state = ScanState::new();
        walk(tree.path(), 50_000, &state);
        state.progress()
    };

    assert!(bytes_of(&unbounded, &tree.0.join("small")).is_some(), "kept with no floor");
    assert!(bytes_of(&bounded, &tree.0.join("small")).is_none(), "dropped under the floor");
    assert!(bytes_of(&bounded, &tree.0.join("big")).is_some(), "the big one stays");
    assert_eq!(
        bytes_of(&unbounded, tree.path()),
        bytes_of(&bounded, tree.path()),
        "the root's total is the same either way - folding hides a row, not bytes"
    );
}

/// An unprivileged scan of `/` meets plenty of these. Counting them is
/// what explains a total that looks too small; aborting on them would
/// make the feature useless anywhere but a home directory.
#[test]
fn an_unreadable_directory_is_counted_rather_than_fatal() {
    let tree = Tree::new();
    tree.file("readable/file.bin", 30_000);
    let shut = tree.dir("shut");
    tree.file("shut/hidden.bin", 30_000);
    let mut perms = fs::metadata(&shut).expect("metadata").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o000);
    fs::set_permissions(&shut, perms).expect("chmod");

    let state = ScanState::new();
    walk(tree.path(), 0, &state);
    let progress = state.progress();

    assert_eq!(progress.unreadable, 1, "the shut directory was counted: {progress:?}");
    assert!(bytes_of(&progress, &tree.0.join("readable")).is_some(), "the rest was still walked");
    assert!(progress.done, "and the walk still finished");
    // Its contents are unreachable, but the directory itself still
    // occupies blocks and can still be stat'd - and `du` counts them.
    // Returning zero here left masys 36,864 bytes short of `du` over
    // /home/user, which is the whole of the remaining disagreement.
    let shut_bytes = bytes_of(&progress, &shut).expect("the shut directory is still a row");
    assert!(shut_bytes > 0, "an unreadable directory still costs its own blocks: {shut_bytes}");
}

/// `du -x`, and not a nicety: this host has 26TB of NFS under `/mnt`, and
/// a scan that wandered in would take hours.
///
/// `/run` rather than `/`: it is a few megabytes and carries half a dozen
/// mounts of its own, so it proves the same thing in milliseconds. Walking
/// `/` for this took 17s, which is not a price a test suite should pay to
/// learn something a small tree already shows.
#[test]
fn a_walk_does_not_cross_a_mount_boundary() {
    let run = Path::new("/run");
    if !run.is_dir() {
        return;
    }
    let state = ScanState::new();
    walk(run, 0, &state);
    let progress = state.progress();

    // The mount *point* belongs to the parent filesystem and is fine to
    // record; anything beneath it is a different filesystem and is not.
    let crossed: Vec<&PathBuf> = progress.dirs.iter().map(|d| &d.path).filter(|p| p.starts_with("/run/user/1000/")).collect();
    assert!(crossed.is_empty(), "the walk crossed into another filesystem: {crossed:?}");
    assert!(progress.done);
}

/// A hardlinked file occupies its blocks once, and `du` counts it once.
/// `/nix/store` is built out of hardlinks, so double-counting would
/// overstate the largest tree on a NixOS host.
#[test]
fn a_hardlink_is_counted_once() {
    let tree = Tree::new();
    tree.file("one/file.bin", 80_000);
    fs::hard_link(tree.0.join("one/file.bin"), tree.0.join("one/link.bin")).expect("a hardlink");

    let state = ScanState::new();
    walk(tree.path(), 0, &state);
    let progress = state.progress();

    let one = bytes_of(&progress, &tree.0.join("one")).expect("retained");
    assert!(one < 160_000, "counted once, not twice: {one}");
}
