//! The `PlatformService` for NixOS.
//!
//! Minimal on purpose: it answers the one question that changes what
//! masys is allowed to say, which is whether a runtime change to a unit
//! survives. Generations, package listings and boot pressure are
//! deliberately still unanswered - the guard is what blocks the Units
//! buffer's enable/disable, and it is what this crate exists for today.

pub mod inputs;
pub mod links;
pub mod ops;
pub mod profiles;

use masys_domain::declarative::{DeclarativeService, HomeMode, Input, InputSource, Inputs, NixOp, Profile, ProfileKind, RebootState};
use masys_domain::error::MasysError;
use masys_domain::platform::{BootPressure, Ownership, Package, PendingReboot, PlatformId, UpdateStatus};
use masys_domain::service::PlatformService;

/// Where NixOS keeps everything it manages. A unit whose definition
/// resolves in here is generated from configuration.
const STORE: &str = "/nix/store";

const UNIT_DIR: &str = "/etc/systemd/system";

/// The system profile's directory, which also holds `per-user`.
const PROFILES: &str = "/nix/var/nix/profiles";

/// Channels are root's profile, not a paradigm of their own: numbered
/// symlinks in a directory, the same shape as every other profile.
const CHANNELS: &str = "/nix/var/nix/profiles/per-user/root";

