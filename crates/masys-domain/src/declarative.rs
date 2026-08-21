//! The port for a distro whose system state is generated from a
//! configuration rather than assembled by commands.
//!
//! Separate from `PlatformService`, which stays neutral. That port exists
//! to answer one question every distro can answer - does a runtime
//! `enable` survive - and `masys-platform-fallback` exists to force it to
//! model not knowing. Generations, profiles and flake locks are not
//! questions Debian can answer at all, and putting them there would
//! dissolve the property the fallback crate is there to protect.
//!
//! Held by the session as an `Option`. Absent is the ordinary case: on
//! Debian, Arch, or an unrecognised host there is no service, and the Nix
//! view is not registered rather than being registered and empty.

use crate::error::MasysError;

/// Which profile a generation belongs to.
///
/// Three, and they are the same kind of thing - a numbered symlink into
/// the store with an mtime. Channels stop being a separate paradigm the
/// moment they are modelled as what they are, which is why rolling back
/// a channel needs no concept of its own - it is the same generation
/// switch as the other two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfileKind {
    /// `/nix/var/nix/profiles/system`
    System,
    /// `~/.local/state/nix/profiles/profile`
    Home,
    /// `/nix/var/nix/profiles/per-user/root/channels`
    Channels,
}

/// One generation of one profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    pub id: u64,
    /// Unix milliseconds, from the symlink's own mtime. `None` when the
    /// link could not be read, not `0` - `0` is a real date, 1970-01-01,
    /// and a generation claiming to predate nix itself is a fabrication.
    /// The view renders `-` for these, the convention `masys_domain::rate`
    /// already uses for a rate with no second sample.
    ///
    /// Not from `nixos-rebuild list-generations`, which reports build
    /// dates that are non-monotonic - it dates generation 432 after 433 on
    /// the development host - and not from `nh os info`, which disagrees
    /// with it. The symlink mtime is the timestamp of the event actually
    /// being reported, costs a `readdir` rather than a 1.8-second
    /// subprocess, and ascends cleanly across every generation in the
    /// directory.
    pub created_ms: Option<u64>,
    /// `None` when the generation link is dangling and could not be
    /// resolved. Not `""`: two generations that both failed to resolve
    /// would carry the same empty string and compare equal to each other,
    /// so a later lookup by store path would confidently name the wrong
    /// generation. Two unknowns must never compare equal.
    pub store_path: Option<String>,
    /// The version the store path names, e.g. `26.11.20260804.e72e4f2`.
    /// `None` for a profile whose paths do not carry one - the home
    /// profile's are plain `-profile`.
    pub label: Option<String>,
    /// The store path of this generation's kernel. Compared by path rather
    /// than by version string: two builds of 6.18.42 are different
    /// kernels, and the hash is what says so.
    pub kernel: Option<String>,
    pub current: bool,
    pub booted: bool,
}

/// The kernel version a `kernel` store path names, e.g. `6.18.42` from
/// `/nix/store/<hash>-linux-6.18.42/bzImage`.
///
/// Used for display only. Whether two generations share a kernel is
/// decided by comparing `Generation::kernel` in full, because two builds
/// of 6.18.42 are different kernels and only the hash says so.
///
/// Lives in `masys-domain` rather than beside the rest of this crate's
/// string parsing in `masys-platform-nixos::links`, because
/// `masys-render` is the only caller: it draws `Generation::kernel` for
/// a row and must not depend on the adapter that read it, and
/// masys-domain is the one crate both already depend on. The adapter
/// itself has no reader of its own - it only ever needed the raw path
/// this parses, never the parsed version.
pub fn kernel_version(kernel_path: &str) -> Option<String> {
    let dir = kernel_path.rsplit('/').nth(1)?;
    let version = dir.split_once("-linux-")?.1;
    // A version starts with a digit. Without this, a directory that
    // merely contains `-linux-` hands back whatever followed it, stated
    // as confidently as a real version.
    version.chars().next().filter(char::is_ascii_digit)?;
    Some(version.to_string())
}

