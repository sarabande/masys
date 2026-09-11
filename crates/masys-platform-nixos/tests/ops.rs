//! The command every operation runs, asserted as data.
//!
//! **Nothing in this file spawns a process.** That is the point of
//! splitting `argv` from `run`: the whole operation mapping is decided by
//! a pure function, so it can be pinned on a machine that is not NixOS
//! and without a rebuild ever happening. A test here that called `run`
//! would activate a system configuration.

use masys_domain::declarative::{NixOp, RebuildVerb};
use masys_domain::error::MasysError;
use masys_platform_nixos::ops::{
    Elevate, accepts_elevate, activation_argv, argv, elevation, flake_ref_from, offered, on_path,
    outcome, run_with, switched_but_not_activated,
};

/// The flake reference the rebuild tests use. A path rather than a
/// `github:` ref because that is what resolves on a real host, out of
/// `$NH_FLAKE` or `/etc/nixos`.
const FLAKE: &str = "/home/user/.dotfiles";

const SYSTEM: &str = "/nix/var/nix/profiles/system";

#[test]
fn a_rebuild_appends_the_flake_ref_where_one_resolves() {
    let op = NixOp::Rebuild(RebuildVerb::Switch);
    assert_eq!(
        argv(&op, Some(FLAKE), Elevate::No),
        vec!["nixos-rebuild", "switch", "--flake", FLAKE]
    );
}

/// A bare `nixos-rebuild switch` is the correct command on a channels
/// host, not a broken one. The flake-only operations dim where no ref
/// resolves; the rebuild verbs stay live, so this must be a command
/// somebody can actually run.
#[test]
fn a_rebuild_without_a_flake_ref_is_still_a_valid_command() {
    assert_eq!(
        argv(&NixOp::Rebuild(RebuildVerb::Switch), None, Elevate::No),
        vec!["nixos-rebuild", "switch"]
    );
}

/// The five sub-command spellings, which are not a mechanical transform
/// of the variant names - `DryActivate` is `dry-activate`. A derive that
/// lowercased the variant would be right four times out of five and
/// silently wrong on the fifth.
#[test]
fn every_rebuild_verb_maps_to_its_own_sub_command() {
    let verbs = [
        (RebuildVerb::Switch, "switch"),
        (RebuildVerb::Boot, "boot"),
        (RebuildVerb::Test, "test"),
        (RebuildVerb::Build, "build"),
        (RebuildVerb::DryActivate, "dry-activate"),
    ];
    for (verb, expected) in verbs {
        assert_eq!(
            argv(&NixOp::Rebuild(verb), None, Elevate::No),
            vec!["nixos-rebuild", expected],
            "for {verb:?}"
        );
    }
}

/// Activating a specific generation has no high-level form - there is no
/// `nixos-rebuild --to-generation` - so it is two commands. `argv`
/// returns the first; `run` chains the second.
#[test]
fn activating_a_generation_switches_the_profile_first() {
    let op = NixOp::Activate {
        profile: SYSTEM.to_string(),
        generation: 427,
    };
    assert_eq!(
        argv(&op, None, Elevate::No),
        vec!["nix-env", "-p", SYSTEM, "--switch-generation", "427"]
    );
}

/// The second command, and it is read *through* the profile symlink the
/// first one has just repointed. Which is exactly why it cannot be
/// folded into `argv`: it is not a second word in one command line, it is
/// a second command that is only correct after the first succeeded.
#[test]
fn activating_a_generation_then_runs_that_generations_activation_script() {
    assert_eq!(
        activation_argv(SYSTEM, Elevate::No),
        vec![
            "/nix/var/nix/profiles/system/bin/switch-to-configuration",
            "switch"
        ]
    );
}

/// `--rollback` is a flag on an action, not an action of its own.
/// `nixos-rebuild --help` on this host lists thirteen actions -
/// `{switch,boot,test,build,edit,repl,dry-build,dry-run,dry-activate,
/// build-image,build-vm,build-vm-with-bootloader,list-generations}` - and
/// `rollback` is not among them, so `nixos-rebuild rollback` prints usage
/// and exits. `switch` is the action it pairs with, because a rollback
/// means running the previous configuration *now*; `boot` would leave the
/// current one running until a reboot.
///
/// And no flake reference, where the five rebuild verbs take one: this
/// builds nothing, which is why it is its own `NixOp` and not a
/// `RebuildVerb`.
#[test]
fn a_rollback_is_switch_with_a_flag_and_takes_no_flake_ref() {
    assert_eq!(
        argv(&NixOp::Rollback, Some(FLAKE), Elevate::No),
        vec!["nixos-rebuild", "switch", "--rollback"]
    );
}

