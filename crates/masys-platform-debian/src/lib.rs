//! Debian platform facts: pending reboot, unit ownership, and what is
//! sitting in `/boot`.
//!
//! The second adapter, and it exists to be a second one. `PlatformService`
//! was designed against NixOS and, until this crate, only NixOS answered
//! it - which meant the port's claim to model *a host, generally* was
//! untested. One adapter proves a port can be satisfied; it cannot prove
//! the port asks the right questions, because there is nothing for the
//! answers to disagree with.
//!
//! It found one thing immediately, recorded on the fields that changed:
//! `BootPressure::generations` was a `u32`, and a distro whose `/boot` is
//! not generation-based has no honest number to put in it. See
//! [`boot_pressure_from`].
//!
//! **Every fact here is a pure function over its input**, with the `impl`
//! below doing the reading and delegating. That is `masys-platform-nixos`'s
//! shape too (`is_nixos` takes the text of `/etc/os-release`), and here it
//! is load-bearing rather than tidy: masys is developed on NixOS, so an
//! adapter whose logic could only be exercised on Debian would be an
//! adapter nothing ever ran.

use std::path::Path;

use masys_domain::error::MasysError;
use masys_domain::platform::{BootPressure, Ownership, Package, PendingReboot, PlatformId};
use masys_domain::service::{PackageService, PlatformService};

/// Where Debian's `update-notifier` leaves word that a reboot is needed.
const REBOOT_REQUIRED: &str = "/var/run/reboot-required";

/// And which packages asked for it. Appended to by maintainer scripts, so
/// it may hold several lines or not exist at all.
const REBOOT_REQUIRED_PKGS: &str = "/var/run/reboot-required.pkgs";

/// Where the kernel images live.
const BOOT: &str = "/boot";

/// dpkg's own record of what is installed.
const DPKG_STATUS: &str = "/var/lib/dpkg/status";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DebianPlatform;

/// Whether this host is Debian or descended from it, from
/// `/etc/os-release`.
///
/// `ID` *or* `ID_LIKE`, unlike masys-platform-nixos's `is_nixos` and its
/// single field, because every fact this adapter knows is inherited: Ubuntu, Mint
/// and Pop!\_OS all carry `/var/run/reboot-required`, all let `systemctl
/// enable` persist, and all accumulate kernels in `/boot`. Matching `ID`
/// alone would send them to the fallback, which answers none of it.
///
/// `ID_LIKE` is a space-separated list, so it is split rather than
/// compared whole - `ID_LIKE="ubuntu debian"` is Mint's, and a substring
/// test would also accept a hypothetical `notdebian`.
///
/// Named rather than linked, and it has to be: the two adapters do not
/// depend on each other and must not - `layering.rs` forbids either
/// reaching through the other - so there is no path for rustdoc to
/// resolve and an intra-doc link here can only ever dangle.
pub fn is_debian(os_release: &str) -> bool {
    let field = |key: &str| {
        os_release
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .map(|value| value.trim().trim_matches('"'))
    };
    if field("ID=") == Some("debian") {
        return true;
    }
    field("ID_LIKE=")
        .map(|value| value.split_whitespace().any(|word| word == "debian"))
        .unwrap_or(false)
}

/// The reboot verdict, given the contents of `reboot-required.pkgs` where
/// the marker file exists.
///
/// `None` for "no marker", which is Debian saying no reboot is pending -
/// an answer, not a failed read. The port's `Option` is what carries that
/// distinction and it is why this returns one.
///
/// The reason names the packages because that is the decision the operator
/// is making: `linux-image-*` and `libc6` produce the same marker file and
/// very different urgencies. Where the list is missing or empty the marker
/// still stands - its existence is the signal, the list is detail, and
/// reporting "no reboot" for want of the detail would invert the answer.
pub fn pending_from(pkgs: Option<&str>) -> Option<PendingReboot> {
    let pkgs = pkgs?;
    let named: Vec<&str> = pkgs
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    Some(PendingReboot {
        reason: match named.is_empty() {
            true => "a package upgrade needs a reboot".to_string(),
            false => format!("{} upgraded", named.join(", ")),
        },
    })
}

/// One kernel image in `/boot`, identified by the release it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelImage {
    /// `6.1.0-18-amd64` - what `uname -r` prints for the running one.
    pub release: String,
    /// Every file belonging to this release, summed: `vmlinuz`,
    /// `initrd.img`, `System.map` and `config`. The initrd is usually the
    /// large one.
    pub bytes: u64,
}

/// What `/boot` is holding that could be freed, given its kernel images
/// and the running release.
///
/// **`generations` is `None`, and that is this crate's one contribution to
/// the port's shape.** The field was a `u32` when only NixOS answered, and
/// `masys_domain::platform::BootPressure`'s own doc told a future Debian
/// adapter to "report `generations: 0`". That would have rendered - the
/// finding's line appends `. {n} generations` whenever the value is
/// present - as `0 generations` on a host that has no such concept, which
/// is the precise mistake `masys_render::view` argues against for the Nix
/// buffer's own counts: a zero that reads as a finding rather than as an
/// absence.
///
/// Debian's `/boot` is not generation-based. It holds kernels, and old
/// ones are reclaimed by `apt autoremove` rather than by rolling anything
/// back. So the honest answer to "how many generations" is *not a number*.
///
/// `None` overall - rather than a `BootPressure` with nothing in it - when
/// only the running kernel is present, which is the healthy case and the
/// common one immediately after an upgrade cleanup.
/// `running` is `None` where the running release could not be read. That
/// is not a release no image matches - it is not knowing which image is
/// in use, and summing every kernel then counts the *running* one as
/// reclaimable. The honest answer is no figure at all, which is what the
/// port's `Option` is for.
pub fn boot_pressure_from(kernels: &[KernelImage], running: Option<&str>) -> Option<BootPressure> {
    let running = running?;
    let reclaimable: u64 = kernels
        .iter()
        .filter(|image| image.release != running)
        .map(|image| image.bytes)
        .sum();
    match reclaimable {
        0 => None,
        bytes => Some(BootPressure {
            generations: None,
            // Debian answers the half NixOS cannot: `/boot` there holds
            // the kernel images themselves and is world-readable, so the
            // figure is measured rather than inferred from the store.
            reclaimable_bytes: Some(bytes),
        }),
    }
}