/// One profile and every generation it retains, ascending by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub kind: ProfileKind,
    /// The profile symlink itself - `/nix/var/nix/profiles/system`, not
    /// the directory it sits in. What `nix-env -p <path>` is given:
    /// `App::profile_path` reads it to build `NixOp::Activate`, and
    /// `ops::activation_argv` joins `bin/switch-to-configuration` onto
    /// it. Handed the directory, the first of those would create a new
    /// profile beside the real ones and the second would name a file that
    /// does not exist.
    ///
    /// The adapter's to report rather than the caller's to compose. The
    /// home profile moved from `~/.nix-profile` to
    /// `~/.local/state/nix/profiles/profile`, and which of the two a host
    /// uses is exactly the kind of thing this port exists to answer.
    pub path: String,
    /// Whether this process can write the directory the profile's
    /// generation links live in - which is what both operations that
    /// touch a profile actually need. `nix-env --switch-generation`
    /// replaces a symlink in that directory, and `nix-collect-garbage
    /// --delete-older-than` unlinks generations from it.
    ///
    /// Reported because these two have no elevation of their own. A unit
    /// verb shells out to `systemctl`, which triggers polkit, so an
    /// unprivileged masys can be *granted* the action; `nixos-rebuild`
    /// has `--elevate {none,sudo,run0}`. `nix-env` and
    /// `nix-collect-garbage` have neither - their synopses carry no such
    /// flag - so the only thing masys can do is decline to start a
    /// command it can see will not finish.
    ///
    /// `None` where the check could not be made, and it refuses a key the
    /// same as `Some(false)` does: "I could not find out" must not become
    /// permission nobody measured. Advisory either way - the command
    /// enforces the real thing, and this can go stale between the read
    /// and the keypress. What it buys is that a refusal arrives as a
    /// marked key rather than as a part-finished delete.
    pub writable: Option<bool>,
    pub generations: Vec<Generation>,
}

/// How home-manager is deployed, which decides whether it can be switched
/// on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeMode {
    /// A NixOS module: generations are real, and they are activated by
    /// `nixos-rebuild switch`. There is no `home-manager` binary.
    Module,
    /// A standalone install with its own `home-manager` command.
    Standalone,
    /// Not installed.
    Absent,
}

/// A reboot the running system needs, and what actually differs.
///
/// `None` from `reboot()` means booted and current are the same store
/// path. Where they differ, `kernel_changed` and `initrd_changed` are the
/// difference between "reboot now" and "activation-only, this can wait" -
/// which is the whole reason to report it rather than printing "reboot
/// required" like everything else does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebootState {
    pub booted_store_path: String,
    pub current_store_path: String,
    pub kernel_changed: bool,
    pub initrd_changed: bool,
}

/// One dependency the configuration pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub name: String,
    /// `owner/repo` where the lock records a forge owner and repo, for
    /// the row's right-hand column. Otherwise the `url` or `path` the
    /// lock records instead - a path input showing its path is more
    /// useful than a blank column - and `None` only when the locked
    /// block carries none of those.
    pub origin: Option<String>,
    pub rev: Option<String>,
    /// Unix seconds the input was last modified upstream, from the lock.
    /// `None` for a channel, which records no such thing.
    pub last_modified_secs: Option<u64>,
    /// Whether the configuration names this input directly, as against
    /// inheriting it through another input. A 235-day-old transitive
    /// `flake-compat` is not the same finding as a stale `nixpkgs`.
    pub direct: bool,
}

/// Where the inputs came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputSource {
    Flake {
        /// Not read today; `nix flake update` is what would use it - run
        /// with this lock's directory as its working directory, which is
        /// what this path names.
        lock_path: String,
    },
    Channels,
}

