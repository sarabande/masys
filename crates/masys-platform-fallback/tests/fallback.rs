use masys_domain::platform::{Ownership, PlatformId};
use masys_domain::service::PlatformService;
use masys_platform_fallback::FallbackPlatform;

/// The whole point of this adapter: an unrecognised distro is a normal,
/// working state, not an error. Every method answers rather than failing,
/// so the app never has to special-case "no platform adapter".
#[test]
fn every_method_answers_rather_than_erroring() {
    let platform = FallbackPlatform;
    assert_eq!(platform.id(), PlatformId::Unsupported);
    assert!(platform.packages().expect("packages answers").is_empty());
    let updates = platform.updates().expect("updates answers");
    assert_eq!((updates.available, updates.security), (0, 0));
    assert_eq!(platform.pending_reboot().expect("pending_reboot answers"), None);
    assert_eq!(platform.boot_pressure().expect("boot_pressure answers"), None);
}

/// `Imperative` rather than `Declarative`: on a distro nothing recognises,
/// `systemctl enable` is overwhelmingly likely to persist, and claiming a
/// runtime change will be reverted when nothing is going to revert it
/// would put a false warning in front of the operator.
#[test]
fn unit_ownership_is_imperative_on_an_unknown_distro() {
    assert_eq!(FallbackPlatform.unit_ownership("restic-backup.service").expect("ownership answers"), Ownership::Imperative);
}