#[test]
fn a_diff_compares_two_store_paths() {
    let op = NixOp::Diff {
        from: "/nix/store/aaa-nixos-system".to_string(),
        to: "/run/current-system".to_string(),
    };
    assert_eq!(
        argv(&op, None, Elevate::No),
        vec![
            "nix",
            "store",
            "diff-closures",
            "/nix/store/aaa-nixos-system",
            "/run/current-system"
        ]
    );
}

/// `clean` is the standard answer: `nix-collect-garbage
/// --delete-older-than` prunes profile generations *and* collects the
/// store in one step.
#[test]
fn a_clean_prunes_and_collects_in_one_command() {
    assert_eq!(
        argv(
            &NixOp::Clean {
                older_than: "14d".to_string()
            },
            None,
            Elevate::No
        ),
        vec!["nix-collect-garbage", "--delete-older-than", "14d"]
    );
}

/// The escape hatch, and a different command from `clean` rather than a
/// variation on it: `nix-env --delete-generations` is the legacy
/// imperative interface and prunes without collecting, so nothing is
/// reclaimed until something else collects the store.
#[test]
fn deleting_generations_is_a_different_command_from_cleaning() {
    let op = NixOp::DeleteGenerations {
        profile: SYSTEM.to_string(),
        spec: "+5".to_string(),
    };
    assert_eq!(
        argv(&op, None, Elevate::No),
        vec!["nix-env", "-p", SYSTEM, "--delete-generations", "+5"]
    );
}

/// `nix flake update`'s positional arguments are *input names* - measured
/// against nix 2.34.8 on this host, whose synopsis is `nix flake update
/// [option...] inputs...`. So the flake itself goes in `--flake`, and
/// appending the ref positionally the way `flake check` takes it would
/// ask nix to update an input named `/home/user/.dotfiles`.
#[test]
fn a_flake_update_names_the_flake_with_a_flag_not_a_positional() {
    let op = NixOp::FlakeUpdate { input: None };
    assert_eq!(
        argv(&op, Some(FLAKE), Elevate::No),
        vec!["nix", "flake", "update", "--flake", FLAKE]
    );
}

#[test]
fn a_flake_update_of_one_input_names_it_after_the_flake() {
    let op = NixOp::FlakeUpdate {
        input: Some("nixpkgs".to_string()),
    };
    assert_eq!(
        argv(&op, Some(FLAKE), Elevate::No),
        vec!["nix", "flake", "update", "--flake", FLAKE, "nixpkgs"]
    );
}

/// A flake-only operation dims where no ref resolves, and a dimmed row is
/// still runnable behind a confirmation - masys never hides an action
/// outright, only marks it. So this must stay a command nix accepts: with
/// no `--flake` it operates on the working directory, which is nix's own
/// default and not something masys invented.
#[test]
fn a_flake_update_without_a_ref_falls_back_to_the_working_directory() {
    assert_eq!(
        argv(&NixOp::FlakeUpdate { input: None }, None, Elevate::No),
        vec!["nix", "flake", "update"]
    );
}

/// `nix flake check [option...] flake-url` - measured on nix 2.34.8 here.
/// A positional, unlike `flake update`, because for this sub-command the
/// positional slot *is* the flake.
#[test]
fn a_flake_check_takes_the_ref_as_a_positional() {
    assert_eq!(
        argv(&NixOp::FlakeCheck, Some(FLAKE), Elevate::No),
        vec!["nix", "flake", "check", FLAKE]
    );
}

#[test]
fn a_flake_check_without_a_ref_falls_back_to_the_working_directory() {
    assert_eq!(
        argv(&NixOp::FlakeCheck, None, Elevate::No),
        vec!["nix", "flake", "check"]
    );
}

/// The channel operations are the mirror image of the flake ones, and a
/// flake ref means nothing to them: they act on root's channel profile.
#[test]
fn a_channel_update_ignores_any_flake_ref() {
    assert_eq!(
        argv(&NixOp::ChannelUpdate, Some(FLAKE), Elevate::No),
        vec!["nix-channel", "--update"]
    );
}

#[test]
fn a_channel_rollback_ignores_any_flake_ref() {
    assert_eq!(
        argv(&NixOp::ChannelRollback, Some(FLAKE), Elevate::No),
        vec!["nix-channel", "--rollback"]
    );
}

/// `nix search nixpkgs`, which is local. `nh search options` queries
/// search.nixos.org and masys has no network code.
#[test]
fn a_package_search_queries_nixpkgs_locally() {
    let op = NixOp::SearchPackages {
        query: "ripgrep".to_string(),
    };
    assert_eq!(
        argv(&op, None, Elevate::No),
        vec!["nix", "search", "nixpkgs", "ripgrep"]
    );
}

/// `nixos-option` answers what an option is set to *on this host*, which
/// is a narrower and better question for a system tool than searching all
/// of nixpkgs - and it needs no network to answer.
#[test]
fn an_option_lookup_asks_this_host_not_nixpkgs() {
    let op = NixOp::OptionValue {
        name: "services.openssh.enable".to_string(),
    };
    assert_eq!(
        argv(&op, None, Elevate::No),
        vec!["nixos-option", "services.openssh.enable"]
    );
}

