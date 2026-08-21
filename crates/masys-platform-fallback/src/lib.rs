//! The `PlatformService` for a distro nothing recognises.
//!
//! This crate exists for a design reason rather than a practical one: a
//! second implementation, however trivial, is what forces `PlatformService`
//! to model *not being able to answer*. Without it the port would drift
//! toward NixOS's shape - a method returning `generation: u32` looks
//! reasonable until you ask what Debian puts there - and the distro
//! specifics would leak into the port's signatures.
//!
//! Every method answers. An unrecognised distro is a normal, working state:
//! the Packages buffer is simply absent and everything else works, per the
//! design's error table. Nothing here returns `Err`, because "I don't know
//! what distro this is" is not a failure the operator can act on.

use masys_domain::error::MasysError;
use masys_domain::platform::{BootPressure, Ownership, Package, PendingReboot, PlatformId, UpdateStatus};
use masys_domain::service::PlatformService;

/// Stateless: there is nothing to discover and nothing to cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FallbackPlatform;

impl PlatformService for FallbackPlatform {
    fn id(&self) -> PlatformId {
        PlatformId::Unsupported
    }

    /// Empty rather than an error: the Packages buffer is out of v1 scope
    /// anyway, and a distro with no package adapter genuinely has no
    /// packages *this tool can enumerate*, which is what an empty list
    /// says.
    fn packages(&self) -> Result<Vec<Package>, MasysError> {
        Ok(Vec::new())
    }

    fn updates(&self) -> Result<UpdateStatus, MasysError> {
        Ok(UpdateStatus::default())
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
    fn unit_ownership(&self, _unit: &str) -> Result<Ownership, MasysError> {
        Ok(Ownership::Imperative)
    }

    /// Why `/boot` is full is a generation-and-kernel-package question,
    /// and this adapter knows neither. The Disk section still reports the
    /// capacity itself - that comes from `SystemService` - just without
    /// the explanation.
    fn boot_pressure(&self) -> Result<Option<BootPressure>, MasysError> {
        Ok(None)
    }
}
