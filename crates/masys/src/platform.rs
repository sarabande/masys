//! Which adapter this host gets, and what those adapters do when the
//! answer is nothing.
//!
//! Four things that belong together and were in three places: the two
//! selectors, and the two adapters that answer for a host no selector
//! could match. Both of the latter are *null adapters* - they satisfy a
//! port without reading anything - and the composition root is where they
//! belong, because choosing an adapter is the only thing this crate is
//! for.
//!
//! `FallbackPlatform` had a crate of its own until it was measured: 68
//! lines of constants behind a manifest, a test directory and four
//! entries in the layering tables, while `NoScanner` did the identical
//! job in ten lines here. Two answers to one question. This is the other
//! one.
//!
//! What the crate was said to buy was the port's honesty - "a second
//! implementation, however trivial, is what forces `PlatformService` to
//! model not being able to answer". That property is real and it is not
//! carried here. It is carried by the port's own return types:
//! `PlatformId::Unsupported`, `Ownership`, and the `Option` on
//! `pending_reboot` and `boot_pressure`. A port shaped around NixOS would
//! survive this adapter intact - `fn generation(&self) -> u32 { 0 }` is a
//! perfectly happy fallback - so the adapter never was what stopped the
//! leak. Keeping the check in the types is what makes it checkable.

use masys_domain::declarative::DeclarativeService;
use masys_domain::error::MasysError;
use masys_domain::platform::{BootPressure, Ownership, PendingReboot, PlatformId};
use masys_domain::scan::{DirScanner, ScanProgress};
use masys_domain::service::{PackageService, PlatformService};
use masys_platform_debian::{DebianPlatform, is_debian};
use masys_platform_nixos::{NixosPlatform, is_nixos};

/// The platform adapter for this host.
///
/// Given `/etc/os-release`'s text rather than reading it, so that the one
/// read in `run` settles every question the file answers. An unreadable or
/// unrecognised file gets the fallback, which answers every distro-specific
/// question with "not supported" - the design's point being that an unknown
/// distro is a normal working state rather than an error.
///
/// Ordered, and the order is not arbitrary even though the two predicates
/// are disjoint today: `is_nixos` reads `ID` alone while `is_debian` also
/// accepts `ID_LIKE`, so the family test goes second. A distro claiming
/// `ID_LIKE=debian` while being NixOS is not a thing, but "the specific
/// test before the family test" is the rule that stays right when a third
/// adapter arrives.
/// `None` means `/etc/os-release` could not be read, which is a different
/// state from a distro nothing recognises - see [`FallbackPlatform`].
pub fn select_platform(os_release: Option<&str>) -> Box<dyn PlatformService> {
    let Some(os_release) = os_release else {
        return Box::new(FallbackPlatform {
            distro_was_read: false,
        });
    };
    if is_nixos(os_release) {
        return Box::new(NixosPlatform);
    }
    if is_debian(os_release) {
        return Box::new(DebianPlatform);
    }
    Box::new(FallbackPlatform {
        distro_was_read: true,
    })
}

/// The package adapter for this host, if it has one.
///
/// `None` on a distro nothing recognises, and that absence is what removes
/// the Packages buffer rather than any check inside it - the same shape
/// `select_declarative` has, for the same reason. A fallback that answered
/// an empty list would put a buffer on screen claiming this host has no
/// software installed.
pub fn select_packages(os_release: &str) -> Option<Box<dyn PackageService>> {
    if is_nixos(os_release) {
        return Some(Box::new(NixosPlatform));
    }
    if is_debian(os_release) {
        return Some(Box::new(DebianPlatform));
    }
    None
}