/// Standalone home-manager takes a flake the same way the rebuild verbs
/// do, and works without one for a host configured through `home.nix`.
#[test]
fn a_home_switch_appends_the_flake_ref_where_one_resolves() {
    assert_eq!(
        argv(&NixOp::HomeSwitch, Some(FLAKE), Elevate::No),
        vec!["home-manager", "switch", "--flake", FLAKE]
    );
}

#[test]
fn a_home_switch_without_a_flake_ref_is_still_a_valid_command() {
    assert_eq!(
        argv(&NixOp::HomeSwitch, None, Elevate::No),
        vec!["home-manager", "switch"]
    );
}

/// Every operation must name a program. An empty argv would reach
/// `Command::new("")`, and the failure would surface as a confusing "No
/// such file or directory" rather than as the missing mapping it is.
#[test]
fn no_operation_produces_an_empty_command() {
    let operations = [
        NixOp::Rebuild(RebuildVerb::Switch),
        NixOp::Activate {
            profile: SYSTEM.to_string(),
            generation: 1,
        },
        NixOp::Rollback,
        NixOp::Diff {
            from: "a".to_string(),
            to: "b".to_string(),
        },
        NixOp::Clean {
            older_than: "14d".to_string(),
        },
        NixOp::DeleteGenerations {
            profile: SYSTEM.to_string(),
            spec: "+5".to_string(),
        },
        NixOp::FlakeUpdate { input: None },
        NixOp::FlakeCheck,
        NixOp::ChannelUpdate,
        NixOp::ChannelRollback,
        NixOp::SearchPackages {
            query: "q".to_string(),
        },
        NixOp::OptionValue {
            name: "n".to_string(),
        },
        NixOp::HomeSwitch,
    ];
    for op in operations {
        for flake in [None, Some(FLAKE)] {
            let words = argv(&op, flake, Elevate::No);
            assert!(
                !words.is_empty(),
                "{op:?} with flake {flake:?} produced no command"
            );
            assert!(
                !words[0].is_empty(),
                "{op:?} with flake {flake:?} named no program"
            );
        }
    }
}

/// One command for everything but an activation. `run_with` takes its
/// spawner as an argument precisely so this is assertable: nothing below
/// creates a process, and a test here that spawned would activate a
/// system configuration.
#[test]
fn an_operation_that_is_not_an_activation_runs_one_command() {
    let mut ran: Vec<Vec<String>> = Vec::new();
    let result = run_with(
        &NixOp::Rebuild(RebuildVerb::Switch),
        None,
        Elevate::No,
        |argv| {
            ran.push(argv.to_vec());
            Ok(())
        },
    );

    assert!(result.is_ok());
    assert_eq!(
        ran,
        vec![vec!["nixos-rebuild".to_string(), "switch".to_string()]]
    );
}

/// The order is the whole of the correctness here.
/// `switch-to-configuration` is reached *through* the profile symlink
/// that `nix-env --switch-generation` has just repointed, so a run where
/// the switch failed and the script went ahead anyway would re-activate
/// the generation being replaced and report that it had done what was
/// asked. That rationale was a comment with nothing holding it.
#[test]
fn the_activation_script_never_runs_where_the_profile_switch_failed() {
    let op = NixOp::Activate {
        profile: SYSTEM.to_string(),
        generation: 427,
    };
    let mut ran: Vec<Vec<String>> = Vec::new();
    let result = run_with(&op, None, Elevate::No, |argv| {
        ran.push(argv.to_vec());
        Err(MasysError::Command("nix-env exited 1".to_string()))
    });

    assert!(result.is_err());
    assert_eq!(
        ran.len(),
        1,
        "only the profile switch was attempted: {ran:?}"
    );
    assert_eq!(ran[0][0], "nix-env");
}

/// Both halves, in order, where the first succeeds.
#[test]
fn activating_a_generation_switches_the_profile_then_runs_its_script() {
    let op = NixOp::Activate {
        profile: SYSTEM.to_string(),
        generation: 427,
    };
    let mut ran: Vec<Vec<String>> = Vec::new();
    let result = run_with(&op, None, Elevate::No, |argv| {
        ran.push(argv.to_vec());
        Ok(())
    });

    assert!(result.is_ok());
    assert_eq!(
        ran,
        vec![
            argv(&op, None, Elevate::No),
            activation_argv(SYSTEM, Elevate::No)
        ]
    );
}

