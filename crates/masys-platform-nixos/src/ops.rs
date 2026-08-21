//! What each operation actually runs, and the one place that decides it.
//!
//! Split the way `links.rs` splits parsing from reading and `reboot_state`
//! splits comparing from looking: [`argv`] is pure and carries the whole
//! operation mapping, [`run_with`] carries the sequencing and takes its
//! spawner as an argument, and [`run`] is only the two reads and the real
//! process. So all of it is assertable on a machine that is not NixOS -
//! `tests/ops.rs` never executes a command, which for a module whose
//! commands activate system configurations is not a convenience but the
//! only safe way to test it.
//!
//! **Nothing here invokes `nh`.** `nh` was the reference for *which*
//! operations are worth offering and nothing more; every command below is
//! `nixos-rebuild`, `nix`, `nix-env`, `nix-channel`, `nixos-option`,
//! `nix-collect-garbage` or `home-manager`. A published crate cannot
//! require somebody else's tool to be installed before its buttons work.

use std::os::unix::process::ExitStatusExt;

use masys_domain::declarative::{NixOp, RebuildVerb};
use masys_domain::error::MasysError;

/// The last place a flake reference is looked for, and only where it
/// holds a `flake.nix`. Every NixOS host has this directory; only some
/// have a flake in it.
const ETC_NIXOS: &str = "/etc/nixos";

/// One command line, as owned words.
///
/// `Vec<String>` rather than `Vec<&str>` because several arms interpolate
/// - a generation number, a profile path joined to `bin/…` - and a
/// borrowed variant would need every caller to hold those temporaries
/// alive. The commands run at most once per keypress; the allocations are
/// free against a human.
fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

/// The `nixos-rebuild` sub-command a [`RebuildVerb`] names.
///
/// A match rather than lowercasing the variant's `Debug`, because the
/// spellings are not a mechanical transform of the names: `DryActivate`
/// is `dry-activate`. A derive would be right four times out of five and
/// wrong on the fifth in a way nothing would notice until the command
/// failed on somebody's host.
fn rebuild_verb(verb: RebuildVerb) -> &'static str {
    match verb {
        RebuildVerb::Switch => "switch",
        RebuildVerb::Boot => "boot",
        RebuildVerb::Test => "test",
        RebuildVerb::Build => "build",
        RebuildVerb::DryActivate => "dry-activate",
    }
}