/// Whether `systemctl enable` could write its symlink at all.
///
/// Checked rather than assumed: NixOS hosts differ, and a host whose
/// `/etc/systemd/system` is a real writable directory will let the
/// enable succeed and then quietly undo it on the next rebuild - which
/// needs a different warning from one that refuses immediately.
fn wants_writable() -> bool {
    std::path::Path::new(UNIT_DIR).join("multi-user.target.wants").metadata().map(|meta| !meta.permissions().readonly()).unwrap_or(false)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NixosPlatform;

/// Whether this host is NixOS, from `/etc/os-release`'s `ID`.
///
/// The ID field rather than PRETTY_NAME: it is the stable machine-readable
/// one, and PRETTY_NAME carries a version and a codename that change.
pub fn is_nixos(os_release: &str) -> bool {
    os_release.lines().find_map(|line| line.strip_prefix("ID=")).map(|value| value.trim().trim_matches('"') == "nixos").unwrap_or(false)
}

/// Whether a unit's definition lives in the nix store, given the path
/// systemd reports for it.
///
/// Follows the link, because `/etc/systemd/system/sshd.service` is itself
/// a symlink into the store on this host - and so is
/// `/etc/systemd/system` itself. Reading the path systemd reports without
/// resolving it would conclude that every unit is hand-managed, which is
/// exactly backwards.
pub fn is_store_managed(fragment_path: &str) -> bool {
    if fragment_path.is_empty() {
        return false;
    }
    std::fs::canonicalize(fragment_path).map(|resolved| resolved.starts_with(STORE)).unwrap_or_else(|_| fragment_path.starts_with(STORE))
}

impl PlatformService for NixosPlatform {
    fn id(&self) -> PlatformId {
        PlatformId::NixOs
    }

    fn packages(&self) -> Result<Vec<Package>, MasysError> {
        // The Packages buffer is out of v1 scope; answering with an empty
        // list would claim this host has no packages.
        Err(MasysError::Platform("listing packages is not implemented for NixOS yet".to_string()))
    }

    fn updates(&self) -> Result<UpdateStatus, MasysError> {
        Err(MasysError::Platform("update checking is not implemented for NixOS yet".to_string()))
    }

    /// Now that `DeclarativeService::reboot` performs the comparison, the
    /// neutral port can answer it too - in the vocabulary Debian and Arch
    /// can also speak, which is a verdict rather than a generation
    /// number. The two ports must never disagree about whether a reboot is
    /// pending, so this delegates rather than repeating the comparison.
    fn pending_reboot(&self) -> Result<Option<PendingReboot>, MasysError> {
        Ok(self.reboot()?.map(|state| PendingReboot { reason: reboot_reason(state.kernel_changed, state.initrd_changed).to_string() }))
    }

    /// The guard.
    ///
    /// A unit generated from configuration is the manifest's to decide.
    /// How that fails depends on the host: usually the change is undone
    /// by the next `nixos-rebuild switch`, but where `/etc/systemd/system`
    /// resolves into the read-only store - as it does here - `systemctl
    /// enable` cannot create its symlink at all and is refused outright.
    /// The note says which, because "reverts later" and "refused now" are
    /// different things to plan around.
    ///
    /// Only enable and disable ask. Starting or stopping a unit writes
    /// nothing and contradicts no manifest: NixOS declares which units
    /// exist and which are enabled at boot, not what is running right
    /// now. An enabled unit coming back after a reboot is the manifest
    /// doing its job, not a conflict.
    ///
    /// A unit *not* in the store was placed by hand and behaves like any
    /// other distro's, so it is genuinely imperative.
    fn unit_ownership(&self, unit: &str) -> Result<Ownership, MasysError> {
        let fragment = format!("{UNIT_DIR}/{unit}");
        if !is_store_managed(&fragment) {
            return Ok(Ownership::Imperative);
        }
        Ok(Ownership::Declarative {
            source: "configuration.nix".to_string(),
            // The distinction matters to whoever is about to press the
            // key. "Reverts later" and "is refused now" are different
            // things to plan around, and on this host it is the second:
            // the directory `systemctl enable` must write its symlink
            // into resolves into the read-only store.
            note: if wants_writable() {
                "generated from configuration".to_string()
            } else {
                "refused now - the unit directory is read-only".to_string()
            },
            reverted_by: "nixos-rebuild switch".to_string(),
        })
    }

    fn boot_pressure(&self) -> Result<Option<BootPressure>, MasysError> {
        // Counting generations with boot entries needs the bootloader's
        // layout, which is not implemented yet.
        Ok(None)
    }
}

/// The one-phrase verdict `PendingReboot::reason` carries.
///
/// Three cases, not two. The initrd is read once, at boot, so a
/// generation whose initrd moved needs a reboot exactly as much as one
/// whose kernel did. Branching on `kernel_changed` alone tells such a host
/// it is "activation-only", which is not an incomplete answer but the
/// opposite of the true one - and `RebootState` carries both flags
/// precisely because together they are the difference between "reboot now"
/// and "this can wait". Only the third case may say it can wait.
///
/// **A phrase, not a sentence, and that is a hard constraint.** This
/// string has exactly one renderer: `masys_render::view::overview_lines`
/// appends it to the Status buffer's third line, after everything else
/// that line carries. At the eighty columns masys is drawn to fit, that
/// leaves [`REASON_BUDGET`] columns, and a longer reason does not wrap -
/// it is clipped, taking the verdict with it and leaving a sentence that
/// trails off mid-word. The detail belongs to the Nix view's reboot block,
/// which has whole lines to spend on it and builds them from the flags
/// rather than from this text.
///
/// `&'static str` rather than `String`: there are three of them and they
/// are fixed, so the test that measures them measures every reason this
/// adapter can ever produce.
pub fn reboot_reason(kernel_changed: bool, initrd_changed: bool) -> &'static str {
    match (kernel_changed, initrd_changed) {
        (true, _) => "kernel changed",
        (false, true) => "initrd changed",
        (false, false) => "activation-only",
    }
}

/// How many terminal columns a `reboot_reason` may occupy.
///
/// The Status buffer's third line at eighty columns, worst case: two
/// columns of frame margin, two more of row indent, then
/// `. clock not synced  .  smart failing  .  reboot pending: `. Whatever
/// is left is the reason's. Both leading flags are spelled at their
/// longest because both are readings this adapter cannot influence, and a
/// budget that assumed the short spelling of either would hold on this
/// host and fail on somebody else's. Bytes stand in for columns because
/// the prefix is ASCII; the reasons are measured in columns, which is the
/// measure that actually decides where the clip falls.
pub const REASON_BUDGET: usize = 80 - 2 - 2 - ". clock not synced  .  smart failing  .  reboot pending: ".len();

