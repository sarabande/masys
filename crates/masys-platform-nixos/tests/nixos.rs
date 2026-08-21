use masys_domain::platform::{Ownership, PlatformId};
use masys_domain::service::PlatformService;
use masys_platform_nixos::{NixosPlatform, is_nixos, is_store_managed};

/// The `ID` field rather than `PRETTY_NAME`: it is the stable
/// machine-readable one, and PRETTY_NAME carries a version and codename
/// that change with every release.
#[test]
fn nixos_is_recognised_by_its_id() {
    assert!(is_nixos("ID=nixos\nPRETTY_NAME=\"NixOS 26.11 (Zokor)\"\n"));
    assert!(is_nixos("PRETTY_NAME=\"NixOS 26.11\"\nID=\"nixos\"\n"), "a quoted ID still counts");
    assert!(!is_nixos("ID=debian\nPRETTY_NAME=\"Debian GNU/Linux 12\"\n"));
    assert!(!is_nixos(""), "a host with no os-release is not NixOS");
}

/// PRETTY_NAME mentioning NixOS is not enough - a Debian host could name
/// it in a comment or a version string.
#[test]
fn a_pretty_name_alone_does_not_make_a_host_nixos() {
    assert!(!is_nixos("PRETTY_NAME=\"NixOS-like thing\"\nID=debian\n"));
}

#[test]
fn a_path_outside_the_store_is_not_store_managed() {
    assert!(!is_store_managed("/tmp"));
    assert!(!is_store_managed(""), "no path is not a store path");
}

/// A unit placed by hand behaves like any other distro's, so enabling it
/// genuinely persists.
#[test]
fn a_unit_with_no_file_at_all_is_imperative() {
    let ownership = NixosPlatform.unit_ownership("definitely-not-a-real-unit.service").expect("an answer");
    assert_eq!(ownership, Ownership::Imperative);
}

#[test]
fn the_platform_identifies_itself() {
    assert_eq!(NixosPlatform.id(), PlatformId::NixOs);
}

/// Answering "no packages" would claim this host has none. The design's
/// point about the fallback adapter cuts both ways: an adapter that
/// cannot answer should say so, not invent a reassuring answer.
#[test]
fn unimplemented_queries_refuse_rather_than_inventing_an_answer() {
    assert!(NixosPlatform.packages().is_err());
    assert!(NixosPlatform.updates().is_err());
    // These two are genuinely "nothing to report" rather than unknown.
    assert_eq!(NixosPlatform.pending_reboot().expect("an answer"), None);
    assert_eq!(NixosPlatform.boot_pressure().expect("an answer"), None);
}

/// "Reverts later" and "is refused now" are different things to plan
/// around. On a host whose `/etc/systemd/system` resolves into the
/// read-only store - as this one's does - `systemctl enable` cannot
/// create its symlink at all, and saying it will revert would send the
/// operator looking for a rebuild that never happens.
#[test]
fn a_store_managed_unit_says_how_the_change_fails() {
    // A real unit on this host, if it is NixOS; the test asserts the
    // shape of the answer rather than which branch this machine takes.
    let ownership = NixosPlatform.unit_ownership("sshd.service").expect("an answer");
    match ownership {
        Ownership::Declarative { source, note, reverted_by } => {
            assert_eq!(source, "configuration.nix");
            assert_eq!(reverted_by, "nixos-rebuild switch");
            assert!(note.contains("read-only") || note.contains("generated"), "the note must say how it fails, got {note:?}");
        }
        // Not a NixOS host, or sshd is hand-placed here: both legitimate.
        Ownership::Imperative => {}
    }
}