/// The command one operation runs, given the flake reference that
/// resolved on this host - or `None` where none did.
///
/// **`None` never makes a command invalid.** A bare `nixos-rebuild
/// switch` is the correct command on a channels host, so the rebuild
/// verbs simply omit `--flake`; the two flake-only operations fall back
/// to nix's own default of the working directory, because a dimmed row is
/// still runnable behind a confirmation - masys marks an action it
/// doubts, it never hides one.
///
/// [`NixOp::Activate`] is the one operation this cannot fully express: it
/// is two commands, and only the first is here. See [`activation_argv`].
pub fn argv(op: &NixOp, flake: Option<&str>) -> Vec<String> {
    match op {
        NixOp::Rebuild(verb) => {
            let mut command = words(&["nixos-rebuild", rebuild_verb(*verb)]);
            command.extend(flake.map(|flake| words(&["--flake", flake])).unwrap_or_default());
            command
        }
        // The first half only. The number is formatted rather than passed
        // as a word because `nix-env` takes the generation as a decimal
        // argument, which is what `u64` already is.
        NixOp::Activate { profile, generation } => words(&["nix-env", "-p", profile, "--switch-generation", &generation.to_string()]),
        // A flag on an action, not an action of its own. `nixos-rebuild
        // --help` on this host lists the actions as
        // `{switch,boot,test,build,edit,repl,dry-build,dry-run,dry-activate,
        // build-image,build-vm,build-vm-with-bootloader,list-generations}`
        // with no `rollback` among them, and documents `--rollback`
        // separately: "Instead of building a new configuration as
        // specified by /etc/nixos/configuration.nix, roll back to the
        // previous configuration." A bare `nixos-rebuild rollback` prints
        // usage and exits.
        //
        // Paired with `switch` rather than `boot` because a rollback means
        // running the previous configuration now; `boot` would leave the
        // current one running until a reboot, which is the opposite of
        // what somebody pressing rollback wants.
        //
        // No `--flake`, deliberately, where the five rebuild verbs take
        // one: `--rollback` builds nothing. It activates a generation that
        // already exists on disk, so the configuration it would be built
        // from is not consulted and naming it would be theatre.
        NixOp::Rollback => words(&["nixos-rebuild", "switch", "--rollback"]),
        NixOp::Diff { from, to } => words(&["nix", "store", "diff-closures", from, to]),
        NixOp::Clean { older_than } => words(&["nix-collect-garbage", "--delete-older-than", older_than]),
        NixOp::DeleteGenerations { profile, spec } => words(&["nix-env", "-p", profile, "--delete-generations", spec]),
        // `--flake`, not a positional, and the difference is load-bearing.
        // `nix flake update`'s synopsis is `nix flake update [option...]
        // inputs...` - measured against nix 2.34.8 on this host - so its
        // positional slot holds *input names*. Passing the ref there the
        // way `flake check` takes it would ask nix to update an input
        // called `/home/user/.dotfiles`. Omitted where no ref resolved,
        // which leaves nix's own default of the working directory.
        NixOp::FlakeUpdate { input } => {
            let mut command = words(&["nix", "flake", "update"]);
            command.extend(flake.map(|flake| words(&["--flake", flake])).unwrap_or_default());
            command.extend(input.iter().map(|input| input.to_string()));
            command
        }
        // A positional here, unlike `flake update` directly above:
        // `nix flake check [option...] flake-url` - same nix, same
        // reading - so for this sub-command the positional slot *is* the
        // flake.
        NixOp::FlakeCheck => {
            let mut command = words(&["nix", "flake", "check"]);
            command.extend(flake.map(|flake| vec![flake.to_string()]).unwrap_or_default());
            command
        }
        // The channel operations act on root's channel profile, so a
        // flake reference means nothing to them - they are the mirror
        // image of the two above, dimmed on a flake host by the same rule
        // that dims those on a channels host.
        NixOp::ChannelUpdate => words(&["nix-channel", "--update"]),
        NixOp::ChannelRollback => words(&["nix-channel", "--rollback"]),
        NixOp::SearchPackages { query } => words(&["nix", "search", "nixpkgs", query]),
        NixOp::OptionValue { name } => words(&["nixos-option", name]),
        NixOp::HomeSwitch => {
            let mut command = words(&["home-manager", "switch"]);
            command.extend(flake.map(|flake| words(&["--flake", flake])).unwrap_or_default());
            command
        }
    }
}

/// The second command of [`NixOp::Activate`]: the activation script of
/// whatever the profile now points at.
///
/// Separate from [`argv`] because it is not a longer command line, it is
/// a *second command* that is only correct after the first one has
/// succeeded. `<profile>/bin/switch-to-configuration` is reached through
/// the profile symlink that `nix-env --switch-generation` has just
/// repointed, so running the two out of order re-activates the generation
/// being replaced - silently, and reporting success.
///
/// This two-step is not masys reaching past the supported interface, it
/// is the supported interface: `nixos-rebuild` offers `--rollback`, which
/// goes to the previous generation only, and `--specialisation`. There is
/// no `--to-generation`.
pub fn activation_argv(profile: &str) -> Vec<String> {
    words(&[&format!("{profile}/bin/switch-to-configuration"), "switch"])
}

/// The flake reference to build from, given the sources in the order the
/// design fixes them.
///
/// Takes its readings rather than performing them, the shape
/// `reboot_state` already uses in this crate: the *ordering* is the part
/// with a decision in it, and this way it is testable with neither
/// variable set and no `/etc/nixos` on the machine running the test.
///
/// `$NH_FLAKE` is read ahead of `$FLAKE` because it is the one actually
/// set on real NixOS hosts - it is set on this development host, and is
/// the only way the configuration here is reachable at all. Reading a
/// variable another tool defines is not a dependency on that tool, and
/// costs one `env::var`; masys still never runs `nh`.
///
/// masys's own `config.toml` sits ahead of all three in the design's
/// order and is absent from this signature. Loading it belongs to the
/// `masys` binary crate, which owns `~/.config/masys/config.toml`, and
/// this adapter cannot reach it - a parameter for a source with no
/// producer would be exactly the dead weight this workspace has been
/// reviewed for once already. It slots in as a fourth argument, ahead of
/// `nh_flake`, on the day something can supply it.
///
/// An empty or blank value is not a reference. A shell that exported
/// `NH_FLAKE=` meaning to unset it would otherwise produce `--flake ''`,
/// which fails on the empty string rather than falling through to the
/// next source - and would stop a channels host that happens to export it
/// from offering the bare `nixos-rebuild switch` that works there.
pub fn flake_ref_from(nh_flake: Option<&str>, flake: Option<&str>, etc_nixos_has_flake: bool) -> Option<String> {
    let named = [nh_flake, flake].into_iter().flatten().map(str::trim).find(|value| !value.is_empty());
    named.map(str::to_string).or_else(|| etc_nixos_has_flake.then(|| ETC_NIXOS.to_string()))
}