/// The half-failed activation, which is the one outcome the failing
/// command's own message cannot describe: the profile has already moved.
/// On screen that is indistinguishable from a successful `nixos-rebuild
/// boot`, and the reboot row does not correct it - `reboot_state`
/// compares `/run/booted-system` against `/run/current-system` and
/// `nix-env --switch-generation` touches neither. So the error must say
/// the profile moved, which generation it moved to, and how to move it
/// back, without losing the failing command's own text.
#[test]
fn an_activation_that_fails_halfway_says_the_profile_has_already_moved() {
    let op = NixOp::Activate {
        profile: SYSTEM.to_string(),
        generation: 427,
    };
    let mut ran: Vec<Vec<String>> = Vec::new();
    let failed = run_with(&op, None, Elevate::No, |argv| {
        ran.push(argv.to_vec());
        (ran.len() == 1).then_some(()).ok_or_else(|| {
            MasysError::Command("switch-to-configuration switch exited 1".to_string())
        })
    })
    .unwrap_err()
    .to_string();

    assert_eq!(ran.len(), 2, "both halves were attempted: {ran:?}");
    assert!(
        failed.contains("generation 427"),
        "names the generation: {failed}"
    );
    assert!(
        failed.contains("is now the system profile"),
        "says the profile already moved: {failed}"
    );
    assert!(
        failed.contains("activate another generation"),
        "says how to move it back: {failed}"
    );
    assert!(
        failed.contains("switch-to-configuration switch exited 1"),
        "keeps the command's own message: {failed}"
    );
}

/// A failure in the *first* half is reported as itself. Nothing has
/// moved there, so claiming the profile had would be the same fabrication
/// in the other direction.
#[test]
fn a_profile_switch_that_fails_is_never_reported_as_a_moved_profile() {
    let op = NixOp::Activate {
        profile: SYSTEM.to_string(),
        generation: 427,
    };
    let failed = run_with(&op, None, Elevate::No, |_| {
        Err(MasysError::Command("nix-env exited 1".to_string()))
    })
    .unwrap_err()
    .to_string();

    assert!(!failed.contains("is now the system profile"), "{failed}");
}

/// Three lines, because `masys_render` draws a `StatusLine::Error` line
/// by line and truncates each at the frame's width. One sentence this
/// long would lose its tail, and the tail is the recovery.
#[test]
fn the_half_activation_error_puts_the_recovery_where_a_narrow_frame_keeps_it() {
    let inner = MasysError::Command("switch-to-configuration switch exited 1".to_string());
    let reported = switched_but_not_activated(427, &inner).to_string();

    let lines: Vec<&str> = reported.lines().collect();
    assert_eq!(lines.len(), 3, "{reported}");
    assert!(
        lines.iter().all(|line| line.len() <= 110),
        "each line fits an ordinary frame: {lines:?}"
    );
}

/// A wait status, built rather than waited for. `ExitStatusExt::from_raw`
/// takes the same encoding `waitpid` fills in - the low byte holds the
/// signal, the next byte the exit code - so what a real child would
/// produce is assertable here without one.
fn status(raw: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(raw)
}

#[test]
fn a_command_that_exited_reports_its_code() {
    assert_eq!(outcome(status(1 << 8)), "exited 1");
    assert_eq!(outcome(status(2 << 8)), "exited 2");
}

/// The one this exists for. `unwrap_or(-1)` rendered a signalled child as
/// "exited -1", which is a status no process ever has: `$?` cannot show
/// it, and an operator looking up what returns -1 is looking for
/// something that did not happen. It is the fabricated zero in different
/// clothes.
///
/// Not ctrl-c, whatever the comment this replaced said. The binary calls
/// `ratatui::restore` before handing the terminal over, so the child runs
/// out of raw mode and ctrl-c signals the whole foreground process group;
/// masys installs no handler and dies with it, never reaching the error.
/// The cases that do reach it are the OOM killer, a `kill` from another
/// terminal, and a segfaulting activation script - 9, 15 and 11 here.
#[test]
fn a_command_a_signal_killed_says_so_rather_than_inventing_a_status() {
    assert_eq!(outcome(status(9)), "killed by signal 9");
    assert_eq!(outcome(status(15)), "killed by signal 15");
    assert_eq!(outcome(status(11)), "killed by signal 11");
    assert!(!outcome(status(9)).contains("-1"), "no invented status");
    assert!(
        !outcome(status(9)).contains("exited"),
        "and it did not exit"
    );
}

/// `$NH_FLAKE` first, because it is the variable actually set on real
/// NixOS hosts. Reading a variable another tool defines is not a
/// dependency on that tool - masys never runs `nh`.
#[test]
fn nh_flake_wins_over_the_plain_flake_variable() {
    assert_eq!(
        flake_ref_from(Some("/a"), Some("/b"), false),
        Some("/a".to_string())
    );
}

#[test]
fn the_plain_flake_variable_is_used_where_nh_flake_is_unset() {
    assert_eq!(
        flake_ref_from(None, Some("/b"), false),
        Some("/b".to_string())
    );
}

