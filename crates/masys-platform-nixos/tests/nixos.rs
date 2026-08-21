use masys_domain::declarative::{DeclarativeService, HomeMode};
use masys_domain::platform::{Ownership, PlatformId};
use masys_domain::service::PlatformService;
use masys_platform_nixos::{NixosPlatform, REASON_BUDGET, is_nixos, is_store_managed, reboot_reason};
use unicode_width::UnicodeWidthStr;

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
    // Genuinely "nothing to report" rather than unknown. `pending_reboot`
    // was asserted here too until it was implemented; it now compares
    // booted against current, and on a host between two generations the
    // honest answer is `Some`.
    assert_eq!(NixosPlatform.boot_pressure().expect("an answer"), None);
}

/// The two ports must never disagree. `pending_reboot` is the neutral
/// port's rendering of the comparison `reboot` performs, so a host where
/// the Status buffer says nothing and the Nix view says "reboot pending"
/// would be masys contradicting itself in two places at once.
#[test]
fn the_neutral_port_and_the_declarative_one_agree_about_a_reboot() {
    let state = NixosPlatform.reboot().expect("an answer");
    let pending = NixosPlatform.pending_reboot().expect("an answer");
    assert_eq!(state.is_some(), pending.is_some(), "one port sees a pending reboot and the other does not");
    if let (Some(state), Some(pending)) = (state, pending) {
        // The whole reason to answer in a sentence: "reboot now" and
        // "this can wait" are different findings, and the reason has to
        // say which of the two it is. Only a generation where *neither*
        // the kernel nor the initrd moved may claim it can wait - the
        // initrd is read at boot too, so an initrd-only change is a
        // reboot, not an activation.
        let can_wait = pending.reason.contains("activation-only");
        assert_eq!(can_wait, !state.kernel_changed && !state.initrd_changed, "the reason contradicts the reading: {pending:?}");
        // And it must name whichever actually moved, not merely report
        // that something did.
        if state.kernel_changed {
            assert!(pending.reason.contains("kernel changed"), "a changed kernel goes unnamed: {pending:?}");
        } else if state.initrd_changed {
            assert!(pending.reason.contains("initrd changed"), "a changed initrd goes unnamed: {pending:?}");
        }
    }
}

/// The Status buffer clips rather than wraps, and what it would clip is
/// the end of the line - which is where the verdict is.
///
/// The same rule `masys-render`'s `the_reboot_block_fits_an_eighty_column_
/// terminal` pins for the Nix view's reboot block, carried to the second
/// place the same fact lands. It was written there first because the block
/// overflowed there first; the reason string then grew to a hundred and
/// four characters and was cut off mid-word in the Status buffer, on this
/// host, reading `reboot pending: the current generation is not the one
/// tha`. A test that only measured one of the two renderers is what let
/// that through.
///
/// All four flag pairs, not the three distinct answers: what is being
/// pinned is that no input produces an over-long reason, and the mapping
/// from pairs to answers is the function's own business.
#[test]
fn every_reboot_reason_fits_an_eighty_column_status_line() {
    for (kernel_changed, initrd_changed) in [(true, true), (true, false), (false, true), (false, false)] {
        let reason = reboot_reason(kernel_changed, initrd_changed);
        assert!(
            reason.width() <= REASON_BUDGET,
            "{reason:?} is {} columns, and only {REASON_BUDGET} survive the Status line it is appended to",
            reason.width()
        );
    }
}

/// Three answers from four inputs, and the one that may say a reboot can
/// wait is the one where nothing that is read at boot moved. Pinned
/// against `reboot_reason` directly because the host cannot be made to
/// produce all four pairs on demand - the test above it can only check
/// whichever pair this machine happens to be in.
#[test]
fn only_an_unchanged_kernel_and_initrd_may_say_the_reboot_can_wait() {
    assert_eq!(reboot_reason(true, true), reboot_reason(true, false), "a changed kernel is the verdict either way");
    assert_eq!(reboot_reason(false, false), "activation-only");
    for (kernel_changed, initrd_changed) in [(true, true), (true, false), (false, true)] {
        let reason = reboot_reason(kernel_changed, initrd_changed);
        assert!(!reason.contains("activation-only"), "{reason:?} says a reboot can wait when something read at boot moved");
    }
}

/// A profile is reported because it has generations. `read_profile`
/// already answers `None` for a directory that is not there; this covers
/// the other absence, a directory that is there and empty. That is the
/// ordinary channels case on a flake host - `per-user/root` exists and
/// holds nothing - and a "Channels 0" section would send somebody looking
/// for channels this host does not use.
#[test]
fn a_reported_profile_always_has_generations() {
    for profile in NixosPlatform.profiles().expect("an answer") {
        assert!(!profile.generations.is_empty(), "{:?} was reported with no generations", profile.kind);
        let ascending = profile.generations.windows(2).all(|pair| pair[0].id < pair[1].id);
        assert!(ascending, "{:?} generations are not ascending by id", profile.kind);
    }
}

/// A whole revision or nothing, never an unexpanded `@revision@` and
/// never a short one. The value is a build-time substitution that stays a
/// placeholder on a system built outside a git checkout, and handing that
/// back as a revision would make every comparison against the lock report
/// drift that is not there.
#[test]
fn a_running_revision_is_a_whole_revision_or_nothing() {
    let Some(rev) = NixosPlatform.running_nixpkgs_rev().expect("an answer") else { return };
    assert!(rev.chars().all(|character| matches!(character, '0'..='9' | 'a'..='f')), "not a revision: {rev:?}");
    // Length, because shape alone cannot tell the two revisions in this
    // file apart. The same script carries `nixosVersion`, whose last
    // segment is the *short* rev - seven hex characters, which passes
    // every check above. It is the wrong one: the lock records forty, and
    // comparing seven calls two different revisions in sync whenever they
    // happen to share a prefix. `>=` rather than `== 40` so a move to
    // sha-256 commit ids fails nothing here - what this pins is the
    // discrimination against the short rev, not any particular hash.
    assert!(rev.len() >= 40, "a short revision where the whole one was wanted: {rev:?}");
}

/// A generated `home-manager-*` unit decides `Module`, and it must decide
/// it before the `PATH` probe runs: a host carrying both the unit and a
/// `home-manager` binary is still activated by `nixos-rebuild switch`, so
/// offering a standalone switch there would be a button that cannot work.
#[test]
fn a_generated_home_manager_unit_means_the_module_mode() {
    let generated = std::fs::read_dir("/etc/systemd/system")
        .map(|entries| entries.flatten().any(|entry| entry.file_name().to_string_lossy().starts_with("home-manager-")))
        .unwrap_or(false);
    if generated {
        assert_eq!(NixosPlatform.home_mode().expect("an answer"), HomeMode::Module);
    }
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