/// This host's flake reference, performing the reads [`flake_ref_from`]
/// orders.
///
/// `var`, not `var_os`: a reference has to become a command-line argument
/// as a `&str`, so a non-UTF-8 value could not be used even if it were
/// read. Treating it as unset falls through to the next source, which is
/// the conservative direction - the alternative is a lossy conversion
/// that hands `nixos-rebuild` a path pointing somewhere else.
pub fn flake_ref() -> Option<String> {
    let nh_flake = std::env::var("NH_FLAKE").ok();
    let flake = std::env::var("FLAKE").ok();
    flake_ref_from(nh_flake.as_deref(), flake.as_deref(), std::path::Path::new(ETC_NIXOS).join("flake.nix").exists())
}

/// Where `<nixos-config>` resolves, taking its readings rather than
/// performing them - `flake_ref_from`'s shape, for its reason.
///
/// Two sources, in `nix`'s own lookup order: an explicit
/// `nixos-config=<path>` entry in `NIX_PATH`, then
/// `/etc/nixos/configuration.nix`, which is the file a channels install
/// puts there and a flake install usually does not.
///
/// A bare `NIX_PATH` directory that happens to contain a `nixos-config`
/// file would also satisfy nix and is not looked for here. That is a
/// deliberate under-report: the cost is marking a row that would have
/// worked, and a mark never blocks - `option value` still runs from the
/// popup. The opposite error leaves a row live that fails before it
/// evaluates anything.
pub fn nixos_config_from(nix_path: Option<&str>, etc_nixos_config: Option<&str>) -> Option<String> {
    let named = nix_path
        .into_iter()
        .flat_map(|path| path.split(':'))
        .filter_map(|entry| entry.strip_prefix("nixos-config="))
        .map(str::trim)
        .find(|value| !value.is_empty());
    named.map(str::to_string).or_else(|| etc_nixos_config.map(str::to_string))
}

/// This host's `<nixos-config>`, performing the reads
/// [`nixos_config_from`] orders.
pub fn nixos_config() -> Option<String> {
    let configuration = std::path::Path::new(ETC_NIXOS).join("configuration.nix");
    nixos_config_from(
        std::env::var("NIX_PATH").ok().as_deref(),
        configuration.is_file().then(|| configuration.to_string_lossy().into_owned()).as_deref(),
    )
}

/// Perform an operation, with the terminal already handed back.
///
/// The reads and the spawning; [`run_with`] holds every decision.
pub fn run(op: &NixOp) -> Result<(), MasysError> {
    run_with(op, flake_ref().as_deref(), spawn)
}

/// The chain one operation runs, against a caller-supplied spawner.
///
/// Takes the spawner rather than performing the spawn, the shape
/// [`crate::reboot_state`] already uses in this crate for its
/// readings: what is decided here is an *ordering* and an error message,
/// and both are testable this way without a single process being created.
/// `run` is then thin enough to read in one line. Before this, nothing
/// tested any of it - a module whose commands activate system
/// configurations had its one sequencing rule held by a comment alone.
///
/// [`NixOp::Activate`] is the only operation that runs two commands, and
/// the chain is here rather than inside [`argv`] so that `argv` stays a
/// function from one operation to one command line - the property every
/// argv test rests on. The order is not incidental: `?` on the first
/// spawn means the activation script runs only where the profile switch
/// succeeded, because activating "generation 427" through a profile still
/// pointing at 428 would re-activate 428 and report that it had done what
/// was asked.
pub fn run_with(op: &NixOp, flake: Option<&str>, mut spawn: impl FnMut(&[String]) -> Result<(), MasysError>) -> Result<(), MasysError> {
    spawn(&argv(op, flake))?;
    if let NixOp::Activate { profile, generation } = op {
        spawn(&activation_argv(profile)).map_err(|error| switched_but_not_activated(*generation, &error))?;
    }
    Ok(())
}

