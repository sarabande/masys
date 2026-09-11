#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformId {
    NixOs,
    /// Debian and everything descended from it - Ubuntu, Mint, Pop!\_OS -
    /// which share every fact the adapter knows: `/var/run/reboot-required`,
    /// `systemctl enable` persisting, and kernels accumulating in `/boot`.
    Debian,
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
    Declarative {
        source: String,
        note: String,
        reverted_by: String,
    },
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

/// One package installed on this host.
///
/// Removed from `PlatformService` on 2026-08-29 because nothing drew it
/// (#16), on the note that "the Packages buffer can bring its own port
/// when it exists". It exists, and this is that port's type - reached
/// through [`crate::service::PackageService`] rather than back on
/// `PlatformService`, so a host that cannot list packages is missing an
/// adapter rather than answering an empty list.
///
/// `name` and `version` only. What a package *is* differs enough between
/// distributions that anything richer would be one distro's vocabulary:
/// dpkg has sections, priorities and an install state; nix has a store
/// hash and no install state at all, because presence in the system
/// closure is the whole of it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Package {
    pub name: String,
    pub version: String,
}

/// Why `/boot` is full.
///
/// - NixOS: `generations` is every generation with a boot entry;
///   `reclaimable_bytes` is what garbage-collecting old ones would free.
/// - Debian: old `linux-image-*` packages left installed after upgrades;
///   `apt autoremove` reclaims the space. Not generation-based, so
///   `generations` is `None`.
/// - Arch: `/boot` is not generation-based either; growth here is unusual
///   and usually means a stale `mkinitcpio` fallback image, not steady
///   state.
///
/// **`generations` is an `Option`, and the second adapter is why.** It was
/// a `u32`, and this doc used to tell a Debian adapter to "report
/// `generations: 0`" - which would have rendered, because
/// `masys_render::view` appends `. {n} generations` to the finding
/// whenever the value is present. A Debian host would have read `0
/// generations`: a count of a NixOS concept it does not have, and the same
/// mistake the Nix buffer's own counts are written to avoid, where `0` is
/// a finding ("there is nothing to roll back to") rather than an absence.
///
/// One adapter could not surface that. NixOS's own `boot_pressure` answers
/// `Ok(None)` - the count needs the bootloader's layout, which is not
/// implemented - so nothing ever populated the field, and a shape that
/// only a differently-answering distro could object to had nothing to
/// object to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootPressure {
    /// `None` on a host whose `/boot` is not generation-based, which is
    /// every host but NixOS. Not `0`: no generations and zero generations
    /// are different claims, and only the second is a finding.
    pub generations: Option<u32>,
    /// `None` on a host that cannot find out. NixOS is one: the ESP is
    /// mounted `umask=0077` by default, so an unprivileged masys cannot
    /// read `/boot` at all, and the store sizes it *can* read are what a
    /// generation holds rather than what its boot entry occupies. Not
    /// `0`, which would claim there is nothing to reclaim.
    pub reclaimable_bytes: Option<u64>,
}
