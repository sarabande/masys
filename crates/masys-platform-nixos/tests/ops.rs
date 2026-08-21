//! The command every operation runs, asserted as data.
//!
//! **Nothing in this file spawns a process.** That is the point of
//! splitting `argv` from `run`: the whole operation mapping is decided by
//! a pure function, so it can be pinned on a machine that is not NixOS
//! and without a rebuild ever happening. A test here that called `run`
//! would activate a system configuration.

use masys_domain::declarative::{NixOp, RebuildVerb};
use masys_domain::error::MasysError;
use masys_platform_nixos::ops::{activation_argv, argv, flake_ref_from, outcome, run_with, switched_but_not_activated};

/// The flake reference the rebuild tests use. A path rather than a
/// `github:` ref because that is what resolves on a real host, out of
/// `$NH_FLAKE` or `/etc/nixos`.
const FLAKE: &str = "/home/user/.dotfiles";

const SYSTEM: &str = "/nix/var/nix/profiles/system";

#[test]
fn a_rebuild_appends_the_flake_ref_where_one_resolves() {
    let op = NixOp::Rebuild(RebuildVerb::Switch);
    assert_eq!(argv(&op, Some(FLAKE)), vec!["nixos-rebuild", "switch", "--flake", FLAKE]);
}

/// A bare `nixos-rebuild switch` is the correct command on a channels
/// host, not a broken one. The flake-only operations dim where no ref
/// resolves; the rebuild verbs stay live, so this must be a command
/// somebody can actually run.
#[test]
fn a_rebuild_without_a_flake_ref_is_still_a_valid_command() {
    assert_eq!(argv(&NixOp::Rebuild(RebuildVerb::Switch), None), vec!["nixos-rebuild", "switch"]);
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
        assert_eq!(argv(&NixOp::Rebuild(verb), None), vec!["nixos-rebuild", expected], "for {verb:?}");
    }
}

/// Activating a specific generation has no high-level form - there is no
/// `nixos-rebuild --to-generation` - so it is two commands. `argv`
/// returns the first; `run` chains the second.
#[test]
fn activating_a_generation_switches_the_profile_first() {
    let op = NixOp::Activate { profile: SYSTEM.to_string(), generation: 427 };
    assert_eq!(argv(&op, None), vec!["nix-env", "-p", SYSTEM, "--switch-generation", "427"]);
}

/// The second command, and it is read *through* the profile symlink the
/// first one has just repointed. Which is exactly why it cannot be
/// folded into `argv`: it is not a second word in one command line, it is
/// a second command that is only correct after the first succeeded.
#[test]
fn activating_a_generation_then_runs_that_generations_activation_script() {
    assert_eq!(activation_argv(SYSTEM), vec!["/nix/var/nix/profiles/system/bin/switch-to-configuration", "switch"]);
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
    assert_eq!(argv(&NixOp::Rollback, Some(FLAKE)), vec!["nixos-rebuild", "switch", "--rollback"]);
}

#[test]
fn a_diff_compares_two_store_paths() {
    let op = NixOp::Diff { from: "/nix/store/aaa-nixos-system".to_string(), to: "/run/current-system".to_string() };
    assert_eq!(argv(&op, None), vec!["nix", "store", "diff-closures", "/nix/store/aaa-nixos-system", "/run/current-system"]);
}

/// `clean` is the standard answer: `nix-collect-garbage
/// --delete-older-than` prunes profile generations *and* collects the
/// store in one step.
#[test]
fn a_clean_prunes_and_collects_in_one_command() {
    assert_eq!(argv(&NixOp::Clean { older_than: "14d".to_string() }, None), vec!["nix-collect-garbage", "--delete-older-than", "14d"]);
}

/// The escape hatch, and a different command from `clean` rather than a
/// variation on it: `nix-env --delete-generations` is the legacy
/// imperative interface and prunes without collecting, so nothing is
/// reclaimed until something else collects the store.
#[test]
fn deleting_generations_is_a_different_command_from_cleaning() {
    let op = NixOp::DeleteGenerations { profile: SYSTEM.to_string(), spec: "+5".to_string() };
    assert_eq!(argv(&op, None), vec!["nix-env", "-p", SYSTEM, "--delete-generations", "+5"]);
}

/// `nix flake update`'s positional arguments are *input names* - measured
/// against nix 2.34.8 on this host, whose synopsis is `nix flake update
/// [option...] inputs...`. So the flake itself goes in `--flake`, and
/// appending the ref positionally the way `flake check` takes it would
/// ask nix to update an input named `/home/user/.dotfiles`.
#[test]
fn a_flake_update_names_the_flake_with_a_flag_not_a_positional() {
    let op = NixOp::FlakeUpdate { input: None };
    assert_eq!(argv(&op, Some(FLAKE)), vec!["nix", "flake", "update", "--flake", FLAKE]);
}

