//! The null adapters and the selection between them, asserted where they
//! now live.
//!
//! Carried over from `masys-platform-fallback/tests/fallback.rs` when the
//! adapter stopped being a crate. Reached through `#[path]` rather than
//! `tests/`, the same way `keymap_tests.rs` is, because `mod platform` is
//! private - an integration test would force it public and widen the
//! composition root's interface for nothing.

use masys_domain::platform::{Ownership, PlatformId};
use masys_domain::service::PlatformService;

use super::{FallbackPlatform, select_platform};

/// The whole point of this adapter: an unrecognised distro is a normal,
/// working state, not an error. Every method answers rather than failing,
/// so the app never has to special-case "no platform adapter".
#[test]
fn every_method_answers_rather_than_erroring() {
    let platform = FallbackPlatform {
        distro_was_read: true,
    };
    assert_eq!(platform.id(), PlatformId::Unsupported);
    assert_eq!(
        platform.pending_reboot().expect("pending_reboot answers"),
        None
    );
    assert_eq!(
        platform.boot_pressure().expect("boot_pressure answers"),
        None
    );
}

/// `Imperative` rather than `Declarative`: on a distro nothing recognises,
/// `systemctl enable` is overwhelmingly likely to persist, and claiming a
/// runtime change will be reverted when nothing is going to revert it
/// would put a false warning in front of the operator.
#[test]
fn unit_ownership_is_imperative_on_an_unknown_distro() {
    assert_eq!(
        FallbackPlatform {
            distro_was_read: true,
        }
        .unit_ownership("restic-backup.service")
        .expect("ownership answers"),
        Ownership::Imperative
    );
}

/// **But an unreadable `/etc/os-release` is not an unrecognised distro.**
///
/// The reasoning above rests on the host being one of the distros that
/// behave that way. Where the file could not be read the host may be
/// NixOS, whose unit files live in the read-only store - so `Imperative`
/// there promises a `systemctl enable` will stick when the next rebuild
/// will undo it. That is the harm `select_platform`'s own comment names.
///
/// An `Err` marks the rows, which is what the ownership guard already
/// does with a read it could not take.
#[test]
fn unit_ownership_declines_when_the_distro_could_not_be_read() {
    assert!(
        FallbackPlatform {
            distro_was_read: false,
        }
        .unit_ownership("restic-backup.service")
        .is_err(),
        "not knowing the distro is not knowing whether a change persists"
    );
}

/// Which adapter a host gets, which was untested while there was only one
/// real one to get.
///
/// Asserted through `id()` because that is the only thing the three
/// adapters differ in that is visible through the port - and it is why
/// `id` survived the trim that took `packages` and `updates` (#16). The
/// method has no production caller; it has this.
#[test]
fn each_host_gets_the_adapter_that_can_answer_for_it() {
    for (os_release, expected) in [
        ("ID=nixos\n", PlatformId::NixOs),
        ("ID=debian\nVERSION_ID=\"12\"\n", PlatformId::Debian),
        // A derivative, which the Debian adapter claims through `ID_LIKE`
        // because every fact it knows is inherited.
        ("ID=ubuntu\nID_LIKE=debian\n", PlatformId::Debian),
        ("ID=arch\n", PlatformId::Unsupported),
        // An *empty* `/etc/os-release` is a normal working state: the
        // file was read and names no distro masys knows. Unreadable is a
        // different fact and is asserted separately below - this comment
        // called the two one thing until 2026-08-30, which is how the
        // collapse survived.
        ("", PlatformId::Unsupported),
    ] {
        assert_eq!(
            select_platform(Some(os_release)).id(),
            expected,
            "for {os_release:?}"
        );
    }

    // Unreadable. Same `id`, because `PlatformId` has no word for it -
    // the difference shows up in `unit_ownership`, above.
    assert_eq!(select_platform(None).id(), PlatformId::Unsupported);
}

/// NixOS is tested before the Debian family, and a host that somehow
/// claimed both must come back NixOS: the specific test wins over the
/// family test, which is the rule that stays right when a third adapter
/// arrives.
#[test]
fn the_specific_test_wins_over_the_family_test() {
    assert_eq!(
        select_platform(Some("ID=nixos\nID_LIKE=debian\n")).id(),
        PlatformId::NixOs
    );
}
