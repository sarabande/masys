#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformId {
    NixOs,
    /// No specific adapter recognised the running distro - the fallback
    /// adapter answers everything else on `PlatformService` with
    /// "unsupported" rather than guessing.
    Unsupported,
}

/// Whether a runtime change to a unit's enabled state survives the next
/// declarative rebuild. `PlatformService`'s single most load-bearing type:
/// it is what stops NixOS specifics (generation numbers, store paths) from
/// leaking into the port's method signatures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ownership {
    /// Debian, Arch: `systemctl enable`/`disable` persists on its own -
    /// nothing else owns the unit file.
    Imperative,
    /// NixOS: the unit is generated from configuration, so a runtime
    /// enable/disable/mask reverts the next time that source is applied.
    /// `source` names it (e.g. `services.restic.enable`), `note` is
    /// adapter-supplied detail for the popup, `reverted_by` names the
    /// action that undoes the runtime change (e.g. `nixos-rebuild
    /// switch`).
    Declarative { source: String, note: String, reverted_by: String },
}

/// A reboot the platform knows is pending, and why.
///
/// - NixOS: the running kernel/initrd isn't the current generation's -
///   `reason` names the generation gap.
/// - Debian: `/var/run/reboot-required` exists after a kernel/glibc
///   upgrade; `reason` is that file's own explanatory text.
/// - Arch: no first-class signal - a running-kernel-vs-installed-package
///   version mismatch is the closest equivalent, and it's heuristic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReboot {
    pub reason: String,
}

/// Why `/boot` is full.
///
/// - NixOS: `generations` is every generation with a boot entry;
///   `reclaimable_bytes` is what garbage-collecting old ones would free.
/// - Debian: old `linux-image-*` packages left installed after upgrades;
///   `apt autoremove` reclaims the space. Not generation-counted, so a
///   real adapter would report `generations: 0`.
/// - Arch: `/boot` is not generation-based; growth here is unusual and
///   usually means a stale `mkinitcpio` fallback image, not steady state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootPressure {
    pub generations: u32,
    pub reclaimable_bytes: u64,
}

/// One installed package. The Packages buffer (this type's only consumer)
/// is deferred past v1, but the method is part of the port now so both
/// adapters implement the full contract from day one, per
/// `masys-design.md`'s "Ports" section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UpdateStatus {
    pub available: u32,
    pub security: u32,
}