/// What pins this configuration, from whichever of the two paradigms is in
/// use. Both may be present; so may neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    pub source: InputSource,
    pub inputs: Vec<Input>,
}

/// NixOS facts, and the one method that acts on them.
///
/// Every method but [`DeclarativeService::run`] is a read: it answers a
/// question and changes nothing. `run` is the single mutating entry
/// point, named so that the boundary is visible at every call site -
/// there is no second way to make this port touch the host.
pub trait DeclarativeService {
    /// Every profile with generations, in `ProfileKind` order. A profile
    /// directory that does not exist is omitted rather than reported
    /// empty: "this host has no home-manager" and "home-manager has no
    /// generations" are different facts.
    fn profiles(&self) -> Result<Vec<Profile>, MasysError>;

    /// `None` when booted and current are the same store path.
    fn reboot(&self) -> Result<Option<RebootState>, MasysError>;

    /// How home-manager is deployed on this host. `Module` means it was
    /// generated into the system closure and is activated by
    /// `nixos-rebuild switch` - there is no separate `home-manager`
    /// binary in that mode. `Standalone` means it has its own command and
    /// its own generations. `Absent` means it is not installed at all.
    ///
    /// All three are real answers, not a failure to look: a host that
    /// simply does not use home-manager is `Absent`, the same kind of fact
    /// as `Module` or `Standalone`. `Err` is reserved for genuinely being
    /// unable to tell - the filesystem checks this rests on could not be
    /// read.
    fn home_mode(&self) -> Result<HomeMode, MasysError>;

    /// `None` when neither a flake lock nor a populated channel profile
    /// can be found - which is a real state, not a failure.
    fn inputs(&self) -> Result<Option<Inputs>, MasysError>;

    /// The nixpkgs revision the running system was built from. Compared
    /// against the lock's to detect a `nix flake update` that has not been
    /// followed by a rebuild - a state nothing else reports.
    ///
    /// `None` when the running system carries no such revision to report -
    /// a channel-built system pins nothing this precise, and that absence
    /// is real, not a lookup failure. `Err` when the store path that would
    /// carry it exists but cannot be read.
    fn running_nixpkgs_rev(&self) -> Result<Option<String>, MasysError>;

    /// The retention `nix-gc.service` is configured with, e.g.
    /// `--delete-older-than 14d`. `None` when the service exists and its
    /// generated script sets no `--delete-older-than` - a real absence of
    /// policy, and the row then says the schedule without claiming one.
    /// `Err` when the unit or its generated script exists but cannot be
    /// read or parsed - that is a lookup failure, not an absence, and it
    /// must not collapse into the same `None` a genuine unbounded policy
    /// would produce.
    ///
    /// Only the retention: the *schedule* comes from the timer unit, which
    /// `SystemService` already polls. Asking this port for it would be
    /// this crate reimplementing systemd.
    fn gc_retention(&self) -> Result<Option<String>, MasysError>;

    /// How many roots are pinning the store against collection.
    ///
    /// `u32`, not `Option<u32>`: unlike `gc_retention` and
    /// `running_nixpkgs_rev`, there is no real state here for `None` to
    /// name. `/nix/var/nix/gcroots` exists on every working Nix install,
    /// so an empty count is a genuine zero, not an absence of one. A
    /// failure to enumerate it - permission denied, the directory itself
    /// unreadable - is `Err`, not `0`: `0` reads as "nothing is pinning
    /// your store", an actionable claim this port must not make by
    /// accident of a read failure.
    fn gc_roots(&self) -> Result<u32, MasysError>;

