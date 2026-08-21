use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use masys_domain::declarative::ProfileKind;
use masys_platform_nixos::profiles::read_profile;

static UNIQUE: AtomicU32 = AtomicU32::new(0);

/// A profile directory under the system temp directory, unique per test -
/// pid plus an atomic counter, the same pair `masys-scan/tests/walk.rs`
/// uses - and removed on drop. A fixed name here collided across two
/// concurrent runs, or two users on one machine; this can't.
///
/// Not created by `new` itself: `a_missing_profile_directory_reads_as_absent`
/// needs a path that does not exist at all, so only `fixture` makes one
/// real.
struct ProfileDir(PathBuf);

impl ProfileDir {
    fn new(label: &str) -> ProfileDir {
        let path =
            std::env::temp_dir().join(format!("masys-profiles-{label}-{}-{}", std::process::id(), UNIQUE.fetch_add(1, Ordering::Relaxed)));
        ProfileDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ProfileDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A profile directory built in a tempdir, so the read is exercised
/// against real symlinks and real mtimes on any machine.
fn fixture(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    for id in [3u64, 4, 5] {
        let target = dir.join(format!("store-{id}"));
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, dir.join(format!("system-{id}-link"))).unwrap();
    }
    std::os::unix::fs::symlink(dir.join("system-5-link"), dir.join("system")).unwrap();
}

#[test]
fn generations_ascend_by_id_and_the_profile_link_is_not_one_of_them() {
    let tmp = ProfileDir::new("asc");
    fixture(tmp.path());

    let profile = read_profile(ProfileKind::System, tmp.path(), "system", None).unwrap().unwrap();

    assert_eq!(profile.generations.iter().map(|g| g.id).collect::<Vec<_>>(), vec![3, 4, 5]);
}

#[test]
fn the_generation_the_profile_points_at_is_current() {
    let tmp = ProfileDir::new("cur");
    fixture(tmp.path());

    let profile = read_profile(ProfileKind::System, tmp.path(), "system", None).unwrap().unwrap();

    let current: Vec<u64> = profile.generations.iter().filter(|g| g.current).map(|g| g.id).collect();
    assert_eq!(current, vec![5]);
}

/// A booted path that matches an older generation marks that one, and
/// marks nothing when it matches none - a host booted from a generation
/// since deleted is a real state and must not panic.
#[test]
fn the_booted_store_path_marks_its_generation_when_one_matches() {
    let tmp = ProfileDir::new("boot");
    fixture(tmp.path());
    let booted = tmp.path().join("store-3").to_string_lossy().to_string();

    let profile = read_profile(ProfileKind::System, tmp.path(), "system", Some(&booted)).unwrap().unwrap();
    let marked: Vec<u64> = profile.generations.iter().filter(|g| g.booted).map(|g| g.id).collect();
    assert_eq!(marked, vec![3]);

    let gone = read_profile(ProfileKind::System, tmp.path(), "system", Some("/nix/store/deleted")).unwrap().unwrap();
    assert!(gone.generations.iter().all(|g| !g.booted));
}

/// The profile symlink, not the directory it sits in - and this is the
/// one field of `Profile` that becomes a command-line argument, so the
/// distinction is a running command rather than a rendered string.
/// `nix-env -p /nix/var/nix/profiles` would create a *new* profile named
/// `profiles` beside the real ones, and `ops::activation_argv` would join
/// `bin/switch-to-configuration` onto a directory that has no `bin` -
/// measured on this host, where `/nix/var/nix/profiles/system/bin` holds
/// exactly that one file and `/nix/var/nix/profiles/bin` does not exist.
///
/// Nothing crossed this seam before: the app's fake handed back the
/// symlink form and the adapter produced the directory form, and the two
/// suites each agreed with themselves.
#[test]
fn a_profiles_path_is_the_symlink_nix_env_takes_not_its_directory() {
    let tmp = ProfileDir::new("path");
    fixture(tmp.path());

    let profile = read_profile(ProfileKind::System, tmp.path(), "system", None).unwrap().unwrap();

    assert_eq!(profile.path, tmp.path().join("system").to_string_lossy().to_string());
}

/// Whether the profile directory can be written, which is what decides
/// whether masys offers `a` and `c` at all. The direction is the whole
/// content of the check, and getting it inverted would dim every key on
/// a host where they work - or, worse, offer `c` on one where it would
/// delete half of what it found and fail.
///
/// The read-only half is asserted only for a non-root run: `faccessat`
/// answers yes for uid 0 whatever the mode bits say, which is not the
/// check being wrong, it is root genuinely being able to write the
/// directory. The writable half is asserted either way, so this is never
/// a test that quietly does nothing.
#[test]
fn a_profile_directory_reads_as_writable_only_where_it_can_be_written() {
    let tmp = ProfileDir::new("perm");
    fixture(tmp.path());

    let profile = read_profile(ProfileKind::System, tmp.path(), "system", None).unwrap().unwrap();
    assert_eq!(profile.writable, Some(true), "a directory this process just created");

    let mut mode = fs::metadata(tmp.path()).unwrap().permissions();
    mode.set_mode(0o555);
    fs::set_permissions(tmp.path(), mode).unwrap();
    let refused = read_profile(ProfileKind::System, tmp.path(), "system", None).unwrap().unwrap();
    // SAFETY: `geteuid` reads process state and takes no arguments.
    let root = unsafe { libc::geteuid() } == 0;
    if !root {
        assert_eq!(refused.writable, Some(false), "0o555 with the generations still readable");
    }

    // Restored so `Drop`'s `remove_dir_all` can unlink what is inside.
    let mut mode = fs::metadata(tmp.path()).unwrap().permissions();
    mode.set_mode(0o755);
    fs::set_permissions(tmp.path(), mode).unwrap();
}

/// Absent is not empty. A host with no home-manager must not render a
/// section claiming home-manager has no generations.
#[test]
fn a_missing_profile_directory_reads_as_absent() {
    let missing = ProfileDir::new("nope");
    assert_eq!(read_profile(ProfileKind::Home, missing.path(), "profile", None).unwrap(), None);
}

/// A generation whose link is dangling - its target removed by a GC run
/// mid-read - reports what it actually knows (nothing) rather than an
/// invented store path, and is never mistaken for the current generation.
#[test]
fn a_dangling_generation_link_has_no_store_path_and_is_not_current() {
    let tmp = ProfileDir::new("dangle-mid");
    fixture(tmp.path());
    fs::remove_dir_all(tmp.path().join("store-4")).unwrap();

    let profile = read_profile(ProfileKind::System, tmp.path(), "system", None).unwrap().unwrap();

    let four = profile.generations.iter().find(|g| g.id == 4).unwrap();
    assert_eq!(four.store_path, None);
    assert!(!four.current);

    // The unrelated healthy generation the profile actually points at is
    // unaffected by a sibling's dangling link.
    let five = profile.generations.iter().find(|g| g.id == 5).unwrap();
    assert!(five.current);
}

/// The Critical bug this test was written to catch: when the generation
/// the profile pointer resolves to is *also* dangling, both the profile
/// pointer's resolution and that generation's own resolution fail, and a
/// naive `==` between the two `None`s manufactures a match from nothing.
/// No generation may be marked current when the profile itself cannot
/// say which one is.
#[test]
fn a_dangling_current_generation_and_unresolvable_profile_pointer_mark_nothing_current() {
    let tmp = ProfileDir::new("dangle-current");
    fixture(tmp.path());
    // store-5 is both generation 5's own target and, via system-5-link,
    // what the profile pointer resolves to - removing it dangles both at
    // once, which is exactly the combination the old `==` fabricated a
    // match from.
    fs::remove_dir_all(tmp.path().join("store-5")).unwrap();

    let profile = read_profile(ProfileKind::System, tmp.path(), "system", None).unwrap().unwrap();

    assert!(profile.generations.iter().all(|g| !g.current));
}

/// Two generations that both failed to resolve must not be confused with
/// each other, or with a real booted path, through a shared fallback
/// value. Dangling generations never count as booted, even when a real
/// booted store path is supplied and matches a healthy sibling.
#[test]
fn dangling_generations_are_never_marked_booted_even_with_a_real_booted_path() {
    let tmp = ProfileDir::new("dangle-booted");
    fixture(tmp.path());
    fs::remove_dir_all(tmp.path().join("store-3")).unwrap();
    fs::remove_dir_all(tmp.path().join("store-4")).unwrap();
    let booted = tmp.path().join("store-5").to_string_lossy().to_string();

    let profile = read_profile(ProfileKind::System, tmp.path(), "system", Some(&booted)).unwrap().unwrap();

    let three = profile.generations.iter().find(|g| g.id == 3).unwrap();
    let four = profile.generations.iter().find(|g| g.id == 4).unwrap();
    let five = profile.generations.iter().find(|g| g.id == 5).unwrap();
    assert!(!three.booted);
    assert!(!four.booted);
    assert!(five.booted);
}