/// `/etc/nixos` is the last source, and only where it actually holds a
/// `flake.nix`. Every NixOS host has an `/etc/nixos`; only some have a
/// flake in it, and naming the directory regardless would hand
/// `nixos-rebuild` a ref that cannot evaluate.
#[test]
fn etc_nixos_is_the_last_resort_and_only_where_it_holds_a_flake() {
    assert_eq!(
        flake_ref_from(None, None, true),
        Some("/etc/nixos".to_string())
    );
    assert_eq!(flake_ref_from(None, None, false), None);
}

/// An exported-but-empty `NH_FLAKE=` is a shell that meant to unset it,
/// not a flake at the root of the filesystem. Taking it at face value
/// would produce `--flake ''`, which fails on the empty string instead of
/// falling through to the next source - and, worse, would make a channels
/// host that happens to export it stop offering a working bare
/// `nixos-rebuild switch`.
#[test]
fn an_empty_variable_is_not_a_flake_reference() {
    assert_eq!(
        flake_ref_from(Some(""), Some("/b"), false),
        Some("/b".to_string())
    );
    assert_eq!(
        flake_ref_from(Some("   "), None, true),
        Some("/etc/nixos".to_string())
    );
    assert_eq!(flake_ref_from(Some(""), Some(""), false), None);
}

/// `nix`'s own lookup order for `<nixos-config>`: an explicit `NIX_PATH`
/// entry, then `/etc/nixos/configuration.nix`.
///
/// Taken as readings rather than performed, so this runs on a machine
/// with neither - which is what the development host is, and why the
/// `option value` row is marked there.
#[test]
fn a_nixos_config_is_found_on_the_search_path_before_etc_nixos() {
    use masys_platform_nixos::ops::nixos_config_from;

    assert_eq!(
        nixos_config_from(
            Some("nixpkgs=flake:nixpkgs:nixos-config=/srv/cfg.nix"),
            None
        ),
        Some("/srv/cfg.nix".to_string())
    );
    assert_eq!(
        nixos_config_from(
            Some("nixpkgs=flake:nixpkgs:nixos-config=/srv/cfg.nix"),
            Some("/etc/nixos/configuration.nix")
        ),
        Some("/srv/cfg.nix".to_string()),
        "the explicit entry wins"
    );
    assert_eq!(
        nixos_config_from(
            Some("nixpkgs=flake:nixpkgs"),
            Some("/etc/nixos/configuration.nix")
        ),
        Some("/etc/nixos/configuration.nix".to_string()),
        "and the file is the fallback"
    );
}

/// The development host, exactly: a flake `NIX_PATH` with no
/// `nixos-config` entry, a channels directory that does not exist, and no
/// `/etc/nixos`. `nixos-option` fails there, so the row is marked.
#[test]
fn a_flake_host_with_no_nix_path_entry_has_no_nixos_config() {
    use masys_platform_nixos::ops::nixos_config_from;

    assert_eq!(
        nixos_config_from(
            Some("nixpkgs=flake:nixpkgs:/nix/var/nix/profiles/per-user/root/channels"),
            None
        ),
        None
    );
    assert_eq!(nixos_config_from(None, None), None);
    assert_eq!(
        nixos_config_from(Some("nixos-config="), None),
        None,
        "an empty entry is not a path"
    );
}

/// The flag is nixos-rebuild-ng's. A host still running the classic shell
/// script would take it, print usage and exit - after the build, which is
/// the failure passing it is meant to remove.
///
/// Both fixtures are abridged from real output: the first from
/// `nixos-rebuild --help` on nixos-rebuild-ng 26.11, the second from the
/// classic script's usage text, which lists its options and has no
/// elevation among them.
#[test]
fn elevate_is_offered_only_by_a_nixos_rebuild_that_documents_it() {
    let ng = "\
     [--profile-name PROFILE_NAME] [--specialisation  SPECIALISATION]  [--roll-
     back]   [--store-path  STORE_PATH]  [--upgrade]  [--upgrade-all]  [--json]
     [--elevate {none,sudo,run0}] [--ask-elevate-password] [--no-reexec]";
    let classic = "\
Usage: nixos-rebuild [--help] {switch | boot | test | build | edit | repl |
         dry-build | dry-run | dry-activate | build-vm | build-vm-with-bootloader}
         [{--upgrade | --upgrade-all}] [--install-bootloader] [--rollback]";

    assert!(accepts_elevate(ng));
    assert!(
        !accepts_elevate(classic),
        "the classic script has no such flag and must not be handed one"
    );
}