    /// This host's flake reference, or `None` where none resolves.
    ///
    /// `None` is an answer, not a failure: a channels host has no flake
    /// reference and a bare `nixos-rebuild switch` is the correct command
    /// there. `Err` is being unable to tell.
    ///
    /// Read by the transient to decide which of its rows are marked -
    /// `nix flake update` on a host with no flake is the one case the
    /// design calls genuinely flake-only. It has to be *this* fact and
    /// not the paradigm [`Inputs::source`] reports, because an
    /// implementation builds its argv from this same answer: sourcing the
    /// mark and the command differently is how a popup comes to mark a
    /// row that would have worked, or leave live one that cannot.
    ///
    /// Distinct from `Inputs::source` in a real case, which is why the
    /// distinction is worth a method: a flake checked out but never
    /// locked has a reference and no `flake.lock`, so it reports
    /// `Channels` or nothing while `nix flake update` - the command that
    /// would *write* that lock - works fine.
    fn flake_ref(&self) -> Result<Option<String>, MasysError>;

    /// Whether `<nixos-config>` resolves on this host, which is what
    /// `nixos-option` needs and cannot be given on the command line.
    ///
    /// `None` is an answer: a flake host that sets no `nix.nixPath` and
    /// has no `/etc/nixos/configuration.nix` has no `<nixos-config>`, and
    /// `nixos-option` there fails with *file 'nixos-config' was not found
    /// in the Nix search path* before it evaluates anything. Measured on
    /// the development host, which is exactly that shape - it is why this
    /// method exists rather than the row being left live.
    ///
    /// Not the same question as [`DeclarativeService::flake_ref`], and
    /// not the complement of it: a flake host *can* put `nixos-config` on
    /// the path, and a channels host normally has one without having a
    /// flake at all.
    fn nixos_config(&self) -> Result<Option<String>, MasysError>;

    /// Perform an operation.
    ///
    /// Every one of these takes over the terminal - a rebuild streams for
    /// minutes and `nix store diff-closures` pages. So this is called
    /// from `App::run_suspended`, with the terminal already handed back,
    /// and never from `tick` or `rebuild`. There is nothing an
    /// implementation can check to make sure of that; the contract is the
    /// caller's to keep, the same one
    /// `masys_systemd::actions::systemctl_interactive` already carries.
    ///
    /// An `Err` here is the command's own failure. The command's stderr
    /// has already reached the terminal by the time this returns, because
    /// the operator needs `nixos-rebuild`'s error text and not masys's
    /// summary of it; what the `Err` adds is which command failed and how.
    fn run(&self, op: &NixOp) -> Result<(), MasysError>;
}