/// Whether a reboot is pending, and what actually differs.
///
/// Takes its readings rather than performing them, so the comparison -
/// which is the part with a decision in it - is testable without a NixOS
/// host.
///
/// Kernels and initrds are compared by **store path**, not by version.
/// Two builds of 6.18.42 are different kernels and only the hash says so;
/// comparing the version would report "activation-only" for a rebuild that
/// genuinely needs a reboot, which is the one mistake this function exists
/// to avoid.
pub fn reboot_state(
    booted_system: &str,
    current_system: &str,
    booted_kernel: Option<&str>,
    current_kernel: Option<&str>,
    booted_initrd: Option<&str>,
    current_initrd: Option<&str>,
) -> Option<masys_domain::declarative::RebootState> {
    if booted_system == current_system {
        return None;
    }
    Some(masys_domain::declarative::RebootState {
        booted_store_path: booted_system.to_string(),
        current_store_path: current_system.to_string(),
        // An unreadable side counts as changed. The alternative is
        // claiming a reboot can wait on the strength of a reading that was
        // not taken.
        kernel_changed: booted_kernel != current_kernel || booted_kernel.is_none(),
        initrd_changed: booted_initrd != current_initrd || booted_initrd.is_none(),
    })
}

/// A file's contents, or `None` if it is simply not there.
///
/// The distinction this whole adapter turns on. `read_to_string(..).ok()`
/// collapses "this host has no such file" into the same value as "this
/// file exists and I was refused it", and the second must not be reported
/// as an absence - a permission error would otherwise render as a host
/// with no GC policy, which is a finding somebody would act on.
fn read_if_present(path: &str) -> Result<Option<String>, MasysError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(MasysError::Io(error)),
    }
}

impl DeclarativeService for NixosPlatform {
    fn profiles(&self) -> Result<Vec<Profile>, MasysError> {
        // `.ok()` deliberately, where `reboot()` propagates. The failure
        // modes point opposite ways: there, an unread link would become a
        // positive claim that no reboot is needed, while here it can only
        // *withhold* a marker - `read_profile` refuses to pair a `None`
        // with anything, so no generation is wrongly flagged booted, one
        // is merely left unflagged. And `Err` would cost the whole
        // generations list - every row, read from a different directory
        // entirely - to avoid losing one annotation on it.
        let booted = std::fs::canonicalize("/run/booted-system").ok().map(|path| path.to_string_lossy().to_string());
        // `None` rather than a relative path when `HOME` is unset:
        // `PathBuf::default().join(..)` yields `.local/state/nix/profiles`,
        // which would read whatever happens to sit under the working
        // directory and report it as this user's generations.
        let home = std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/state/nix/profiles"));

        let sources = [
            (ProfileKind::System, Some(std::path::PathBuf::from(PROFILES)), "system"),
            (ProfileKind::Home, home, "profile"),
            (ProfileKind::Channels, Some(std::path::PathBuf::from(CHANNELS)), "channels"),
        ];

        let mut profiles = Vec::new();
        for (kind, dir, prefix) in sources {
            let Some(dir) = dir else { continue };
            // Only the system profile can hold the booted generation.
            let booted = matches!(kind, ProfileKind::System).then_some(booted.as_deref()).flatten();
            let Some(profile) = crate::profiles::read_profile(kind, &dir, prefix, booted)? else { continue };
            // A directory that exists with no generations in it is a
            // second kind of absence, distinct from the missing directory
            // `read_profile` already reports as `None`, and it needs the
            // same treatment. It is the ordinary channels case on a flake
            // host: `/nix/var/nix/profiles/per-user/root` is there and
            // empty, and a "Channels" section reading zero would invite
            // somebody to go looking for channels this host does not use.
            if !profile.generations.is_empty() {
                profiles.push(profile);
            }
        }
        Ok(profiles)
    }