/// Who gets elevated, and who does not.
///
/// Every verb that reaches `switch-to-configuration` gets it, measured on
/// the host and recorded in issue #13: `switch`, `boot`, `test` and
/// `dry-activate` are all refused there unprivileged. `dry-activate`
/// changes nothing and is elevated anyway, because the alternative is not
/// "no password prompt" but "no answer, five minutes later".
#[test]
fn only_the_verbs_that_activate_are_elevated() {
    let unprivileged = 1000;
    for verb in [
        RebuildVerb::Switch,
        RebuildVerb::Boot,
        RebuildVerb::Test,
        RebuildVerb::DryActivate,
    ] {
        assert_eq!(
            elevation(
                &NixOp::Rebuild(verb),
                unprivileged,
                Some(true),
                Elevate::Run0,
                |_| false
            ),
            Elevate::Run0,
            "for {verb:?}"
        );
    }
    // `build` stops at a store path and is the one verb that never
    // reaches `switch-to-configuration`.
    assert_eq!(
        elevation(
            &NixOp::Rebuild(RebuildVerb::Build),
            unprivileged,
            Some(true),
            Elevate::Run0,
            |_| false
        ),
        Elevate::No
    );
    // A rollback is a switch that builds from somewhere else.
    assert_eq!(
        elevation(
            &NixOp::Rollback,
            unprivileged,
            Some(true),
            Elevate::Run0,
            |_| false
        ),
        Elevate::Run0
    );
}

/// Root needs nothing, and neither does a host whose `nixos-rebuild` has
/// not been asked - or could not be.
///
/// `None` is the unread case and it is treated as "no": passing a flag
/// masys did not confirm trades a refusal at activation for a usage error
/// at activation, which is the same minutes gone and a stranger message.
#[test]
fn nothing_is_elevated_without_both_a_reason_and_a_measurement() {
    let switch = NixOp::Rebuild(RebuildVerb::Switch);
    assert_eq!(
        elevation(&switch, 0, Some(true), Elevate::Run0, |_| false),
        Elevate::No,
        "root has nothing to raise"
    );
    assert_eq!(
        elevation(&switch, 1000, Some(false), Elevate::Run0, |_| false),
        Elevate::No,
        "this nixos-rebuild does not take it"
    );
    assert_eq!(
        elevation(&switch, 1000, None, Elevate::Run0, |_| false),
        Elevate::No,
        "and an unread probe is not a yes"
    );
}

/// The operations with no elevation flag are run *through* the method
/// instead, and never handed a flag that would not parse.
///
/// `nix-env` takes no such argument and `nix-collect-garbage`'s synopsis
/// is `[--delete-old] [-d] [--delete-older-than period] [--max-freed
/// bytes] [--dry-run]`. Passing `--elevate` to either would produce a
/// command line that fails to parse, after the operator agreed to it.
#[test]
fn the_operations_without_a_flag_are_wrapped_rather_than_flagged() {
    let system = "/nix/var/nix/profiles/system";
    let ops = [
        NixOp::Activate {
            profile: system.to_string(),
            generation: 427,
        },
        NixOp::Clean {
            older_than: "14d".to_string(),
        },
        NixOp::DeleteGenerations {
            profile: system.to_string(),
            spec: "+5".to_string(),
        },
    ];
    for op in ops {
        let command = argv(&op, None, Elevate::Run0);
        assert_eq!(
            command.first().map(String::as_str),
            Some("run0"),
            "for {op:?}: {command:?}"
        );
        assert!(
            !command.iter().any(|word| word == "--elevate"),
            "for {op:?}: {command:?}"
        );
    }
    // And the second half of an activation, which is a command of its own.
    assert_eq!(
        activation_argv(system, Elevate::Run0),
        vec![
            "run0",
            "/nix/var/nix/profiles/system/bin/switch-to-configuration",
            "switch"
        ]
    );
}

/// The four that only read are never elevated, whatever the host offers.
///
/// `nix store diff-closures`, `nix flake check`, `nixos-option` and `nix
/// search` change nothing, and a password prompt to look at something
/// teaches an operator to type one without reading.
#[test]
fn reading_never_asks_for_root() {
    let ops = [
        NixOp::Diff {
            from: "/nix/store/a".to_string(),
            to: "/nix/store/b".to_string(),
        },
        NixOp::FlakeCheck,
        NixOp::OptionValue {
            name: "services.openssh.enable".to_string(),
        },
        NixOp::SearchPackages {
            query: "ripgrep".to_string(),
        },
        NixOp::ChannelUpdate,
        NixOp::FlakeUpdate { input: None },
    ];
    for op in ops {
        assert_eq!(
            elevation(&op, 1000, Some(true), Elevate::Run0, |_| false),
            Elevate::No,
            "for {op:?}"
        );
    }
}

