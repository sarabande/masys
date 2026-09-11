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
/// `Vec<String>` rather than `Vec<&str>` because several arms
/// interpolate - a generation number, a profile path joined to `bin/…` -
/// and a borrowed variant would need every caller to hold those
/// temporaries alive. The commands run at most once per keypress; the
/// allocations are free against a human.
fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

/// How `nixos-rebuild` should raise privilege for the commands that
/// activate.
///
/// masys runs unprivileged, and the three rebuild verbs that write
/// (`switch`, `boot` and `test`) are where that used to cost the most:
/// the build runs for minutes and is then refused at activation, which
/// was the first thing the README's Status section named.
///
/// `sudo` where the host has it, `run0` where it does not.
///
/// **`sudo` prompts on the terminal. `run0` prompts wherever the session's
/// polkit agent happens to be, which is not the terminal and may be
/// nowhere at all.**
///
/// That decides it, and it decides it against the earlier preference.
/// These operations are the ones that take the screen: masys hands the
/// terminal over so a rebuild can stream its own output for as long as it
/// runs, and the whole point of handing it over is that what happens next
/// happens where the operator is looking. `sudo` asks there. `run0`
/// delegates to whatever agent the *session* registered - on a GNOME
/// desktop that is a `gnome-shell` dialog somewhere behind the terminal,
/// and over SSH or on a bare TTY there is no agent to ask at all.
/// Measured on the development host: `Authorization Manager Agent Helper
/// (PID 2778/UID 1000)`, `gnome-shell`'s agent, put the password prompt
/// in a window while masys had just given up the screen for the rebuild's
/// output. `run0` offers no way to force a terminal agent - its options
/// are `--no-ask-password`, `--pty` and `--pty-late`, none of which
/// changes who asks.
///
/// Issue #13 rates `sudo` the weaker of the two because it depends on a
/// sudoers policy masys cannot see. That is true, and polkit is equally a
/// policy masys cannot see; the argument was never about knowability. It
/// was that polkit is the mechanism masys already relies on for unit
/// verbs - and that still holds *for unit verbs*, which are instant, go
/// through `systemctl` and never take the terminal. A dialog is a fine
/// place to authorise those. It is the wrong place to authorise something
/// the operator is watching a terminal for.
///
/// So the preference is not "sudo is better" but "ask where the operator
/// is looking", and the two mechanisms differ on exactly that.
/// `NIX_SUDOOPTS` is the escape hatch `nixos-rebuild` documents for
/// tuning sudo, and masys passes the environment through untouched.
///
/// No `--ask-elevate-password`. masys hands the terminal over for the
/// whole rebuild - the design's `Flow::Suspend` - so `sudo` prompts on a
/// real terminal the operator is looking at. Reading the password
/// ourselves to feed it in would be masys handling a root password for no
/// gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Elevate {
    /// Nothing to add: masys is already root, this `nixos-rebuild` has no
    /// such flag, this host offers neither method, or the verb activates
    /// nothing.
    #[default]
    No,
    /// `--elevate run0`, systemd's polkit-based elevation.
    Run0,
    /// `--elevate sudo`, which prefixes the activation commands with
    /// `sudo`.
    Sudo,
}

impl Elevate {
    /// The word for this method, or `None` where there is nothing to add.
    ///
    /// One word, two mechanisms. `nixos-rebuild` takes it as the value of
    /// `--elevate` and raises privilege itself; everything else has no
    /// such flag and is *run through* the program of the same name. Which
    /// of the two applies is the operation's to know, so each arm of
    /// [`argv`] uses this the way its own tool supports.
    fn word(self) -> Option<&'static str> {
        match self {
            Elevate::No => None,
            Elevate::Run0 => Some("run0"),
            Elevate::Sudo => Some("sudo"),
        }
    }

    /// The command, run through this method.
    ///
    /// A prefix rather than a flag, for the tools that have none.
    /// `run0 nix-env …` raises `org.freedesktop.systemd1`'s polkit
    /// action, which is the same one a unit verb raises through
    /// `systemctl` - so an operator granted masys's unit verbs is granted
    /// this by the rule they already hold.
    fn wrapping(self, command: Vec<String>) -> Vec<String> {
        match self.word() {
            Some(method) => words(&[method]).into_iter().chain(command).collect(),
            None => command,
        }
    }
}

/// The elevation this host can actually perform, preferring the one that
/// asks on the terminal.
///
/// Both arguments are "is this program on `PATH`", which is as far as a
/// look can get: whether the *policy* will let this operator through is
/// not knowable without trying, for either method. What is knowable is
/// which mechanism exists, and passing a method the host does not have
/// would turn a refusal at activation into a "command not found" at
/// activation - the same minutes gone, a stranger message.
pub fn offered(has_run0: bool, has_sudo: bool) -> Elevate {
    match (has_sudo, has_run0) {
        (true, _) => Elevate::Sudo,
        (false, true) => Elevate::Run0,
        (false, false) => Elevate::No,
    }
}