#[test]
fn a_flake_update_of_one_input_names_it_after_the_flake() {
    let op = NixOp::FlakeUpdate { input: Some("nixpkgs".to_string()) };
    assert_eq!(argv(&op, Some(FLAKE)), vec!["nix", "flake", "update", "--flake", FLAKE, "nixpkgs"]);
}

/// A flake-only operation dims where no ref resolves, and a dimmed row is
/// still runnable behind a confirmation - masys never hides an action
/// outright, only marks it. So this must stay a command nix accepts: with
/// no `--flake` it operates on the working directory, which is nix's own
/// default and not something masys invented.
#[test]
fn a_flake_update_without_a_ref_falls_back_to_the_working_directory() {
    assert_eq!(argv(&NixOp::FlakeUpdate { input: None }, None), vec!["nix", "flake", "update"]);
}

/// `nix flake check [option...] flake-url` - measured on nix 2.34.8 here.
/// A positional, unlike `flake update`, because for this sub-command the
/// positional slot *is* the flake.
#[test]
fn a_flake_check_takes_the_ref_as_a_positional() {
    assert_eq!(argv(&NixOp::FlakeCheck, Some(FLAKE)), vec!["nix", "flake", "check", FLAKE]);
}

#[test]
fn a_flake_check_without_a_ref_falls_back_to_the_working_directory() {
    assert_eq!(argv(&NixOp::FlakeCheck, None), vec!["nix", "flake", "check"]);
}

/// The channel operations are the mirror image of the flake ones, and a
/// flake ref means nothing to them: they act on root's channel profile.
#[test]
fn a_channel_update_ignores_any_flake_ref() {
    assert_eq!(argv(&NixOp::ChannelUpdate, Some(FLAKE)), vec!["nix-channel", "--update"]);
}

#[test]
fn a_channel_rollback_ignores_any_flake_ref() {
    assert_eq!(argv(&NixOp::ChannelRollback, Some(FLAKE)), vec!["nix-channel", "--rollback"]);
}

/// `nix search nixpkgs`, which is local. `nh search options` queries
/// search.nixos.org and masys has no network code.
#[test]
fn a_package_search_queries_nixpkgs_locally() {
    let op = NixOp::SearchPackages { query: "ripgrep".to_string() };
    assert_eq!(argv(&op, None), vec!["nix", "search", "nixpkgs", "ripgrep"]);
}

/// `nixos-option` answers what an option is set to *on this host*, which
/// is a narrower and better question for a system tool than searching all
/// of nixpkgs - and it needs no network to answer.
#[test]
fn an_option_lookup_asks_this_host_not_nixpkgs() {
    let op = NixOp::OptionValue { name: "services.openssh.enable".to_string() };
    assert_eq!(argv(&op, None), vec!["nixos-option", "services.openssh.enable"]);
}

/// Standalone home-manager takes a flake the same way the rebuild verbs
/// do, and works without one for a host configured through `home.nix`.
#[test]
fn a_home_switch_appends_the_flake_ref_where_one_resolves() {
    assert_eq!(argv(&NixOp::HomeSwitch, Some(FLAKE)), vec!["home-manager", "switch", "--flake", FLAKE]);
}

#[test]
fn a_home_switch_without_a_flake_ref_is_still_a_valid_command() {
    assert_eq!(argv(&NixOp::HomeSwitch, None), vec!["home-manager", "switch"]);
}