/// A profile this operator already owns is written directly.
///
/// `~/.local/state/nix/profiles` is the operator's, so deleting a
/// home-manager generation needs nothing - and prompting for root to
/// remove your own generation would be masys asking for a privilege the
/// act does not use. Asked per profile rather than of the uid, which is
/// what makes the two cases distinguishable at all.
#[test]
fn a_profile_the_operator_owns_is_not_elevated_for() {
    let home = "/home/user/.local/state/nix/profiles/profile";
    let system = "/nix/var/nix/profiles/system";
    let owns_home = |path: &str| path == home;

    let delete_home = NixOp::DeleteGenerations {
        profile: home.to_string(),
        spec: "+5".to_string(),
    };
    let delete_system = NixOp::DeleteGenerations {
        profile: system.to_string(),
        spec: "+5".to_string(),
    };
    assert_eq!(
        elevation(&delete_home, 1000, Some(true), Elevate::Run0, owns_home),
        Elevate::No
    );
    assert_eq!(
        elevation(&delete_system, 1000, Some(true), Elevate::Run0, owns_home),
        Elevate::Run0
    );

    // And root needs nothing either way.
    assert_eq!(
        elevation(&delete_system, 0, Some(true), Elevate::Run0, owns_home),
        Elevate::No
    );
}

/// `clean` is asked of the uid rather than of a path, because it has no
/// one path: `nix-collect-garbage` "looks in a few locations, and acts on
/// all profiles it finds there" - the system profile among them, which is
/// never the operator's.
#[test]
fn cleaning_is_elevated_even_where_the_operator_owns_a_profile() {
    let clean = NixOp::Clean {
        older_than: "14d".to_string(),
    };
    assert_eq!(
        elevation(&clean, 1000, Some(true), Elevate::Run0, |_| true),
        Elevate::Run0
    );
    assert_eq!(
        elevation(&clean, 0, Some(true), Elevate::Run0, |_| true),
        Elevate::No,
        "root needs nothing"
    );
}

/// A host with no `run0` and no `sudo` gets no wrapper, rather than a
/// command prefixed with a program that is not there.
#[test]
fn a_host_that_offers_nothing_wraps_nothing() {
    let clean = NixOp::Clean {
        older_than: "14d".to_string(),
    };
    assert_eq!(
        elevation(&clean, 1000, Some(true), Elevate::No, |_| false),
        Elevate::No
    );
    assert_eq!(
        argv(&clean, None, Elevate::No),
        vec!["nix-collect-garbage", "--delete-older-than", "14d"]
    );
}

/// And where it is elevated, the flag lands after the sub-command and its
/// flake, so a host that elevates and one that does not differ by a
/// suffix rather than by a rearrangement.
#[test]
fn an_elevated_rebuild_carries_the_flag_after_its_flake() {
    assert_eq!(
        argv(
            &NixOp::Rebuild(RebuildVerb::Switch),
            Some(FLAKE),
            Elevate::Run0
        ),
        vec![
            "nixos-rebuild",
            "switch",
            "--flake",
            FLAKE,
            "--elevate",
            "run0"
        ]
    );
    assert_eq!(
        argv(&NixOp::Rollback, None, Elevate::Sudo),
        vec!["nixos-rebuild", "switch", "--rollback", "--elevate", "sudo"]
    );
}

/// `sudo` first, because `sudo` asks on the terminal and `run0` asks
/// wherever the session's polkit agent is - a window behind the terminal
/// on a desktop, and nowhere at all over SSH.
///
/// These operations are the ones that take the screen. masys hands the
/// terminal over so the command can stream its own output, and a password
/// prompt that lands somewhere else defeats the handover.
#[test]
fn the_method_that_asks_on_the_terminal_is_preferred() {
    assert_eq!(
        offered(true, true),
        Elevate::Sudo,
        "both present: the terminal one wins"
    );
    assert_eq!(offered(false, true), Elevate::Sudo);
    assert_eq!(
        offered(true, false),
        Elevate::Run0,
        "run0 where there is no sudo, since a GUI prompt beats none"
    );
    assert_eq!(
        offered(false, false),
        Elevate::No,
        "a host with neither gets nothing rather than a broken command"
    );
}

/// Which is looked for on `PATH` and nowhere else, so a `run0` sitting in
/// a directory this operator does not run commands from is not one masys
/// can use.
#[test]
fn a_method_is_found_on_path_or_not_at_all() {
    // Two files exist: the real one, and a `run0` in the working
    // directory - which is what an empty `PATH` entry resolves to, and
    // what this fixture is here to catch. A version of it that named only
    // the absolute path could not tell the two cases apart.
    let present = |candidate: &std::path::Path| {
        candidate == std::path::Path::new("/run/current-system/sw/bin/run0")
            || candidate == std::path::Path::new("run0")
    };
    assert!(on_path(
        "run0",
        "/usr/bin:/run/current-system/sw/bin",
        present
    ));
    assert!(!on_path("run0", "/usr/bin:/usr/local/bin", present));
    assert!(
        !on_path("sudo", "/run/current-system/sw/bin", present),
        "the same directory, a different program"
    );
    // A masys started from a directory holding a file called `run0` must
    // not hand that file root.
    assert!(
        !on_path("run0", "", present),
        "an empty PATH offers nothing"
    );
    assert!(
        !on_path("run0", "/usr/bin::/usr/local/bin", present),
        "and neither does an empty entry between two real ones"
    );
}

