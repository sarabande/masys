//! Pure parsers over the names nix gives things.
//!
//! Separated from the reads because these carry all the parsing risk and
//! none of the I/O: the whole of this module is testable on any machine,
//! which is what keeps the adapter's suite from needing a NixOS host.

/// The generation number in a profile link name, e.g. `system-437-link`.
///
/// `None` for the profile symlink itself, which lives in the same
/// directory as its generations and would otherwise be read as one.
pub fn generation_id(file_name: &str, prefix: &str) -> Option<u64> {
    let rest = file_name.strip_prefix(prefix)?.strip_prefix('-')?;
    rest.strip_suffix("-link")?.parse().ok()
}

/// The version a system generation's store path names, e.g.
/// `26.11.20260804.e72e4f2` from
/// `…-nixos-system-devbox-26.11.20260804.e72e4f2`.
///
/// Split from the right on the marker rather than counting fields: a host
/// name may itself contain a hyphen, so the number of segments between
/// `nixos-system-` and the version is not fixed. The version is the last
/// segment and the host is whatever precedes it.
pub fn version_label(store_path: &str) -> Option<String> {
    let name = store_path.rsplit('/').next()?;
    let after = name.split_once("-nixos-system-")?.1;
    let version = after.rsplit_once('-')?.1;
    // A version starts with a digit. Without this a host called
    // `nixos-system-web-server` would report `server` as its version.
    version.chars().next().filter(char::is_ascii_digit)?;
    // A version also always carries a dot - `26.11.20260804.e72e4f2`,
    // never a bare integer. Without this, a host whose name itself ends
    // in digits (`devbox-2`, on a path that carries no real version)
    // comes back as `Some("2")`: a wrong version stated as confidently as
    // a right one.
    version.contains('.').then_some(())?;
    Some(version.to_string())
}