/// Every operation must name a program. An empty argv would reach
/// `Command::new("")`, and the failure would surface as a confusing "No
/// such file or directory" rather than as the missing mapping it is.
#[test]
fn no_operation_produces_an_empty_command() {
    let operations = [
        NixOp::Rebuild(RebuildVerb::Switch),
        NixOp::Activate { profile: SYSTEM.to_string(), generation: 1 },
        NixOp::Rollback,
        NixOp::Diff { from: "a".to_string(), to: "b".to_string() },
        NixOp::Clean { older_than: "14d".to_string() },
        NixOp::DeleteGenerations { profile: SYSTEM.to_string(), spec: "+5".to_string() },
        NixOp::FlakeUpdate { input: None },
        NixOp::FlakeCheck,
        NixOp::ChannelUpdate,
        NixOp::ChannelRollback,
        NixOp::SearchPackages { query: "q".to_string() },
        NixOp::OptionValue { name: "n".to_string() },
        NixOp::HomeSwitch,
    ];
    for op in operations {
        for flake in [None, Some(FLAKE)] {
            let words = argv(&op, flake);
            assert!(!words.is_empty(), "{op:?} with flake {flake:?} produced no command");
            assert!(!words[0].is_empty(), "{op:?} with flake {flake:?} named no program");
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
    let result = run_with(&NixOp::Rebuild(RebuildVerb::Switch), None, |argv| {
        ran.push(argv.to_vec());
        Ok(())
    });

    assert!(result.is_ok());
    assert_eq!(ran, vec![vec!["nixos-rebuild".to_string(), "switch".to_string()]]);
}

/// The order is the whole of the correctness here.
/// `switch-to-configuration` is reached *through* the profile symlink
/// that `nix-env --switch-generation` has just repointed, so a run where
/// the switch failed and the script went ahead anyway would re-activate
/// the generation being replaced and report that it had done what was
/// asked. That rationale was a comment with nothing holding it.
#[test]
fn the_activation_script_never_runs_where_the_profile_switch_failed() {
    let op = NixOp::Activate { profile: SYSTEM.to_string(), generation: 427 };
    let mut ran: Vec<Vec<String>> = Vec::new();
    let result = run_with(&op, None, |argv| {
        ran.push(argv.to_vec());
        Err(MasysError::Command("nix-env exited 1".to_string()))
    });

    assert!(result.is_err());
    assert_eq!(ran.len(), 1, "only the profile switch was attempted: {ran:?}");
    assert_eq!(ran[0][0], "nix-env");
}

/// Both halves, in order, where the first succeeds.
#[test]
fn activating_a_generation_switches_the_profile_then_runs_its_script() {
    let op = NixOp::Activate { profile: SYSTEM.to_string(), generation: 427 };
    let mut ran: Vec<Vec<String>> = Vec::new();
    let result = run_with(&op, None, |argv| {
        ran.push(argv.to_vec());
        Ok(())
    });

    assert!(result.is_ok());
    assert_eq!(ran, vec![argv(&op, None), activation_argv(SYSTEM)]);
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
    let op = NixOp::Activate { profile: SYSTEM.to_string(), generation: 427 };
    let mut ran: Vec<Vec<String>> = Vec::new();
    let failed = run_with(&op, None, |argv| {
        ran.push(argv.to_vec());
        (ran.len() == 1).then_some(()).ok_or_else(|| MasysError::Command("switch-to-configuration switch exited 1".to_string()))
    })
    .unwrap_err()
    .to_string();

    assert_eq!(ran.len(), 2, "both halves were attempted: {ran:?}");
    assert!(failed.contains("generation 427"), "names the generation: {failed}");
    assert!(failed.contains("is now the system profile"), "says the profile already moved: {failed}");
    assert!(failed.contains("activate another generation"), "says how to move it back: {failed}");
    assert!(failed.contains("switch-to-configuration switch exited 1"), "keeps the command's own message: {failed}");
}

/// A failure in the *first* half is reported as itself. Nothing has
/// moved there, so claiming the profile had would be the same fabrication
/// in the other direction.
#[test]
fn a_profile_switch_that_fails_is_never_reported_as_a_moved_profile() {
    let op = NixOp::Activate { profile: SYSTEM.to_string(), generation: 427 };
    let failed = run_with(&op, None, |_| Err(MasysError::Command("nix-env exited 1".to_string()))).unwrap_err().to_string();

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
    assert!(lines.iter().all(|line| line.len() <= 110), "each line fits an ordinary frame: {lines:?}");
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
    assert!(!outcome(status(9)).contains("exited"), "and it did not exit");
}

/// `$NH_FLAKE` first, because it is the variable actually set on real
/// NixOS hosts. Reading a variable another tool defines is not a
/// dependency on that tool - masys never runs `nh`.
#[test]
fn nh_flake_wins_over_the_plain_flake_variable() {
    assert_eq!(flake_ref_from(Some("/a"), Some("/b"), false), Some("/a".to_string()));
}

#[test]
fn the_plain_flake_variable_is_used_where_nh_flake_is_unset() {
    assert_eq!(flake_ref_from(None, Some("/b"), false), Some("/b".to_string()));
}

/// `/etc/nixos` is the last source, and only where it actually holds a
/// `flake.nix`. Every NixOS host has an `/etc/nixos`; only some have a
/// flake in it, and naming the directory regardless would hand
/// `nixos-rebuild` a ref that cannot evaluate.
#[test]
fn etc_nixos_is_the_last_resort_and_only_where_it_holds_a_flake() {
    assert_eq!(flake_ref_from(None, None, true), Some("/etc/nixos".to_string()));
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
    assert_eq!(flake_ref_from(Some(""), Some("/b"), false), Some("/b".to_string()));
    assert_eq!(flake_ref_from(Some("   "), None, true), Some("/etc/nixos".to_string()));
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

    assert_eq!(nixos_config_from(Some("nixpkgs=flake:nixpkgs:nixos-config=/srv/cfg.nix"), None), Some("/srv/cfg.nix".to_string()));
    assert_eq!(
        nixos_config_from(Some("nixpkgs=flake:nixpkgs:nixos-config=/srv/cfg.nix"), Some("/etc/nixos/configuration.nix")),
        Some("/srv/cfg.nix".to_string()),
        "the explicit entry wins"
    );
    assert_eq!(
        nixos_config_from(Some("nixpkgs=flake:nixpkgs"), Some("/etc/nixos/configuration.nix")),
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

    assert_eq!(nixos_config_from(Some("nixpkgs=flake:nixpkgs:/nix/var/nix/profiles/per-user/root/channels"), None), None);
    assert_eq!(nixos_config_from(None, None), None);
    assert_eq!(nixos_config_from(Some("nixos-config="), None), None, "an empty entry is not a path");
}