/// Whether a program is on `PATH`.
///
/// Takes the existence check as an argument, the shape
/// [`crate::reboot_state`] already uses in this crate: the part with a
/// decision in it is testable without a filesystem.
pub fn on_path(program: &str, path: &str, exists: impl Fn(&std::path::Path) -> bool) -> bool {
    path.split(':')
        .filter(|entry| !entry.is_empty())
        .any(|entry| exists(&std::path::Path::new(entry).join(program)))
}

/// Whether this host's `nixos-rebuild` accepts `--elevate`.
///
/// Measured off its own `--help`, because the flag is nixos-rebuild-ng's
/// and the classic shell script has nothing like it. A host still running
/// the old one would take the flag, print usage and exit - after the
/// build, which is the failure this whole thing exists to remove.
///
/// A string predicate rather than a version test: `nixos-rebuild
/// --version` prints a usage banner on this host rather than a version,
/// and the question is what the binary in front of us accepts, not which
/// implementation somebody believes it is.
pub fn accepts_elevate(help: &str) -> bool {
    help.contains("--elevate")
}

/// The elevation one operation wants, given what masys measured about
/// the host and about itself.
///
/// `None` for `accepts` means the probe did not come back, and it is
/// treated as "no": passing a flag masys did not confirm would trade a
/// refusal at activation for a usage error at activation, which is the
/// same minutes wasted and a stranger message.
///
/// Root needs nothing, and neither does `build`, which is the one verb
/// that stops at a store path.
///
/// `dry-activate` *is* elevated, though it changes nothing, and the
/// reason is measured rather than assumed. It reaches `systemd-run …
/// switch-to-configuration dry-activate` like the three that write, and
/// unprivileged it is refused there - after the build, which is minutes
/// gone for an answer it never gives. An earlier version of this function
/// left it out on the argument that prompting for a password to preview
/// something teaches the operator to type one without reading. That
/// argument is real and it loses to this one: the alternative is not
/// "no prompt", it is "no answer, five minutes later".
pub fn elevation(
    op: &NixOp,
    euid: u32,
    accepts: Option<bool>,
    offered: Elevate,
    writable: impl Fn(&str) -> bool,
) -> Elevate {
    // Root needs nothing, whatever the operation.
    if euid == 0 {
        return Elevate::No;
    }
    match op {
        // The three that write a profile, and have no elevation flag
        // between them: `nix-env`'s synopsis carries none and
        // `nix-collect-garbage`'s is `[--delete-old] [-d]
        // [--delete-older-than period] [--max-freed bytes] [--dry-run]`.
        // So they are run *through* the method rather than told about it.
        //
        // Asked of the profile rather than of the uid, because not every
        // profile is root's. `~/.local/state/nix/profiles` belongs to the
        // operator, so deleting a home-manager generation needs nothing -
        // and prompting for root to remove your own generation would be
        // masys asking for a privilege the act does not use.
        NixOp::Activate { profile, .. } | NixOp::DeleteGenerations { profile, .. } => {
            match writable(profile) {
                true => Elevate::No,
                false => offered,
            }
        }
        // `nix-collect-garbage` "looks in a few locations, and acts on
        // all profiles it finds there" - the system profile among them,
        // which is never the operator's. There is no one path to ask
        // about, so this is asked of the uid.
        NixOp::Clean { .. } => offered,
        _ => rebuild_elevation(op, accepts, offered),
    }
}