/// `--upgrade` updates the channels and then does exactly what `switch`
/// does, so it pairs with `switch` the way `--rollback` does.
///
/// And no flake reference, for a sharper reason than `Rollback`'s: this
/// one *would* build from the configuration, but `--upgrade` updates
/// channels, and a flake host has none. The row dims there rather than
/// producing a command that asks `nixos-rebuild` to refresh inputs the
/// configuration does not read.
#[test]
fn an_upgrade_is_switch_with_a_flag_and_takes_no_flake_ref() {
    assert_eq!(
        argv(&NixOp::Upgrade, Some(FLAKE), Elevate::No),
        vec!["nixos-rebuild", "switch", "--upgrade"]
    );
}

/// Elevated on the same terms as `switch`, which is what it is once the
/// channels are updated: an activation that fails for want of privilege
/// wastes the same minutes.
#[test]
fn an_upgrade_elevates_the_way_switch_does() {
    assert_eq!(
        argv(&NixOp::Upgrade, None, Elevate::Sudo),
        vec!["nixos-rebuild", "switch", "--upgrade", "--elevate", "sudo"]
    );
}

/// `nixos-rebuild repl` takes a flake reference where one resolves, unlike
/// `--rollback` and `--upgrade`: it is one of the thirteen actions rather
/// than a flag on `switch`, and it opens a repl *on this configuration*,
/// which is the thing the reference names.
#[test]
fn a_repl_is_an_action_and_takes_the_flake_ref() {
    assert_eq!(
        argv(&NixOp::Repl, Some(FLAKE), Elevate::No),
        vec!["nixos-rebuild", "repl", "--flake", FLAKE]
    );
    assert_eq!(
        argv(&NixOp::Repl, None, Elevate::No),
        vec!["nixos-rebuild", "repl"]
    );
}

/// And never elevates. `repl` builds nothing and activates nothing - it
/// evaluates - so there is no activation for polkit to refuse and no
/// reason to ask for privilege the operator would then be holding inside
/// an interactive session.
#[test]
fn a_repl_never_elevates() {
    assert_eq!(
        argv(&NixOp::Repl, None, Elevate::Sudo),
        vec!["nixos-rebuild", "repl"]
    );
}

/// `build-vm` builds a runner, and takes `--flake` where one resolves.
///
/// Checked against `nixos-rebuild(8)` on nix 2.34.8: the sub-command
/// takes no argument of its own and leaves its result at
/// `./result/bin/run-<host>-vm`.
#[test]
fn build_vm_builds_a_runner_for_this_configuration() {
    assert_eq!(
        argv(&NixOp::BuildVm, Some(FLAKE), Elevate::No),
        vec!["nixos-rebuild", "build-vm", "--flake", FLAKE]
    );
    // A channels host has no ref, and a bare `build-vm` is the correct
    // command there rather than a broken one.
    assert_eq!(
        argv(&NixOp::BuildVm, None, Elevate::No),
        vec!["nixos-rebuild", "build-vm"]
    );
}

/// **No `--image-variant` is what makes it list rather than build.**
///
/// nixos-rebuild(8): "Select a variant with the --image-variant option or
/// run without any options to get a list of available variants." That
/// sentence is why the two are separate operations - the list is how the
/// variant a caller types is discovered, and masys has no picker to fill.
#[test]
fn build_image_with_no_variant_lists_the_variants() {
    let command = argv(&NixOp::ListImageVariants, Some(FLAKE), Elevate::No);
    assert_eq!(
        command,
        vec!["nixos-rebuild", "build-image", "--flake", FLAKE]
    );
    assert!(
        !command.iter().any(|word| word == "--image-variant"),
        "a variant would make this build one instead of listing them: {command:?}"
    );
}

/// And with a variant, it builds that one.
#[test]
fn build_image_with_a_variant_builds_it() {
    assert_eq!(
        argv(
            &NixOp::BuildImage {
                variant: "proxmox".to_string()
            },
            Some(FLAKE),
            Elevate::No,
        ),
        vec![
            "nixos-rebuild",
            "build-image",
            "--image-variant",
            "proxmox",
            "--flake",
            FLAKE
        ]
    );
}

/// None of the three elevates.
///
/// They build and activate nothing, so there is no activation for polkit
/// to refuse - the same reasoning `repl` carries, and the reason all
/// three fall through `rebuild_elevation`'s wildcard rather than earning
/// arms of their own.
#[test]
fn the_build_only_operations_never_elevate() {
    for op in [
        NixOp::BuildVm,
        NixOp::ListImageVariants,
        NixOp::BuildImage {
            variant: "proxmox".to_string(),
        },
    ] {
        let command = argv(&op, None, Elevate::Sudo);
        assert!(
            !command.iter().any(|word| word == "sudo" || word == "run0"),
            "{op:?} must not be wrapped: {command:?}"
        );
    }
}