    fn reboot(&self) -> Result<Option<RebootState>, MasysError> {
        // `Ok(None)` from this method carries one documented meaning -
        // booted and current are the same store path - and
        // `pending_reboot` hands it on as "nothing to reboot for". So only
        // a link that genuinely is not there may reach it. A container, a
        // chroot or a build sandbox has no `/run/booted-system` at all,
        // and on such a host there is no booted system to differ from.
        // Every other failure is `Err`: telling somebody no reboot is
        // needed on the strength of a read that was refused is the exact
        // false statement this method exists to prevent, and it is the one
        // the stub this replaced was written to avoid making.
        let resolve = |path: &str| match std::fs::canonicalize(path) {
            Ok(resolved) => Ok(Some(resolved.to_string_lossy().to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(MasysError::Io(error)),
        };
        // `.ok()` here is safe in the one direction that matters, unlike
        // above: `reboot_state` counts an unread kernel or initrd as
        // *changed*, so a failed read errs toward "reboot now" rather than
        // toward "this can wait". Propagating instead would replace a
        // conservative answer with no answer at all.
        let link =
            |base: &str, name: &str| std::fs::read_link(format!("{base}/{name}")).ok().map(|path| path.to_string_lossy().to_string());
        let (Some(booted), Some(current)) = (resolve("/run/booted-system")?, resolve("/run/current-system")?) else {
            return Ok(None);
        };
        Ok(reboot_state(
            &booted,
            &current,
            link("/run/booted-system", "kernel").as_deref(),
            link("/run/current-system", "kernel").as_deref(),
            link("/run/booted-system", "initrd").as_deref(),
            link("/run/current-system", "initrd").as_deref(),
        ))
    }

    /// `Module` where a `home-manager-*.service` was generated into the
    /// system closure and no `home-manager` binary exists. On such a host
    /// home-manager is activated by `nixos-rebuild switch`, and offering a
    /// separate switch would be a button that cannot work.
    ///
    /// `read_dir` on `UNIT_DIR` follows it, which is what makes this work
    /// at all here: `/etc/systemd/system` is a symlink into the read-only
    /// store, and the generated units are its contents.
    fn home_mode(&self) -> Result<HomeMode, MasysError> {
        let module = match std::fs::read_dir(UNIT_DIR) {
            Ok(entries) => entries.flatten().any(|entry| entry.file_name().to_string_lossy().starts_with("home-manager-")),
            // No unit directory at all means no module install. Any other
            // failure is not an answer, and reporting `Absent` from one
            // would hide a standalone install behind a permission error.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(MasysError::Io(error)),
        };
        if module {
            return Ok(HomeMode::Module);
        }
        // `var_os`, not `var(..).unwrap_or_default()` - the same mistake
        // `profiles()` avoids with `HOME`. An unset or non-UTF-8 `PATH`
        // becomes `""`, whose single empty segment makes
        // `Path::new("").join("home-manager")` a *relative* path, probed
        // against whatever directory masys was started from. No `PATH` is
        // not a standalone install; it is no way to look for one.
        //
        // Empty components are dropped for the same reason. A shell reads
        // one as the working directory, but a `home-manager` binary that
        // happens to sit in the directory masys was launched from is not
        // evidence that this host has a standalone home-manager.
        let standalone = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).filter(|dir| !dir.as_os_str().is_empty()).any(|dir| dir.join("home-manager").exists()))
            .unwrap_or(false);
        Ok(if standalone { HomeMode::Standalone } else { HomeMode::Absent })
    }

    fn inputs(&self) -> Result<Option<Inputs>, MasysError> {
        if let Some(lock_path) = crate::inputs::locate_lock(None) {
            // `?` rather than `read_if_present`: `locate_lock` only yields
            // a path it has already seen as a file, so a failure here is a
            // lock that exists and could not be read - an error, and never
            // the "this host has no lock" that `None` means below.
            let text = std::fs::read_to_string(&lock_path)?;
            return Ok(Some(Inputs {
                source: InputSource::Flake { lock_path: lock_path.to_string_lossy().to_string() },
                inputs: crate::inputs::parse_lock(&text)?,
            }));
        }
        // No lock. Channels are the other paradigm, and a channel profile
        // with generations is what says this host uses them.
        let channels = crate::profiles::read_profile(ProfileKind::Channels, std::path::Path::new(CHANNELS), "channels", None)?;
        let Some(channels) = channels.filter(|profile| !profile.generations.is_empty()) else { return Ok(None) };
        Ok(Some(Inputs {
            source: InputSource::Channels,
            inputs: channels
                .generations
                .iter()
                .map(|generation| Input {
                    name: format!("channels generation {}", generation.id),
                    origin: None,
                    rev: None,
                    // A generation whose mtime could not be read carries
                    // `None`, and stays `None`: a channel dated
                    // 1970-01-01 is a fabrication, not an old channel.
                    last_modified_secs: generation.created_ms.map(|created| created / 1000),
                    direct: true,
                })
                .collect(),
        }))
    }