/// What to report when an activation's second command fails after its
/// first has succeeded.
///
/// The profile symlink now points at `generation`, `/run/current-system`
/// still points at whatever was running, and no bootloader entry was
/// installed. The failing command's own message says which half failed;
/// it cannot say that the other half already happened, and that is the
/// fact the operator has to act on.
///
/// Without it the screen is indistinguishable from a successful
/// `nixos-rebuild boot` - a generation staged and waiting for a reboot.
/// The reboot row does not correct the impression either:
/// [`crate::reboot_state`] compares `/run/booted-system` against
/// `/run/current-system`, and `nix-env --switch-generation` touches
/// neither, so it goes on reporting exactly what it reported before. The
/// profile having moved is carried by nothing else on the screen.
///
/// Recoverable, and said so: activating another generation moves the
/// profile back, which is the same keypress that got here. Three lines
/// because `masys_render` draws a `StatusLine::Error` line by line and
/// truncates each at the frame's width - one sentence this long would
/// lose its tail rather than wrap, and the tail is the recovery.
pub fn switched_but_not_activated(generation: u64, error: &MasysError) -> MasysError {
    MasysError::Command(format!(
        "generation {generation} is now the system profile, but activating it failed\n\
         nothing runs from it and no boot entry was installed; activate another generation to move it back\n\
         {error}"
    ))
}

/// Runs one command line with stdio inherited.
///
/// `status()` with stdio inherited, exactly like
/// `masys_systemd::actions::systemctl_interactive` and for the same
/// reason: the command owns the screen. A rebuild streams for minutes and
/// `nix store diff-closures` pages, so capturing stdout would hold all of
/// that in memory and show none of it until the command had finished. The
/// caller is responsible for having released the terminal first; there is
/// nothing here that can check.
///
/// The error carries the command and its exit code, not the command's own
/// message - stderr went to the terminal and is already in front of the
/// operator, which is the whole point of inheriting it. What the operator
/// cannot see there is *which* of the two commands an `Activate` failed
/// in, so the joined argv is what this adds.
fn spawn(argv: &[String]) -> Result<(), MasysError> {
    // Every arm of `argv` starts with a program name and
    // `no_operation_produces_an_empty_command` pins that, so this is
    // unreachable today. An `Err` rather than a panic anyway: the cost of
    // being wrong here is masys aborting mid-operation, and a future arm
    // added wrongly should surface as a refused operation.
    let Some((program, arguments)) = argv.split_first() else {
        return Err(MasysError::Command("no command to run".to_string()));
    };
    let status = std::process::Command::new(program)
        .args(arguments)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(MasysError::Io)?;
    if status.success() {
        return Ok(());
    }
    Err(MasysError::Command(format!("{} {}", argv.join(" "), outcome(status))))
}

/// How a command ended, in the words of the thing that ended it.
///
/// `code()` is `None` for a child that a signal killed, and
/// `unwrap_or(-1)` there produced "`nixos-rebuild switch` exited -1".
/// That is a fabricated reading wearing a number instead of a zero: no
/// process exits -1, `$?` never shows it, and an operator searching for
/// what returns -1 is searching for something that did not happen.
/// `signal()` is `Some` in exactly the case `code()` is `None`, so
/// nothing has to be invented.
///
/// A signalled child is not hypothetical, and it is *not* ctrl-c. The
/// comment this replaces claimed "a rebuild interrupted with ctrl-c is
/// the ordinary case", which is wrong in a way nothing checked: the
/// binary calls `ratatui::restore` before handing over, so the child runs
/// with the terminal out of raw mode, and ctrl-c raises SIGINT against
/// the whole foreground process group. masys installs no handler - there
/// is no `sigaction` anywhere in the workspace - so masys dies alongside
/// the child and never reaches this line. What does reach it is the OOM
/// killer taking a `nix` build, an operator's `kill` from another
/// terminal, or an activation script segfaulting.
///
/// Neither a code nor a signal cannot happen for a `status()` that
/// returned `Ok` on Linux, where a reaped child either exited or was
/// signalled. Said as the unknown it is rather than folded into either,
/// which is the whole point of not writing `-1`.
pub fn outcome(status: std::process::ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exited {code}"),
        (None, Some(signal)) => format!("killed by signal {signal}"),
        (None, None) => "ended without an exit status or a signal".to_string(),
    }
}
