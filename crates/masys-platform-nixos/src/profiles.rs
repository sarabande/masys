//! Reading a profile directory into generations.
//!
//! One `readdir` and a `readlink` per entry. No subprocess: see
//! `masys_domain::declarative::Generation::created_ms` for why neither
//! `nixos-rebuild list-generations` nor `nh os info` is parsed.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use masys_domain::declarative::{Generation, Profile, ProfileKind};
use masys_domain::error::MasysError;

use crate::links::{generation_id, version_label};

/// Every generation in `dir` whose name matches `prefix`, ascending by id.
///
/// `Ok(None)` when the directory does not exist, which is how a host with
/// no home-manager and no channels is reported. `booted` is the resolved
/// `/run/booted-system`, or `None` for a profile where the concept does
/// not apply.
pub fn read_profile(kind: ProfileKind, dir: &Path, prefix: &str, booted: Option<&str>) -> Result<Option<Profile>, MasysError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MasysError::Io(error)),
    };

    // What the profile symlink itself resolves to. Canonicalised so it
    // compares equal to the generations' own resolved targets - the
    // profile link points at a generation link, which points at the store,
    // and comparing one hop against two would find nothing current.
    let current_target = std::fs::canonicalize(dir.join(prefix)).ok();

    let mut generations = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = generation_id(name, prefix) else { continue };

        let link = entry.path();
        let resolved = std::fs::canonicalize(&link).ok();
        // `None`, not `""`, when the link is dangling: see
        // `masys_domain::declarative::Generation::store_path` for why an
        // invented empty string is worse than admitting the read failed.
        let store_path = resolved.as_ref().map(|p| p.to_string_lossy().to_string());

        // `symlink_metadata`, not `metadata`: the mtime wanted is the
        // link's own - when this generation was created - not the store
        // path's, which is shared between generations with identical
        // closures and would date several of them the same.
        let created_ms = std::fs::symlink_metadata(&link)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_millis() as u64);

        let kernel = std::fs::read_link(link.join("kernel")).ok().map(|p| p.to_string_lossy().to_string());

        generations.push(Generation {
            id,
            created_ms,
            label: store_path.as_deref().and_then(version_label),
            // Both sides must be a known, resolved path before they count
            // as a match - `resolved` is `None` when this generation's own
            // link is dangling, and `current_target` is `None` when the
            // profile pointer itself could not be resolved. `.zip` refuses
            // to pair a `None` with anything, so two failed reads can
            // never fabricate a match the way a bare `==` would.
            current: resolved.as_ref().zip(current_target.as_ref()).is_some_and(|(a, b)| a == b),
            // Both sides must be a real, resolved store path before they
            // can be said to match - `store_path` is `None` for a
            // dangling generation, and `.zip` refuses to pair a `None`
            // with anything, so an unknown never counts as booted.
            booted: store_path.as_deref().zip(booted).is_some_and(|(path, booted)| path == booted),
            store_path,
            kernel,
        });
    }

    generations.sort_by_key(|generation| generation.id);
    // `dir.join(prefix)`, not `dir`. `Profile::path` is what `nix-env -p`
    // is handed and what `ops::activation_argv` joins
    // `bin/switch-to-configuration` onto, and neither takes the directory:
    // `nix-env -p /nix/var/nix/profiles` would create a *new* profile
    // named `profiles` beside the real ones, and
    // `/nix/var/nix/profiles/bin/switch-to-configuration` does not exist -
    // measured here, where `/nix/var/nix/profiles/system/bin` holds
    // exactly that one file and `/nix/var/nix/profiles/bin` is absent.
    // `prefix` is already the symlink's name for all three kinds:
    // `system`, `profile` and `channels`.
    Ok(Some(Profile { kind, path: dir.join(prefix).to_string_lossy().to_string(), writable: writable(dir), generations }))
}

/// Whether this process could write `dir`: the check `nix-env
/// --switch-generation` and `nix-collect-garbage --delete-older-than`
/// will each make for real, made once here so masys can mark a key
/// instead of starting a command that cannot finish.
///
/// The directory rather than the profile symlink, because that is what
/// both of them write: one replaces a link in it, the other unlinks
/// generations from it.
///
/// `faccessat` with `AT_EACCESS` rather than `access(2)`, which asks
/// about the *real* uid. The two agree for masys today and the effective
/// ids are what the kernel will check when the write happens, so the
/// question worth asking is the one the write will face.
///
/// Not a probe write. Creating and removing a file in
/// `/nix/var/nix/profiles` to find out would leave masys's temporary
/// behind if the process died between the two, and would need write
/// access to answer whether it has write access on a host where it does.
/// Nor a mode-and-owner comparison: `PermissionsExt::readonly()` reads
/// `drwxr-xr-x root root` as writable, because the owner's write bit is
/// set - measured on this host, where that is exactly
/// `/nix/var/nix/profiles` and exactly the case this exists to catch.
/// Reimplementing the check by hand also gets ACLs and capabilities
/// wrong; the kernel already knows the answer.
///
/// `None` where the call failed for a reason that is neither yes nor no.
/// `EACCES` and `EROFS` are answers - the bits say no, or the filesystem
/// is mounted read-only - and everything else is the check failing, which
/// must not be reported as either permission or its absence.
fn writable(dir: &Path) -> Option<bool> {
    // Unreachable: a path cannot contain an interior NUL, since NUL is the
    // one byte a path component may not hold. `None` rather than an
    // `unwrap` because the cost of being wrong is masys aborting.
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `path` is a valid NUL-terminated C string that outlives the
    // call, and `faccessat` reads it and writes nothing.
    if unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::W_OK, libc::AT_EACCESS) } == 0 {
        return Some(true);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EACCES | libc::EROFS) => Some(false),
        _ => None,
    }
}