    /// Out of the `nixos-version` script the system closure generates,
    /// which is the only place in the running system that carries the
    /// revision in full.
    ///
    /// Not `/run/current-system/nixpkgs-revision`: no such file exists.
    /// The closure writes `/run/current-system/nixos-version`, and that
    /// holds a version string - `26.11.20260804.e72e4f2` - whose last
    /// segment is the *short* rev. Seven hex digits cannot be compared
    /// against a lock's forty without truncating the lock too, and a
    /// seven-digit comparison calls two different revisions in sync
    /// whenever they happen to share a prefix. The drift this exists to
    /// detect deserves the whole revision.
    ///
    /// Read as a file rather than run as a command. The revision is a
    /// build-time literal baked into the script, exactly like the
    /// retention `gc_retention` lifts out of the generated
    /// `nix-gc-start`; spawning a shell to echo back a constant would cost
    /// a subprocess and buy no accuracy.
    fn running_nixpkgs_rev(&self) -> Result<Option<String>, MasysError> {
        let Some(script) = read_if_present("/run/current-system/sw/bin/nixos-version")? else { return Ok(None) };
        // The `--json` branch emits `"nixpkgsRevision":"<rev>"`. Split on
        // the key and then on quotes rather than matching the pair
        // together, so a generator that spaces its JSON differently still
        // reads.
        let Some((_, after_key)) = script.split_once("\"nixpkgsRevision\"") else { return Ok(None) };
        let Some((_, after_quote)) = after_key.split_once('"') else { return Ok(None) };
        let Some((rev, _)) = after_quote.split_once('"') else { return Ok(None) };
        // The script guards its own `--revision` output with `[[ <rev> =~
        // ^[0-9a-f]+$ ]]`, because the value is a build-time substitution
        // that stays an unexpanded `@revision@` on a system built outside
        // a git checkout. The same predicate here: a placeholder is this
        // system recording no revision, and handing it back as one would
        // make every comparison against it report drift.
        let looks_like_a_rev = !rev.is_empty() && rev.chars().all(|character| matches!(character, '0'..='9' | 'a'..='f'));
        Ok(looks_like_a_rev.then(|| rev.to_string()))
    }

    /// Grepped out of whatever `nix-gc.service` runs, which on NixOS is a
    /// generated wrapper script rather than the command itself. `None`
    /// where it cannot be found: the row then reports the schedule without
    /// claiming a retention it did not read.
    fn gc_retention(&self) -> Result<Option<String>, MasysError> {
        // Absent at each step is a real answer - no nix-gc.service means
        // no automatic collection is configured, which is exactly the
        // finding worth reporting. A read that *fails* is not: a
        // permission error here would otherwise be indistinguishable from
        // "no policy", and the row would quietly claim the host has none.
        let Some(unit) = read_if_present(&format!("{UNIT_DIR}/nix-gc.service"))? else { return Ok(None) };
        let Some(exec) = unit.lines().find_map(|line| line.strip_prefix("ExecStart=")) else { return Ok(None) };
        let Some(script_path) = exec.split_whitespace().next() else { return Ok(None) };
        let Some(script) = read_if_present(script_path)? else { return Ok(None) };
        Ok(script.split_whitespace().skip_while(|word| *word != "--delete-older-than").nth(1).map(str::to_string))
    }

    /// A `0` here reads as an actionable claim - nothing is pinning the
    /// store - so an unreadable directory must not produce one. Missing is
    /// zero; unreadable is an error.
    fn gc_roots(&self) -> Result<u32, MasysError> {
        match std::fs::read_dir("/nix/var/nix/gcroots/auto") {
            Ok(entries) => Ok(entries.flatten().count() as u32),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(MasysError::Io(error)),
        }
    }

    /// The one mutating method on this port, and nothing is decided here.
    /// `ops::run` resolves the flake reference, maps the operation to a
    /// command line and spawns it; keeping the mapping out of this `impl`
    /// is what lets `tests/ops.rs` assert all of it without a
    /// `NixosPlatform` and without executing anything.
    fn flake_ref(&self) -> Result<Option<String>, MasysError> {
        Ok(ops::flake_ref())
    }

    fn nixos_config(&self) -> Result<Option<String>, MasysError> {
        Ok(ops::nixos_config())
    }

    fn run(&self, op: &NixOp) -> Result<(), MasysError> {
        crate::ops::run(op)
    }
}