/// One operation the Nix view can perform.
///
/// A typed enum rather than a data-driven table because the operations do
/// not share an argument shape: `Activate` takes a profile and a
/// generation, `Clean` takes a retention, `SearchPackages` takes a query,
/// `Diff` takes two store paths, and `Rollback` takes nothing. Expressing
/// that in a table needs an argument-schema mini-language; the type
/// system says it once, for free, and a missing field is a compile error
/// rather than a command that runs with an argument missing.
///
/// **Nothing here invokes `nh`.** `nh` was the reference for *which*
/// operations are worth offering and nothing more - every one of these
/// drives `nixos-rebuild`, `nix`, `nix-env`, `nix-channel`,
/// `nixos-option` or `nix-collect-garbage` directly. A published crate
/// cannot require somebody else's tool to be installed before its buttons
/// work. `masys-platform-nixos::ops::argv` is the one place that says
/// what each of these runs.
///
/// The design's table lists three more - `repl`, `build-vm` and
/// `build-image` - which are deliberately absent until something can
/// reach them. Each carries an unresolved question that only the reaching
/// code can settle: `repl` is interactive with no natural end, where
/// every operation here streams and exits; `build-image` requires an
/// `--image-variant` whose candidate list has to come from the flake, so
/// it needs the transient's input engine before its field shape is
/// knowable; and `build-vm`'s value is in *running* the
/// `./result/bin/run-*-vm` it produces, which masys does not yet do. A
/// variant with no producer is dead weight - `SectionKind::NixUnits` was
/// carried for a while with nothing constructing it - and each of these
/// costs one variant, one match arm and one test whenever it is wanted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NixOp {
    /// `nixos-rebuild <verb>`, with `--flake <ref>` where one resolves.
    ///
    /// The ref is appended, not required: a bare `nixos-rebuild switch`
    /// is the correct command on a channels host, not a broken one, so
    /// these rows stay live where no ref resolves. Only the genuinely
    /// flake-only operations dim.
    Rebuild(RebuildVerb),
    /// Activate a specific generation. Two commands, because there is no
    /// high-level form: `nixos-rebuild` offers `--rollback`, which goes
    /// to the previous generation only, and `--specialisation`. There is
    /// no `--to-generation`. So `nix-env --switch-generation` followed by
    /// `switch-to-configuration switch` is not masys reaching past the
    /// supported interface, it is the supported interface.
    Activate {
        profile: String,
        generation: u64,
    },
    /// `nixos-rebuild switch --rollback` - the previous generation.
    ///
    /// `--rollback` is a flag, not an action: `nixos-rebuild`'s action
    /// list holds thirteen names and `rollback` is not one of them, so a
    /// bare `nixos-rebuild rollback` prints usage and exits. It pairs
    /// with `switch` because a rollback means running the previous
    /// configuration now.
    ///
    /// Its own variant rather than a [`RebuildVerb`], because it is the
    /// one rebuild form that takes no flake reference: it builds nothing,
    /// it activates a generation that already exists.
    Rollback,
    /// `nix store diff-closures <from> <to>`.
    Diff {
        from: String,
        to: String,
    },
    /// `nix-collect-garbage --delete-older-than <spec>`.
    ///
    /// The standard answer, which is why it is listed before
    /// [`NixOp::DeleteGenerations`]: it prunes profile generations and
    /// collects the store in one step.
    Clean {
        older_than: String,
    },
    /// `nix-env -p <profile> --delete-generations <spec>`.
    ///
    /// The escape hatch. This is the legacy imperative interface and it
    /// prunes *without* collecting - the generations go, the store does
    /// not shrink until something collects it - so it is the wrong reflex
    /// for "reclaim disk" and the right one for "drop these specific
    /// generations".
    DeleteGenerations {
        profile: String,
        spec: String,
    },
    /// `nix flake update`, or one input where named.
    FlakeUpdate {
        input: Option<String>,
    },
    /// `nix flake check`.
    FlakeCheck,
    /// `nix-channel --update`, and its rollback. Mirror images of the
    /// flake operations: these dim on a flake host by the same rule that
    /// dims those on a channels host.
    ChannelUpdate,
    ChannelRollback,
    /// `nix search nixpkgs <query>`.
    ///
    /// Local, unlike `nh search options`, which queries
    /// search.nixos.org. masys has no network code and acquiring one
    /// would make it a different kind of program - TLS, timeouts, offline
    /// behaviour, and a view that can now fail for reasons that have
    /// nothing to do with the host.
    SearchPackages {
        query: String,
    },
    /// `nixos-option <name>` - this host's value, not a nixpkgs-wide
    /// search. Narrower than `nh search options` and a better question
    /// for a tool that reports on one machine.
    OptionValue {
        name: String,
    },
    /// `home-manager switch`. Only meaningful in [`HomeMode::Standalone`];
    /// in [`HomeMode::Module`] home-manager is activated by
    /// `nixos-rebuild switch` and there is no `home-manager` binary to
    /// run.
    HomeSwitch,
}

/// The `nixos-rebuild` sub-commands that build a configuration.
///
/// The five the design's transient draws, and no more.
/// [`NixOp::Rollback`] is not among them: it is `switch` with a
/// `--rollback` flag rather than a sub-command, and it takes no flake
/// reference where all five of these do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildVerb {
    /// Build, activate, and make it the boot default.
    Switch,
    /// Build and make it the boot default, without activating now.
    Boot,
    /// Build and activate, without touching the boot default.
    Test,
    /// Build only.
    Build,
    /// Print what would change, activating nothing.
    DryActivate,
}