/// The `--elevate` value `nixos-rebuild` should be given, or `No`.
///
/// Separate from the wrapper above because it is a different mechanism
/// with a different precondition: this one asks whether *the binary in
/// front of us* accepts the flag at all.
fn rebuild_elevation(op: &NixOp, accepts: Option<bool>, offered: Elevate) -> Elevate {
    // `--rollback` changes which configuration `nixos-rebuild` builds
    // from, not whether it activates, so it wants what `switch` wants.
    // Every other operation drives `nix`, `nix-env`, `nix-channel` or
    // `nixos-option`, and none of those has an elevation flag at all -
    // which is exactly why masys marks the rows that write a root-owned
    // profile rather than offering to raise privilege for them.
    let verb = match op {
        NixOp::Rebuild(verb) => *verb,
        NixOp::Rollback | NixOp::Upgrade => RebuildVerb::Switch,
        // Everything else, `Repl` included, activates nothing and so has
        // nothing to elevate for. `Repl` had an arm of its own here
        // saying exactly that, immediately above this one and returning
        // the same value - deleting it changed no behaviour and no test,
        // which is what made it worth deleting.
        _ => return Elevate::No,
    };
    let activates = !matches!(verb, RebuildVerb::Build);
    match accepts == Some(true) && activates {
        true => offered,
        false => Elevate::No,
    }
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
pub fn argv(op: &NixOp, flake: Option<&str>, elevate: Elevate) -> Vec<String> {
    match op {
        NixOp::Rebuild(verb) => {
            let mut command = words(&["nixos-rebuild", rebuild_verb(*verb)]);
            command.extend(
                flake
                    .map(|flake| words(&["--flake", flake]))
                    .unwrap_or_default(),
            );
            // Last, so the sub-command and its flake stay where every
            // reading of this command line expects them, and so a host
            // that elevates and one that does not differ by a suffix
            // rather than by a rearrangement.
            if let Some(method) = elevate.word() {
                command.extend(words(&["--elevate", method]));
            }
            command
        }
        // The first half only. The number is formatted rather than passed
        // as a word because `nix-env` takes the generation as a decimal
        // argument, which is what `u64` already is.
        NixOp::Activate {
            profile,
            generation,
        } => elevate.wrapping(words(&[
            "nix-env",
            "-p",
            profile,
            "--switch-generation",
            &generation.to_string(),
        ])),
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
        // Elevated on the same terms as `switch`, which is what it is:
        // `--rollback` changes which command `nixos-rebuild` builds from,
        // not whether it activates. A rollback refused at activation
        // wastes the same minutes.
        NixOp::Rollback => {
            let mut command = words(&["nixos-rebuild", "switch", "--rollback"]);
            if let Some(method) = elevate.word() {
                command.extend(words(&["--elevate", method]));
            }
            command
        }
        // Also a flag on `switch`, and also without `--flake` - but for
        // the opposite reason to `--rollback`. That one builds nothing, so
        // naming a configuration would be theatre. This one builds
        // normally; what it cannot have is a flake *reference*, because
        // `--upgrade` updates channels and a flake host has none. The row
        // dims there rather than emitting this command at all, so the
        // absence here is a second line of defence rather than the rule.
        NixOp::Upgrade => {
            let mut command = words(&["nixos-rebuild", "switch", "--upgrade"]);
            if let Some(method) = elevate.word() {
                command.extend(words(&["--elevate", method]));
            }
            command
        }
        // An action like the rebuild verbs, so it takes the flake
        // reference; unlike them it never elevates. `repl` evaluates and
        // opens a prompt - it builds no system and activates nothing, so
        // there is no activation for polkit to refuse, and handing an
        // operator root inside an interactive session is a privilege they
        // did not ask for and would then be holding.
        NixOp::Repl => {
            let mut command = words(&["nixos-rebuild", "repl"]);
            command.extend(
                flake
                    .map(|flake| words(&["--flake", flake]))
                    .unwrap_or_default(),
            );
            command
        }
        // Both build-image forms and build-vm take `--flake` where one
        // resolves, exactly as the rebuild verbs do: they build *this*
        // configuration, and on a flake host that is what names it.
        NixOp::BuildVm => {
            let mut command = words(&["nixos-rebuild", "build-vm"]);
            command.extend(
                flake
                    .map(|flake| words(&["--flake", flake]))
                    .unwrap_or_default(),
            );
            command
        }
        // No `--image-variant`, which is what makes it print the list
        // rather than build one. nixos-rebuild(8): "run without any
        // options to get a list of available variants".
        NixOp::ListImageVariants => {
            let mut command = words(&["nixos-rebuild", "build-image"]);
            command.extend(
                flake
                    .map(|flake| words(&["--flake", flake]))
                    .unwrap_or_default(),
            );
            command
        }
        NixOp::BuildImage { variant } => {
            let mut command = words(&["nixos-rebuild", "build-image", "--image-variant", variant]);
            command.extend(
                flake
                    .map(|flake| words(&["--flake", flake]))
                    .unwrap_or_default(),
            );
            command
        }
        NixOp::Diff { from, to } => words(&["nix", "store", "diff-closures", from, to]),
        NixOp::Clean { older_than } => elevate.wrapping(words(&[
            "nix-collect-garbage",
            "--delete-older-than",
            older_than,
        ])),
        NixOp::DeleteGenerations { profile, spec } => elevate.wrapping(words(&[
            "nix-env",
            "-p",
            profile,
            "--delete-generations",
            spec,
        ])),
        // `--flake`, not a positional, and the difference is load-bearing.
        // `nix flake update`'s synopsis is `nix flake update [option...]
        // inputs...` - measured against nix 2.34.8 on this host - so its
        // positional slot holds *input names*. Passing the ref there the
        // way `flake check` takes it would ask nix to update an input
        // called `/home/user/.dotfiles`. Omitted where no ref resolved,
        // which leaves nix's own default of the working directory.
        NixOp::FlakeUpdate { input } => {
            let mut command = words(&["nix", "flake", "update"]);
            command.extend(
                flake
                    .map(|flake| words(&["--flake", flake]))
                    .unwrap_or_default(),
            );
            command.extend(input.iter().map(|input| input.to_string()));
            command
        }
        // A positional here, unlike `flake update` directly above:
        // `nix flake check [option...] flake-url` - same nix, same
        // reading - so for this sub-command the positional slot *is* the
        // flake.
        NixOp::FlakeCheck => {
            let mut command = words(&["nix", "flake", "check"]);
            command.extend(
                flake
                    .map(|flake| vec![flake.to_string()])
                    .unwrap_or_default(),
            );
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
            command.extend(
                flake
                    .map(|flake| words(&["--flake", flake]))
                    .unwrap_or_default(),
            );
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
pub fn activation_argv(profile: &str, elevate: Elevate) -> Vec<String> {
    elevate.wrapping(words(&[
        &format!("{profile}/bin/switch-to-configuration"),
        "switch",
    ]))
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
pub fn flake_ref_from(
    nh_flake: Option<&str>,
    flake: Option<&str>,
    etc_nixos_has_flake: bool,
) -> Option<String> {
    let named = [nh_flake, flake]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty());
    named
        .map(str::to_string)
        .or_else(|| etc_nixos_has_flake.then(|| ETC_NIXOS.to_string()))
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
    flake_ref_from(
        nh_flake.as_deref(),
        flake.as_deref(),
        std::path::Path::new(ETC_NIXOS).join("flake.nix").exists(),
    )
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
    named
        .map(str::to_string)
        .or_else(|| etc_nixos_config.map(str::to_string))
}

/// This host's `<nixos-config>`, performing the reads
/// [`nixos_config_from`] orders.
pub fn nixos_config() -> Option<String> {
    let configuration = std::path::Path::new(ETC_NIXOS).join("configuration.nix");
    nixos_config_from(
        std::env::var("NIX_PATH").ok().as_deref(),
        configuration
            .is_file()
            .then(|| configuration.to_string_lossy().into_owned())
            .as_deref(),
    )
}

/// Perform an operation, with the terminal already handed back.
///
/// The reads and the spawning; [`run_with`] holds every decision.
pub fn run(op: &NixOp) -> Result<(), MasysError> {
    let elevate = elevation(
        op,
        euid(),
        accepts_elevate_here(),
        offered_here(),
        crate::profiles::writable_dir,
    );
    run_with(op, flake_ref().as_deref(), elevate, spawn)
}

/// Which elevation this host offers, from `PATH`.
pub fn offered_here() -> Elevate {
    let path = std::env::var("PATH").unwrap_or_default();
    offered(
        on_path("run0", &path, |candidate| candidate.exists()),
        on_path("sudo", &path, |candidate| candidate.exists()),
    )
}

/// This process's effective uid.
pub(crate) fn euid() -> u32 {
    // Infallible by POSIX: `geteuid` has no error return.
    unsafe { libc::geteuid() }
}

/// Asks this host's `nixos-rebuild` what it accepts, or `None` where the
/// question could not be put.
///
/// Run here, at the moment an operation is about to start, rather than
/// once at startup. It costs one exec - 182 ms on the development host -
/// against a rebuild that runs for minutes, so there is nothing to save
/// by caching it and nothing to keep honest afterwards: a cached answer
/// would go on being reported across a `nixos-rebuild` that was replaced
/// underneath the session.
///
/// Nothing else in masys spawns a process to answer a question, and this
/// is the exception rather than a new habit: the flag is not recorded
/// anywhere on the filesystem, and the alternative is passing it blind.
fn accepts_elevate_here() -> Option<bool> {
    let output = std::process::Command::new("nixos-rebuild")
        .arg("--help")
        .output()
        .ok()?;
    // `--help` is expected to exit zero; a non-zero exit means this is
    // not a `nixos-rebuild` masys can reason about, and `None` says so
    // rather than reading whatever it printed.
    if !output.status.success() {
        return None;
    }
    Some(accepts_elevate(&String::from_utf8_lossy(&output.stdout)))
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
pub fn run_with(
    op: &NixOp,
    flake: Option<&str>,
    elevate: Elevate,
    mut spawn: impl FnMut(&[String]) -> Result<(), MasysError>,
) -> Result<(), MasysError> {
    spawn(&argv(op, flake, elevate))?;
    if let NixOp::Activate {
        profile,
        generation,
    } = op
    {
        spawn(&activation_argv(profile, elevate))
            .map_err(|error| switched_but_not_activated(*generation, &error))?;
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
    Err(MasysError::Command(format!(
        "{} {}",
        argv.join(" "),
        outcome(status)
    )))
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
