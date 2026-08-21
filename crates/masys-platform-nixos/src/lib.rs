//! The `PlatformService` for NixOS.
//!
//! Minimal on purpose: it answers the one question that changes what
//! masys is allowed to say, which is whether a runtime change to a unit
//! survives. Generations, package listings and boot pressure are
//! deliberately still unanswered - the guard is what blocks the Units
//! buffer's enable/disable, and it is what this crate exists for today.

use masys_domain::error::MasysError;
use masys_domain::platform::{BootPressure, Ownership, Package, PendingReboot, PlatformId, UpdateStatus};
use masys_domain::service::PlatformService;

/// Where NixOS keeps everything it manages. A unit whose definition
/// resolves in here is generated from configuration.
const STORE: &str = "/nix/store";

const UNIT_DIR: &str = "/etc/systemd/system";

/// Whether `systemctl enable` could write its symlink at all.
///
/// Checked rather than assumed: NixOS hosts differ, and a host whose
/// `/etc/systemd/system` is a real writable directory will let the
/// enable succeed and then quietly undo it on the next rebuild - which
/// needs a different warning from one that refuses immediately.
fn wants_writable() -> bool {
    std::path::Path::new(UNIT_DIR).join("multi-user.target.wants").metadata().map(|meta| !meta.permissions().readonly()).unwrap_or(false)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NixosPlatform;

/// Whether this host is NixOS, from `/etc/os-release`'s `ID`.
///
/// The ID field rather than PRETTY_NAME: it is the stable machine-readable
/// one, and PRETTY_NAME carries a version and a codename that change.
pub fn is_nixos(os_release: &str) -> bool {
    os_release.lines().find_map(|line| line.strip_prefix("ID=")).map(|value| value.trim().trim_matches('"') == "nixos").unwrap_or(false)
}

/// Whether a unit's definition lives in the nix store, given the path
/// systemd reports for it.
///
/// Follows the link, because `/etc/systemd/system/sshd.service` is itself
/// a symlink into the store on this host - and so is
/// `/etc/systemd/system` itself. Reading the path systemd reports without
/// resolving it would conclude that every unit is hand-managed, which is
/// exactly backwards.
pub fn is_store_managed(fragment_path: &str) -> bool {
    if fragment_path.is_empty() {
        return false;
    }
    std::fs::canonicalize(fragment_path).map(|resolved| resolved.starts_with(STORE)).unwrap_or_else(|_| fragment_path.starts_with(STORE))
}

impl PlatformService for NixosPlatform {
    fn id(&self) -> PlatformId {
        PlatformId::NixOs
    }

    fn packages(&self) -> Result<Vec<Package>, MasysError> {
        // The Packages buffer is out of v1 scope; answering with an empty
        // list would claim this host has no packages.
        Err(MasysError::Platform("listing packages is not implemented for NixOS yet".to_string()))
    }

    fn updates(&self) -> Result<UpdateStatus, MasysError> {
        Err(MasysError::Platform("update checking is not implemented for NixOS yet".to_string()))
    }

    fn pending_reboot(&self) -> Result<Option<PendingReboot>, MasysError> {
        // Comparing /run/booted-system against /run/current-system is the
        // real check and is not implemented yet; claiming "no reboot
        // pending" would be a worse answer than staying quiet.
        Ok(None)
    }

    /// The guard.
    ///
    /// A unit generated from configuration is the manifest's to decide.
    /// How that fails depends on the host: usually the change is undone
    /// by the next `nixos-rebuild switch`, but where `/etc/systemd/system`
    /// resolves into the read-only store - as it does here - `systemctl
    /// enable` cannot create its symlink at all and is refused outright.
    /// The note says which, because "reverts later" and "refused now" are
    /// different things to plan around.
    ///
    /// Only enable and disable ask. Starting or stopping a unit writes
    /// nothing and contradicts no manifest: NixOS declares which units
    /// exist and which are enabled at boot, not what is running right
    /// now. An enabled unit coming back after a reboot is the manifest
    /// doing its job, not a conflict.
    ///
    /// A unit *not* in the store was placed by hand and behaves like any
    /// other distro's, so it is genuinely imperative.
    fn unit_ownership(&self, unit: &str) -> Result<Ownership, MasysError> {
        let fragment = format!("{UNIT_DIR}/{unit}");
        if !is_store_managed(&fragment) {
            return Ok(Ownership::Imperative);
        }
        Ok(Ownership::Declarative {
            source: "configuration.nix".to_string(),
            // The distinction matters to whoever is about to press the
            // key. "Reverts later" and "is refused now" are different
            // things to plan around, and on this host it is the second:
            // the directory `systemctl enable` must write its symlink
            // into resolves into the read-only store.
            note: if wants_writable() {
                "generated from configuration".to_string()
            } else {
                "refused now - the unit directory is read-only".to_string()
            },
            reverted_by: "nixos-rebuild switch".to_string(),
        })
    }

    fn boot_pressure(&self) -> Result<Option<BootPressure>, MasysError> {
        // Counting generations with boot entries needs the bootloader's
        // layout, which is not implemented yet.
        Ok(None)
    }
}