/// The release a `/boot` filename belongs to, or `None` for a file that
/// is not part of a kernel image.
///
/// Four prefixes because Debian installs four files per kernel and all
/// four are freed together. `vmlinuz` alone would undercount by roughly
/// the initrd, which is the largest of them.
fn release_of(name: &str) -> Option<&str> {
    ["vmlinuz-", "initrd.img-", "System.map-", "config-"]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
}

/// Every kernel image in `dir`, summed per release.
///
/// An unreadable `/boot` yields an empty list rather than an error: on a
/// host where `/boot` is a separate unmounted partition, or not readable
/// by this user, "I cannot see any kernels" is not a finding about disk
/// pressure and must not become one.
fn kernels_in(dir: &Path) -> Vec<KernelImage> {
    let mut by_release: Vec<KernelImage> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return by_release;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(release) = release_of(name) else {
            continue;
        };
        let bytes = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        match by_release.iter_mut().find(|image| image.release == release) {
            Some(image) => image.bytes += bytes,
            None => by_release.push(KernelImage {
                release: release.to_string(),
                bytes,
            }),
        }
    }
    by_release
}

/// The running kernel release, as `uname -r` prints it.
///
/// Read from `/proc/sys/kernel/osrelease` rather than by calling `uname`:
/// masys reads `/proc` for everything else it knows about the running
/// system, and a subprocess for one string would be the only one in this
/// crate.
/// `None` where it could not be read, which `boot_pressure_from` turns
/// into no figure rather than into a release that matches nothing.
fn running_release() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|release| !release.is_empty())
}

impl PlatformService for DebianPlatform {
    fn id(&self) -> PlatformId {
        PlatformId::Debian
    }

    fn pending_reboot(&self) -> Result<Option<PendingReboot>, MasysError> {
        if !Path::new(REBOOT_REQUIRED).exists() {
            return Ok(None);
        }
        // The marker exists; the package list is optional detail. An
        // unreadable `.pkgs` reads as an empty one rather than as no
        // marker, so the verdict survives losing the explanation.
        let pkgs = std::fs::read_to_string(REBOOT_REQUIRED_PKGS).unwrap_or_default();
        Ok(pending_from(Some(&pkgs)))
    }

    /// Debian's answer, and it is the same for every unit.
    ///
    /// `systemctl enable` writes a symlink under `/etc/systemd/system` and
    /// nothing rewrites it: there is no manifest, so there is nothing for a
    /// runtime change to be reverted by. That is what `Ownership::Imperative`
    /// means and its doc has named Debian as the case since the type was
    /// written.
    ///
    /// A constant, which is worth stating plainly rather than dressing up.
    /// Package-shipped units live in `/lib/systemd/system` and admin ones
    /// in `/etc/systemd/system`, but that distinction does not change the
    /// answer to the question this port asks - enabling either one
    /// persists. Reading the path to return the same value twice would be
    /// a check that looks like a decision.
    fn unit_ownership(&self, _unit: &str) -> Result<Ownership, MasysError> {
        Ok(Ownership::Imperative)
    }

    fn boot_pressure(&self) -> Result<Option<BootPressure>, MasysError> {
        Ok(boot_pressure_from(
            &kernels_in(Path::new(BOOT)),
            running_release().as_deref(),
        ))
    }
}

/// Every installed package in a dpkg status file, ascending by name.
///
/// The file is paragraphs separated by blank lines, each a set of
/// `Field: value` lines. Read directly rather than through `dpkg-query`,
/// which would be the only subprocess in this crate for a file that is
/// right there and plain text.
///
/// **Only `installed`.** A package purged but for its configuration keeps
/// its paragraph, with `Status: deinstall ok config-files` and a version
/// still on it. Counting those reports software the host cannot run, which
/// is the dpkg-shaped version of the empty list this port exists not to
/// invent. The third word of `Status` is the install state; the first two
/// are what was *wanted* and whether dpkg is mid-operation, and neither
/// answers "is it here".
///
/// A paragraph missing either field is skipped rather than half-reported.
pub fn packages_from_status(text: &str) -> Vec<Package> {
    let mut packages: Vec<Package> = text
        .split("\n\n")
        .filter_map(|paragraph| {
            let field = |key: &str| {
                paragraph
                    .lines()
                    .find_map(|line| line.strip_prefix(key))
                    .map(str::trim)
            };
            let installed = field("Status:")
                .and_then(|status| status.split_whitespace().nth(2))
                .is_some_and(|state| state == "installed");
            match installed {
                true => Some(Package {
                    name: field("Package:")?.to_string(),
                    version: field("Version:")?.to_string(),
                }),
                false => None,
            }
        })
        .collect();
    packages.sort();
    packages
}

impl PackageService for DebianPlatform {
    fn packages(&self) -> Result<Vec<Package>, MasysError> {
        let text = std::fs::read_to_string(DPKG_STATUS).map_err(MasysError::Io)?;
        Ok(packages_from_status(&text))
    }
}