/// The declarative adapter for this host, if it has one.
///
/// `None` is the ordinary answer everywhere but NixOS, and it is what
/// keeps the Nix buffer off a host that has no generations to show. Absent
/// rather than empty: an unreadable `/etc/os-release` reads as not-NixOS
/// here, which is the same answer a Debian box gives, and on a host that
/// really is NixOS the buffer going missing is a visible failure rather
/// than a screen of dashes pretending to be readings.
pub fn select_declarative(os_release: &str) -> Option<Box<dyn DeclarativeService>> {
    is_nixos(os_release).then(|| Box::new(NixosPlatform) as Box<dyn DeclarativeService>)
}

/// The `PlatformService` for a distro nothing recognises.
///
/// Every method answers. An unrecognised distro is a normal, working state:
/// the Packages buffer is simply absent and everything else works, per the
/// design's error table. Nothing here returns `Err`, because "I don't know
/// what distro this is" is not a failure the operator can act on.
///
/// Stateless: there is nothing to discover and nothing to cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackPlatform {
    /// Whether `/etc/os-release` was read at all.
    ///
    /// **`false` is not "unrecognised distro".** The composition root
    /// read that file with `unwrap_or_default()` until 2026-08-30, so an
    /// unreadable one arrived here as an empty string - indistinguishable
    /// from a distro with no adapter, and answered with the same
    /// confident `Ownership::Imperative`. On a NixOS host whose
    /// `/etc/os-release` cannot be read, that promises `systemctl enable`
    /// will stick when the next rebuild will undo it, which is the exact
    /// harm `select_platform`'s own comment says the guard exists to
    /// prevent.
    ///
    /// The two are separated here rather than upstream because this is
    /// the only place the difference changes an answer.
    distro_was_read: bool,
}

impl PlatformService for FallbackPlatform {
    fn id(&self) -> PlatformId {
        PlatformId::Unsupported
    }

    /// `None` means "no reboot is known to be pending", which is the
    /// honest answer: this adapter has no way to detect one, and claiming
    /// a reboot is needed would be worse than staying quiet.
    fn pending_reboot(&self) -> Result<Option<PendingReboot>, MasysError> {
        Ok(None)
    }

    /// `Imperative`, not `Declarative`. On a distro nothing recognises,
    /// `systemctl enable` is overwhelmingly likely to persist - Debian and
    /// Arch both behave that way, and they are the common case among
    /// distros with no adapter here. Claiming a runtime change will be
    /// reverted when nothing is going to revert it would put a false
    /// warning in front of the operator, which is worse than the reverse
    /// mistake: an unexpected revert is visible the next time they look,
    /// while a phantom warning trains them to ignore the guard entirely.
    /// All of which holds only where the distro was actually *looked at*.
    /// Where `/etc/os-release` could not be read, this host may be NixOS,
    /// and the reasoning above - "Debian and Arch both behave that way" -
    /// rests on a reading nobody took. An `Err` marks the rows instead,
    /// which is what the ownership guard already does with a failed read.
    fn unit_ownership(&self, _unit: &str) -> Result<Ownership, MasysError> {
        match self.distro_was_read {
            true => Ok(Ownership::Imperative),
            false => Err(MasysError::Platform(
                "/etc/os-release could not be read, so whether a unit change persists is unknown"
                    .to_string(),
            )),
        }
    }

    /// Why `/boot` is full is a generation-and-kernel-package question,
    /// and this adapter knows neither. The Disk section still reports the
    /// capacity itself - that comes from `SystemService` - just without
    /// the explanation.
    fn boot_pressure(&self) -> Result<Option<BootPressure>, MasysError> {
        Ok(None)
    }
}

/// The scanner a host gets when the thread pool will not build.
///
/// The other null adapter, and the reason this module exists rather than
/// two: a pool that will not build is not a reason to refuse to start.
/// Every other buffer still works and this one simply finds nothing.
pub struct NoScanner;

impl DirScanner for NoScanner {
    fn start(&self, _root: &std::path::Path) {}
    fn poll(&self) -> ScanProgress {
        ScanProgress::default()
    }
    fn cancel(&self) {}
}

#[cfg(test)]
#[path = "platform_tests.rs"]
mod platform_tests;
