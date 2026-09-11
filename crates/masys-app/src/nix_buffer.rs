//! The Nix buffer: what the configuration declares, what was realised,
//! and what is running.
//!
//! Ordered by what needs attention, like Status. A pending reboot leads
//! because it is the only section that appears solely because something is
//! wrong. Store follows and always renders: it carries the declared
//! maintenance policy rather than a complaint, which makes it this buffer's
//! equivalent of the Status buffer's System section. Generations and
//! inputs follow, and each is omitted entirely when it has nothing to
//! show - a profile directory that exists and is empty contributes no
//! section, because "no channels" and "zero channel generations" send the
//! operator looking for two different things. Nix's own units come last:
//! they are what is *running*, where everything above them is what was
//! declared and what was realised.

use std::collections::HashSet;

use masys_domain::declarative::{
    DeclarativeService, Generation, HomeMode, InputSource, Inputs, NixOp, Profile, ProfileKind,
    RebootState, RebuildVerb,
};
use masys_domain::error::MasysError;
use masys_domain::sample::Filesystem;
use masys_domain::unit::Unit;
use masys_view::{Node, SectionKind, SwitchRow};

use crate::keymap::{Action, NixFamily, NixVerb};
use crate::systemd_buffer::{Expanded, severity, unit_rows};
use crate::transient::{DefGroup, DefRow, TransientDef};

/// What a Nix verb can do on the row the cursor is on.
///
/// Three answers where there used to be two, because the transient
/// brought verbs that need a value nobody has read: a query, an option
/// name, a generation spec. `Asks` is not "cannot act here" - the row is
/// live, and pressing it opens the input sub-step - and folding the two
/// together would mark `search packages` on every host, which is the one
/// operation that always works.
///
/// One function answers all three, which is the invariant [`NixBuffer::offer`]'s
/// doc protects: the footer, the popup and the dispatch all ask it, so a
/// row that is offered is a row that will act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NixOffer {
    /// Runnable now, arguments resolved.
    Ready(NixOp),
    /// Runnable once the operator types something.
    Asks { prompt: &'static str },
    /// Not runnable here.
    No,
}

/// The Nix buffer: nine readings, and everything they answer.
///
/// These were nine fields of `App`'s `Facts` and fifteen of its methods,
/// and keeping them apart is what cost its row builder eleven parameters -
/// every one of them a fact held on the far side of the seam and passed
/// back across it. The seam now sits around the state instead of through
/// it, so [`NixBuffer::rows`] asks for only what it genuinely shares with
/// the rest of the session.
///
/// The fields are public because they are *readings*, not invariants.
/// This type enforces one rule about them - keep-last-good - and
/// [`NixBuffer::refresh`] is the only place in the program that applies
/// it; nothing else should write one. In exchange a test builds the host
/// it wants with `..Default::default()`, which is what a suite that used
/// to pass eleven positional arguments writes instead.
///
/// Every field is `Option` and `None` means **no read has succeeded
/// yet**, never that the host has none of these.
///
/// Five of the nine are doubly or unexpectedly optional, and for one
/// reason rather than five: a default that renders as a reading is a
/// reading masys did not take. `Vec::new()` renders as `0 system
/// generations . 0 home`, `0` as `0 gc roots`, and a bare `None`
/// retention as a gc job that keeps nothing - three claims an operator
/// would act on, made on the strength of a read that failed. Only
/// `reboot` and `inputs` default honestly, because for those two the
/// port documents `None` as an answer in its own right.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct NixBuffer {
    pub profiles: Option<Vec<Profile>>,
    pub reboot: Option<RebootState>,
    pub inputs: Option<Inputs>,
    /// `None` until a read succeeds, which is not the same fact as
    /// `Absent`. `Absent` is the port's answer for "home-manager is not
    /// installed on this host"; a host whose `home_mode` read keeps
    /// failing has not said that, and must not be recorded as having.
    pub home_mode: Option<HomeMode>,
    pub nixpkgs_rev: Option<String>,
    /// Doubly optional, because there are three facts. The outer `None`
    /// is "not read"; `Some(None)` is the port's own answer that
    /// `nix-gc.service` declares no `--delete-older-than`. The port's doc
    /// is explicit that its `Err` must not collapse into that inner
    /// `None`, and a single `Option` here is exactly that collapse.
    pub gc_retention: Option<Option<String>>,
    /// `Option<u32>` where the port answers `u32`, for the reason the
    /// port's own doc gives for refusing to answer `0`: `0` reads as
    /// "nothing is pinning your store". Storing the default would put the
    /// claim back one layer up.
    pub gc_roots: Option<u32>,
    /// Doubly optional, for `gc_retention`'s reason: the outer `None` is
    /// "not read", and `Some(None)` is the port's own answer that no
    /// flake reference resolves - which is a channels host, not a
    /// failure. Collapsing the two would mark the flake-only rows on a
    /// host whose read merely failed, and mark them for the wrong reason.
    pub flake_ref: Option<Option<String>>,
    /// And where `<nixos-config>` resolves, doubly optional for the same
    /// reason. `nixos-option` is the one operation that needs it.
    pub nixos_config: Option<Option<String>>,
    /// Whether this host can raise privilege for the three operations
    /// that write a root-owned profile.
    ///
    /// A plain `bool` where every reading above is an `Option`, and for a
    /// reason rather than an oversight: the port cannot fail to answer
    /// it. `DeclarativeService::can_elevate` reports a capability the
    /// adapter either has or has not, so there is no unread state to
    /// distinguish - and `false`, the default before any refresh, is
    /// exactly what an unelevatable host looks like. The rows are marked
    /// either way; only the reason differs.
    pub can_elevate: bool,
    /// Whether this masys process is already root.
    ///
    /// A plain `bool` for `can_elevate`'s reason: the port cannot fail to
    /// answer it, and `false` is exactly what an unprivileged host looks
    /// like before the first refresh.
    pub is_root: bool,
}

impl NixBuffer {
    /// Every declarative reading, taken and kept.
    ///
    /// Sampled on the ordinary tick, at whatever cadence the caller runs
    /// one - two seconds in the binary.
    ///
    /// The design's table puts a platform read on 60s, and an earlier
    /// draft of this block carried a comment saying so while reading
    /// every tick. Only one of the two could be true, and it is this
    /// one, because the alternative costs more than it saves: `g`,
    /// "refresh now", reaches the session through the same `tick`, so a
    /// 60-second gate here would quietly make the one key that promises a
    /// fresh sample not deliver one. Telling a timed call from a keyed
    /// one means a `tick` signature the composition root has to thread a
    /// flag through, for a read this cheap. `masys::TICK` records the
    /// same decision for the same reason: one interval for everything
    /// until a buffer needs otherwise.
    ///
    /// What it costs, per tick: `canonicalize` on a handful of links, a
    /// `read_dir` of two profile directories and the unit directory, a
    /// `flake.lock` parse, and two small file reads. Closure sizes - the
    /// one genuinely expensive read - are deferred to `enter` on a row,
    /// not read on every tick.
    pub fn refresh(&mut self, declarative: &dyn DeclarativeService) {
        keep_last_good(&mut self.profiles, declarative.profiles().map(Some));
        keep_last_good(&mut self.reboot, declarative.reboot());
        keep_last_good(&mut self.inputs, declarative.inputs());
        keep_last_good(&mut self.home_mode, declarative.home_mode().map(Some));
        keep_last_good(&mut self.nixpkgs_rev, declarative.running_nixpkgs_rev());
        keep_last_good(&mut self.gc_retention, declarative.gc_retention().map(Some));
        keep_last_good(&mut self.gc_roots, declarative.gc_roots().map(Some));
        keep_last_good(&mut self.flake_ref, declarative.flake_ref().map(Some));
        keep_last_good(&mut self.nixos_config, declarative.nixos_config().map(Some));
        // Not keep-last-good: there is no failure to keep a good answer
        // over, and `PATH` can change under a session that was started
        // from a different one.
        self.can_elevate = declarative.can_elevate();
        self.is_root = declarative.is_root();
    }

    /// Every row this buffer shows, in order.
    ///
    /// Four facts cross, and each is one the session genuinely shares:
    /// the filesystems and the units both arrive in the single sample
    /// every buffer reads, the fold set is buffer-wide, and the clock is
    /// the tick's. Nothing else does - where this used to be handed two
    /// `Vec<Node>` that `App` had built, it builds them here, from units
    /// that were already crossing anyway.
    ///
    /// `folds` is keyed by `SectionKind::fold_key` rather than by the
    /// title, and `Store` is never looked up at all. `layout` carries
    /// the reasoning for both, beside the lookups that depend on it -
    /// this paragraph used to be a second copy of it, which is two places
    /// to correct when the rule changes and one of them a doc comment
    /// nothing would fail to update.
    pub fn rows<E: Expanded>(
        &self,
        filesystems: &[Filesystem],
        units: &[Unit],
        expanded: &E,
        folds: &HashSet<String>,
        now_ms: u64,
    ) -> Vec<Node> {
        // `/nix` where the store has a filesystem of its own, falling
        // back to `/` where it is a directory on the root one. `None`
        // where neither could be measured: `unwrap_or(0.0)` here would
        // draw "0% used, 0 bytes free" for a store nothing is known
        // about, which is a fabricated reading in the shape of a real one
        // and the failure this whole buffer keeps having to design against.
        let mount = |wanted: &str| filesystems.iter().find(move |fs| fs.mount_point == wanted);
        let store = mount("/nix").or_else(|| mount("/"));
        layout(
            self,
            store,
            now_ms,
            self.policy_rows(units, now_ms),
            self.nix_unit_rows(units, expanded, now_ms),
            folds,
        )
    }

    /// One verb-family's popup, with every row that cannot act here marked
    /// and the reason beside it.
    ///
    /// Unlike the unit popup this is not about the row: only the
    /// Generation family is, and it opens - dimmed, marked, never hidden -
    /// off a generation too, rather than only existing on one.
    pub fn family_transient(&self, family: NixFamily, selected: Option<&Node>) -> TransientDef {
        let generation = selected_generation(selected).map(|(generation, _)| generation.id);
        let retention = self.gc_retention.clone().flatten();
        self.mark(
            selected,
            nix_family_transient(family, generation, retention.as_deref()),
        )
    }

    /// The operation a key means here, or `None` where the key will not
    /// act - because the row cannot supply the arguments, or because
    /// masys cannot write what the operation writes.
    ///
    /// The one place that turns an intent into arguments, and the same
    /// one the footer asks whether to dim a key - so a key that is offered
    /// is a key that will act, and a key that will not act is marked. Two
    /// answers derived from one function cannot disagree; the version
    /// where the footer had a rule of its own is exactly how a footer
    /// comes to advertise a key that does nothing.
    pub fn offer(&self, selected: Option<&Node>, verb: NixVerb) -> NixOffer {
        match verb {
            // A bare `nixos-rebuild switch` is the correct command on a
            // channels host, not a broken one, so the rebuild verbs are
            // live whether or not a flake reference resolves. The adapter
            // appends `--flake <ref>` where one does.
            NixVerb::Rebuild(verb) => NixOffer::Ready(NixOp::Rebuild(verb)),
            NixVerb::Rollback => NixOffer::Ready(NixOp::Rollback),
            // Standalone only. In `Module` home-manager is activated by
            // `nixos-rebuild switch` and there is no `home-manager`
            // binary to run; `Absent` has no home-manager at all, and an
            // unread `home_mode` has not said either - none of the three
            // is a host this can run on.
            NixVerb::HomeSwitch => match self.home_mode {
                Some(HomeMode::Standalone) => NixOffer::Ready(NixOp::HomeSwitch),
                _ => NixOffer::No,
            },
            // The flake-only pair. Asked of the *reference*, not of the
            // paradigm `Inputs::source` reports, because the adapter
            // builds its argv from the reference: marking a row from one
            // fact and running it from another is how a popup comes to
            // mark a row that would have worked.
            NixVerb::FlakeUpdate => self.with_flake(NixOp::FlakeUpdate { input: None }),
            NixVerb::FlakeCheck => self.with_flake(NixOp::FlakeCheck),
            // And its mirror. `nix-channel --update` on a flake host
            // updates channels the configuration does not read.
            NixVerb::ChannelUpdate => self.with_channels(NixOp::ChannelUpdate),
            NixVerb::ChannelRollback => self.with_channels(NixOp::ChannelRollback),
            // Gated with the channel pair rather than with the rebuild
            // verbs it otherwise resembles. `--upgrade` updates channels
            // before it switches, and a flake host has none - so this is
            // the one rebuild form that is genuinely unavailable there,
            // where a bare `switch` is merely flakeless.
            NixVerb::Upgrade => self.with_channels(NixOp::Upgrade),
            // Live everywhere the rebuild verbs are, and for their reason:
            // a bare `nixos-rebuild repl` is the correct command on a
            // channels host, and the adapter appends `--flake` where one
            // resolves.
            NixVerb::Repl => NixOffer::Ready(NixOp::Repl),
            NixVerb::BuildVm => NixOffer::Ready(NixOp::BuildVm),
            NixVerb::ListImageVariants => NixOffer::Ready(NixOp::ListImageVariants),
            // The variant is typed: the candidates come from the row
            // above rather than from a picker masys has.
            NixVerb::BuildImage => NixOffer::Asks {
                prompt: "image variant",
            },
            // Neither has a source but the operator. A query is not a row
            // and an option name is not a reading, so these are the two
            // that would be unreachable without the input sub-step.
            NixVerb::SearchPackages => NixOffer::Asks {
                prompt: "search nixpkgs",
            },
            // `nixos-option` reads `<nixos-config>` and there is no flag
            // to hand it one. On a flake host that sets no `nix.nixPath`
            // it fails with *file 'nixos-config' was not found in the Nix
            // search path* before it evaluates anything - measured on the
            // development host, which is exactly that shape.
            NixVerb::OptionValue => match self.nixos_config.as_ref().and_then(|read| read.as_ref())
            {
                Some(_) => NixOffer::Asks { prompt: "option" },
                None => NixOffer::No,
            },
            // The generation under the cursor supplies the spec, so this
            // one asks for nothing. The profile still has to be writable
            // - `nix-env` has no elevation flag, the same reason
            // `Activate` checks.
            NixVerb::DeleteHere => {
                let Some((generation, kind)) = selected_generation(selected) else {
                    return NixOffer::No;
                };
                let Some(profile) = self.profile(kind) else {
                    return NixOffer::No;
                };
                if !self.may_write(profile) {
                    return NixOffer::No;
                }
                NixOffer::Ready(NixOp::DeleteGenerations {
                    profile: profile.path.clone(),
                    spec: generation.id.to_string(),
                })
            }
            // The escape hatch: `+5`, `30d` and a list of numbers are all
            // specs and none of them is a row. Offered against the system
            // profile, which is the one a generation row is about.
            NixVerb::DeleteGenerations => match self.profile(ProfileKind::System) {
                Some(profile) if self.may_write(profile) => NixOffer::Asks {
                    prompt: "delete generations",
                },
                _ => NixOffer::No,
            },
            NixVerb::Activate => {
                let Some((generation, kind)) = selected_generation(selected) else {
                    return NixOffer::No;
                };
                // System generations only, and not out of caution.
                // `NixOp::Activate`'s second command is the profile's own
                // `bin/switch-to-configuration switch`, and that script
                // exists in a system generation and nowhere else -
                // measured here: `/nix/var/nix/profiles/system/bin` holds
                // exactly that one file, while
                // `~/.local/state/nix/profiles/profile/bin` holds no
                // activation script at all. Offering the key on a home or
                // channels row would switch the profile and then fail on
                // a missing file, leaving it moved with nothing
                // activated. Widening this means teaching
                // `ops::activation_argv` the other profiles' scripts
                // first.
                if kind != ProfileKind::System {
                    return NixOffer::No;
                }
                let Some(profile) = self.profile(kind) else {
                    return NixOffer::No;
                };
                // The profile directory has to be writable, and this is
                // the one place in masys where a privilege is checked
                // ahead of an operation rather than left to the command.
                // Every other privileged path has a mechanism of its own:
                // a unit verb shells out to `systemctl`, which triggers
                // polkit, so an unprivileged masys can be granted the
                // action; `nixos-rebuild` has `--elevate {none,sudo,run0}`
                // and can deploy as a non-root user. `nix-env` has
                // neither - its synopsis carries no such flag - so there
                // is nothing to grant and nothing to ask.
                //
                // Refused rather than attempted, even though this half is
                // safe on its own: an unprivileged `a` fails at
                // `nix-env`, changes nothing, and would cost only a
                // confirmation and an error. It is marked anyway so that
                // `a` and `c` answer the same question the same way, and
                // because a key that has never once worked for the
                // ordinary invocation should say so before it is pressed
                // and not after. Run masys as root and the directory is
                // writable and the key is live.
                if !self.may_write(profile) {
                    return NixOffer::No;
                }
                NixOffer::Ready(NixOp::Activate {
                    profile: profile.path.clone(),
                    generation: generation.id,
                })
            }
            NixVerb::Diff => {
                let Some((generation, kind)) = selected_generation(selected) else {
                    return NixOffer::No;
                };
                // Both paths must be *known*. A dangling generation link
                // carries `None`, and `nix store diff-closures` given one
                // path would either fail or - worse, if the two unknowns
                // were papered over with a default - diff something
                // against itself and report no change.
                let Some(from) = generation.store_path.clone() else {
                    return NixOffer::No;
                };
                let Some(to) = self.current_store_path(kind) else {
                    return NixOffer::No;
                };
                // The row under the cursor is the left-hand side and
                // current is the right, so the output reads forwards:
                // what changed getting from there to here.
                NixOffer::Ready(NixOp::Diff { from, to })
            }
            // This host's own declared retention, never a number masys
            // chose. `nix-gc.service` runs `--delete-older-than 14d`
            // here, and that value is one somebody wrote into their
            // configuration; a built-in default would be masys deleting
            // generations on a number nobody chose, which is the
            // fabrication rule applied to an act instead of to a reading.
            //
            // Both `None`s mean the same thing for this purpose and both
            // dim the key: the outer is "no read has succeeded", the
            // inner is the port's own answer that the gc job declares no
            // retention. Neither is a period to delete generations by.
            NixVerb::Clean => {
                // Every profile, and this one is not caution - it is the
                // difference between a refusal and an irreversible half
                // of an act. nix-collect-garbage(1) for nix 2.34.8 here:
                // "it looks in a few locations, and acts on all profiles
                // it finds there", and "Deleting previous configurations
                // makes rollbacks to them impossible." Run unprivileged
                // on this host it would delete the operator's own
                // home-manager generations - `~/.local/state/nix/profiles`
                // is owned by the operator and world-unwritable - and then
                // fail on the
                // root-owned system profile, reporting a non-zero exit
                // and nothing about what had already gone.
                //
                // `nix-collect-garbage`'s synopsis is `[--delete-old]
                // [-d] [--delete-older-than period] [--max-freed bytes]
                // [--dry-run]`: no elevation flag, so unlike a unit verb
                // there is no polkit action to be granted and unlike
                // `nixos-rebuild` no `--elevate` to pass. Declining to
                // start it is the only lever masys has.
                //
                // `all` over a *non-empty* list. An empty one is
                // vacuously true, and it means masys found no profile to
                // check rather than that every profile passed - the same
                // shape as a fabricated zero, one level up. Nix also
                // finds profiles masys never reports, per-user ones among
                // them; this is an approximation, and it is sound in the
                // direction that matters, because a host where
                // `/nix/var/nix/profiles` is writable is a host running
                // as root, where the rest are too.
                let Some(profiles) = self.profiles.as_ref() else {
                    return NixOffer::No;
                };
                let all_writable = !profiles.is_empty()
                    && profiles
                        .iter()
                        .all(|profile| profile.writable == Some(true));
                if !all_writable && !self.can_elevate {
                    return NixOffer::No;
                }
                match self.gc_retention.clone().flatten() {
                    Some(older_than) => NixOffer::Ready(NixOp::Clean { older_than }),
                    None => NixOffer::No,
                }
            }
        }
    }

    /// The operation an `Asks` verb means once the operator has typed a
    /// value. Separate from [`NixBuffer::offer`] only because the value
    /// does not exist when the offer is made; every precondition is still
    /// asked there and nowhere else.
    pub fn op_typed(&self, verb: NixVerb, typed: &str) -> Option<NixOp> {
        match verb {
            NixVerb::SearchPackages => Some(NixOp::SearchPackages {
                query: typed.to_string(),
            }),
            NixVerb::OptionValue => Some(NixOp::OptionValue {
                name: typed.to_string(),
            }),
            NixVerb::BuildImage => Some(NixOp::BuildImage {
                variant: typed.to_string(),
            }),
            NixVerb::DeleteGenerations => {
                let profile = self.profile(ProfileKind::System)?;
                Some(NixOp::DeleteGenerations {
                    profile: profile.path.clone(),
                    spec: typed.to_string(),
                })
            }
            // Every other verb resolved its arguments at offer time.
            _ => None,
        }
    }

    /// The declared maintenance policy, as rows.
    ///
    /// The schedule comes from the timer units masys already polls; only
    /// the retention comes from the declarative port. Asking that port for
    /// a schedule would be it reimplementing systemd, and the two would
    /// then disagree about a timer the operator had overridden.
    ///
    /// A job whose timer unit is not on this host contributes no row.
    /// Absent is not empty: "nix-optimise is not scheduled" and
    /// "nix-optimise is scheduled and has never run" send an operator
    /// looking in two different places.
    fn policy_rows(&self, units: &[Unit], now_ms: u64) -> Vec<Node> {
        [("gc", "nix-gc.timer"), ("optimise", "nix-optimise.timer")]
            .into_iter()
            .filter_map(|(job, unit_name)| {
                let unit = units.iter().find(|unit| unit.name == unit_name)?;
                let timer = unit.timer.as_ref();
                Some(Node::NixPolicy {
                    job: job.to_string(),
                    unit: unit_name.to_string(),
                    // Only the gc job has one. Nothing is retained by
                    // optimising, so a retention on that row would be a
                    // column borrowed from its neighbour.
                    // `Some(None)` for optimise, not `None`: optimising
                    // retains nothing at all, which is a fact about the
                    // job rather than a reading that failed. `-` would
                    // say masys could not find out.
                    retention: if job == "gc" {
                        self.gc_retention.clone()
                    } else {
                        Some(None)
                    },
                    next_in_ms: timer
                        .and_then(|timer| timer.next_ms)
                        .map(|next| next.saturating_sub(now_ms)),
                    last_ago_ms: timer
                        .and_then(|timer| timer.last_ms)
                        .map(|last| now_ms.saturating_sub(last)),
                    enabled: unit.enabled,
                })
            })
            .collect()
    }

    /// Nix's own units, as rows.
    ///
    /// The cheapest section in the buffer: a filter over the units masys
    /// already polls every tick, so it costs one predicate and no read.
    /// It answers what nothing else in this buffer does - whether the daemon
    /// is up, whether this boot's home-manager activation actually
    /// succeeded - which is the running end of declared, realised,
    /// running.
    ///
    /// Built here rather than in `Self::rows` for the reason
    /// `Self::policy_rows` is: this is the layer that holds the poll and the
    /// map of which units are open.
    ///
    /// Worst first, by `systemd_buffer::severity`, so a failed
    /// `nix-gc.service` is the first row under the heading rather than
    /// somewhere in an alphabetical six.
    fn nix_unit_rows<E: Expanded>(&self, units: &[Unit], expanded: &E, now_ms: u64) -> Vec<Node> {
        let mut units: Vec<&masys_domain::unit::Unit> = units
            .iter()
            .filter(|unit| is_nix_unit(&unit.name))
            .collect();
        units.sort_by(|a, b| {
            severity(a.active_state)
                .cmp(&severity(b.active_state))
                .then_with(|| a.name.cmp(&b.name))
        });
        // Flat, at depth 0 and with no `children` marker: the causal tree
        // the systemd buffer draws under a socket or a timer would list
        // `nix-daemon.service` twice in a section of six, once on its own
        // and once under its socket.
        units
            .into_iter()
            .flat_map(|unit| unit_rows(unit, expanded, now_ms, 0, false))
            .collect()
    }

    /// Marks the rows of a Nix transient that cannot act here, and says
    /// why.
    ///
    /// Asked of `Self::offer`, which is also what the footer asks, so a
    /// marked row and a dim key cannot disagree about the same verb.
    /// `NixOffer::Asks` is *not* marked: the row is live, and pressing it
    /// opens the prompt.
    fn mark(&self, selected: Option<&Node>, def: TransientDef) -> TransientDef {
        TransientDef {
            groups: def
                .groups
                .into_iter()
                .map(|group| DefGroup {
                    rows: group
                        .rows
                        .into_iter()
                        .map(|row| match row.action {
                            Action::Nix(verb) => DefRow {
                                dimmed: self.offer(selected, verb) == NixOffer::No,
                                note: self.note(selected, verb),
                                ..row
                            },
                            _ => row,
                        })
                        .collect(),
                    ..group
                })
                .collect(),
            ..def
        }
    }

    /// What to say beside a Nix row - why it will not act, or what it
    /// will act with.
    ///
    /// A note is not the same question as a mark. `clean` is live and
    /// still worth annotating with the period it will use, because
    /// "delete old generations" without a number is the row an operator
    /// should not press.
    fn note(&self, selected: Option<&Node>, verb: NixVerb) -> Option<String> {
        let flake = self
            .flake_ref
            .as_ref()
            .and_then(|read| read.as_ref())
            .is_some();
        let channels = matches!(
            self.inputs.as_ref().map(|inputs| &inputs.source),
            Some(InputSource::Channels)
        );
        match verb {
            NixVerb::FlakeUpdate | NixVerb::FlakeCheck if !flake => {
                Some("no flake configured".to_string())
            }
            NixVerb::ChannelUpdate | NixVerb::ChannelRollback if !channels => {
                Some("no channels on this host".to_string())
            }
            // Names what is missing rather than the paradigm, because a
            // flake host *can* have one: `nix.nixPath` puts it there, and
            // then the row is live.
            NixVerb::OptionValue
                if self
                    .nixos_config
                    .as_ref()
                    .and_then(|read| read.as_ref())
                    .is_none() =>
            {
                Some("no nixos-config on the search path".to_string())
            }
            // The design's own wording. In `Module` there is no
            // `home-manager` binary at all, so the row is not merely
            // ineffective - the command does not exist.
            NixVerb::HomeSwitch => match self.home_mode {
                Some(HomeMode::Module) => Some("activated by nixos-rebuild switch".to_string()),
                Some(HomeMode::Absent) => Some("home-manager not installed".to_string()),
                Some(HomeMode::Standalone) => None,
                None => Some("home-manager mode unread".to_string()),
            },
            // The host's declared retention, on the row that will use it.
            // Where none is declared the row is marked, and this says
            // which of the two absences it is - masys refusing to pick a
            // period is not the same as masys failing to read one.
            // Both facts, where both apply. The retention is what the key
            // will delete by and the note is what it will ask for first,
            // and an operator wants the period most on the row that is
            // about to run as root. Only where masys cannot elevate does
            // the privilege note stand alone: there the key does nothing,
            // so the period it would have used is not the useful half.
            NixVerb::Clean => {
                let retention = match self.gc_retention.clone() {
                    Some(Some(period)) => Some(format!("--delete-older-than {period}")),
                    Some(None) => Some("no retention declared".to_string()),
                    None => Some("retention unread".to_string()),
                };
                match (self.unwritable_note(), retention) {
                    (Some(privilege), _) if !self.can_elevate => Some(privilege),
                    (Some(privilege), Some(retention)) => {
                        Some(format!("{privilege} . {retention}"))
                    }
                    (privilege, retention) => privilege.or(retention),
                }
            }
            // The three that write a profile. `nix-env` has no elevation
            // flag - no polkit action to be granted, no `--elevate` to
            // pass - so the only lever masys has is to say so before the
            // key is pressed. A marked row with no reason beside it is
            // the one thing the design's marking rule exists to prevent.
            NixVerb::Activate => match selected_generation(selected) {
                Some((_, ProfileKind::System)) => self.profile_note(ProfileKind::System),
                // The second command is the profile's own
                // `bin/switch-to-configuration`, and only a system
                // generation has one.
                Some(_) => Some("only a system generation activates".to_string()),
                // The Generation family's own popup opens off a
                // generation row too, now that it has a top-level letter
                // of its own rather than only existing when the row
                // supplied it - so this case is reachable, and a marked
                // row still needs its reason.
                None => Some("no generation under the cursor".to_string()),
            },
            NixVerb::DeleteHere => match selected_generation(selected) {
                Some((_, kind)) => self.profile_note(kind),
                None => Some("no generation under the cursor".to_string()),
            },
            NixVerb::Diff if selected_generation(selected).is_none() => {
                Some("no generation under the cursor".to_string())
            }
            NixVerb::DeleteGenerations => self.profile_note(ProfileKind::System),
            // The five rebuild verbs differ only in what they do, and
            // their names do not say it - `boot` and `test` are nearly
            // opposites and both are one word. The confirmation's own
            // sentence is the row's note, so there is one description of
            // each verb rather than two that can drift apart.
            //
            // `build` never elevates - `ops::elevation` calls it "the one
            // verb that stops at a store path" - so it is the one rebuild
            // row #14 leaves alone. Every other one asks polkit for the
            // same privilege a `systemctl` verb does, and `can_elevate`
            // only answers whether the mechanism exists, not whether the
            // policy lets this operator through - that costs the build
            // first and answers after. Saying so is cheaper than measuring
            // it, and costs a root masys nothing.
            NixVerb::Rebuild(RebuildVerb::Build) => Some(verb.label().to_string()),
            NixVerb::Rebuild(_) | NixVerb::Rollback | NixVerb::Upgrade if !self.is_root => Some(
                format!("{} . activation needs polkit or root", verb.label()),
            ),
            NixVerb::Rebuild(_) | NixVerb::Rollback | NixVerb::Upgrade => {
                Some(verb.label().to_string())
            }
            _ => None,
        }
    }

    /// The store path a profile is pointing at now.
    ///
    /// Read from the facts rather than from the rows: the current
    /// generation's row can be folded away or filtered out while the
    /// cursor sits on an old one, and "against current" must not change
    /// meaning because a section was collapsed.
    ///
    /// `None` where no generation is marked current, and - separately -
    /// where the current one's link could not be resolved. Both are
    /// unknowns rather than answers, and an operation built on one would
    /// name a store path masys never read.
    fn current_store_path(&self, kind: ProfileKind) -> Option<String> {
        let profiles = self.profiles.as_ref()?;
        let profile = profiles.iter().find(|profile| profile.kind == kind)?;
        profile
            .generations
            .iter()
            .find(|generation| generation.current)?
            .store_path
            .clone()
    }

    /// One profile, from the last good read.
    ///
    /// The whole record rather than just its path, because an operation
    /// needs two things off it: where the profile lives, and whether this
    /// process can write it. Both are the adapter's to report rather than
    /// this crate's to work out - the home profile moved from
    /// `~/.nix-profile` to `~/.local/state/nix/profiles/profile`, and
    /// masys-app performs no I/O at all.
    fn profile(&self, kind: ProfileKind) -> Option<&Profile> {
        self.profiles
            .as_ref()?
            .iter()
            .find(|profile| profile.kind == kind)
    }

    /// `Ready(op)` where this host has a flake reference, `No` where it
    /// has none or the read has not come back.
    fn with_flake(&self, op: NixOp) -> NixOffer {
        match self.flake_ref.as_ref().and_then(|read| read.as_ref()) {
            Some(_) => NixOffer::Ready(op),
            None => NixOffer::No,
        }
    }

    /// And its mirror, for the two channel operations.
    ///
    /// Asked of `Inputs::source` rather than of "no flake reference",
    /// because those are not complements: a host can have neither, and
    /// offering `nix-channel --update` on one that has never had a
    /// channel would be a row live for want of a flake rather than for
    /// having channels.
    fn with_channels(&self, op: NixOp) -> NixOffer {
        match self.inputs.as_ref().map(|inputs| &inputs.source) {
            Some(InputSource::Channels) => NixOffer::Ready(op),
            _ => NixOffer::No,
        }
    }

    /// Whether this operator can write a profile, directly or by asking.
    ///
    /// Two facts rather than one, and they are not the same question.
    /// `writable` is what the kernel already answered about this
    /// directory; `can_elevate` is whether the host has a way to run one
    /// command as root. Either is enough for the row to act.
    fn may_write(&self, profile: &Profile) -> bool {
        profile.writable == Some(true) || self.can_elevate
    }

    /// What a profile-writing row costs, or `None` where it costs
    /// nothing.
    ///
    /// Three answers, because there are three states. The profile is
    /// already this operator's, and the row is plain. It is not, but the
    /// host can raise privilege, so the row says the key will ask for it,
    /// which is the design's rule that a key states what it costs before
    /// it is pressed rather than being taken away. Or it is neither, and
    /// the row is marked as it always was.
    fn profile_note(&self, kind: ProfileKind) -> Option<String> {
        match self.profile(kind) {
            Some(profile) if profile.writable == Some(true) => None,
            Some(_) if self.can_elevate => Some("asks for root".to_string()),
            Some(_) => Some("profile is read-only - run masys as root".to_string()),
            None => Some("no profile read".to_string()),
        }
    }

    /// The same, for `clean`, which acts on every profile it finds rather
    /// than the one under the cursor.
    fn unwritable_note(&self) -> Option<String> {
        let profiles = self.profiles.as_ref()?;
        let all_writable = !profiles.is_empty()
            && profiles
                .iter()
                .all(|profile| profile.writable == Some(true));
        if all_writable {
            return None;
        }
        match self.can_elevate {
            true => Some("asks for root".to_string()),
            false => Some("every profile must be writable - run masys as root".to_string()),
        }
    }
}

/// The generation under the cursor, and which profile it came from.
///
/// The profile as well as the generation, because an id alone does not
/// identify one: this host's system profile is at generation 438 and
/// its home profile at 47, and both numbering schemes start at 1. Every
/// operation on a generation needs to know whose it is.
fn selected_generation(node: Option<&Node>) -> Option<(&Generation, ProfileKind)> {
    match node {
        Some(Node::Generation {
            generation, kind, ..
        }) => Some((generation, *kind)),
        _ => None,
    }
}

/// Takes a port's answer, and keeps the last good one where the read
/// failed.
///
/// The rule every declarative read follows, named once rather than
/// restated seven times. `Err` from these methods means "could not find
/// out", and storing what `unwrap_or_default` would produce - no
/// generations, no reboot pending, no gc policy, nothing pinning the
/// store - turns a read failure into a positive claim about the host.
/// Every one of those claims is actionable, and every one of them is
/// wrong.
///
/// `Ok(None)` still overwrites, and has to: the port documents `None` as
/// a real answer wherever it returns one - booted and current are the same
/// store path, this configuration pins nothing - so holding a stale `Some`
/// over it would leave a reboot showing as pending after the reboot.
pub(crate) fn keep_last_good<T>(slot: &mut T, read: Result<T, MasysError>) {
    if let Ok(reading) = read {
        *slot = reading;
    }
}

/// Every row the Nix buffer shows, in order.
///
/// A pure function of facts: everything it needs is an argument, including
/// the clock, so the whole layout is testable without a NixOS host.
///
/// Private, and an *internal* seam rather than this module's interface.
/// Callers reach it through [`NixBuffer::rows`].
///
/// Takes the buffer rather than its readings one at a time. It had twelve
/// parameters, six of which were `NixBuffer` fields being unpacked at the
/// call and named again here - which is the shape 5539317 split this
/// module apart to end, reappearing one level down. The `#[allow]` that
/// used to sit here was the tell: a lint suppressed rather than answered.
///
/// Still a pure function of facts, which is what the seam is for.
/// `NixBuffer` holds only readings, its fields are public and it derives
/// `Default`, so a test builds the host it wants and calls `rows` - no
/// NixOS needed, and no twelve-argument call to get wrong. `store` is one
/// `Filesystem` rather than a percentage and a byte count travelling in
/// pairs, so the two halves cannot come from different mounts.
///
/// `policies` and `nix_units` are taken by value rather than by slice
/// because `Node` is not `Clone` - it carries a boxed `UnitDetail` and a
/// dozen sample types, and each is only ever built to be handed straight
/// over.
///
/// `collapsed` is keyed by `SectionKind::fold_key`, never by the title.
/// Two of this buffer's titles change under the operator: the Inputs
/// heading carries the drift verdict, and the Home-manager one carries
/// how it is deployed, so a rebuild in another terminal would rename a
/// folded section and quietly reopen it. `Store` is never looked up at
/// all: it always renders in full, the same reason `System` is never
/// checked against the Status buffer's own `collapsed`.
fn layout(
    buffer: &NixBuffer,
    store: Option<&Filesystem>,
    now_ms: u64,
    policies: Vec<Node>,
    nix_units: Vec<Node>,
    collapsed: &HashSet<String>,
) -> Vec<Node> {
    let profiles = buffer.profiles.as_deref();
    let reboot = buffer.reboot.as_ref();
    let inputs = buffer.inputs.as_ref();
    let running_nixpkgs_rev = buffer.nixpkgs_rev.as_deref();
    let gc_roots = buffer.gc_roots;
    // `Absent` for a reading that has not come back, which is not the
    // fabrication it looks like. This is used for exactly one thing -
    // whether the home-manager heading says it is activated by
    // `nixos-rebuild` - and `Absent` is its neutral case, identical to
    // `Standalone`. The claim, `Module`, is only ever made from a read
    // that succeeded.
    let home_mode = buffer.home_mode.unwrap_or(HomeMode::Absent);
    // Split here rather than at the call, so the two halves of one
    // reading cannot arrive from different filesystems.
    let store_used_percent = store.map(|fs| fs.used_percent);
    let store_free_bytes = store.map(|fs| fs.free_bytes);

    let mut rows = Vec::new();
    // `None` profiles is "the read has not come back", and every count
    // below it is `None` too - see `Node::NixStore`. An empty slice is a
    // different fact, and the one the port documents: no profile
    // directory on this host holds a generation.
    let known = profiles.is_some();
    let profiles = profiles.unwrap_or_default();
    let find = |kind: ProfileKind| profiles.iter().find(|profile| profile.kind == kind);
    // Absent still counts as zero, and only inside a read that succeeded:
    // a host with no home-manager has no home generations, which is an
    // answer. `known` is what separates that from never having looked.
    let count = |kind: ProfileKind| {
        known.then(|| {
            find(kind)
                .map(|profile| profile.generations.len() as u32)
                .unwrap_or(0)
        })
    };

    if let Some(state) = reboot {
        // Matched on `Some(path)` only. A generation whose link dangles
        // carries `None`, and must never be mistaken for the booted one
        // just because neither could be resolved.
        let id_of = |path: &str| {
            let generations = &find(ProfileKind::System)?.generations;
            generations
                .iter()
                .find(|generation| generation.store_path.as_deref() == Some(path))
                .map(|g| g.id)
        };
        rows.push(Node::RebootPending {
            booted: id_of(&state.booted_store_path),
            current: id_of(&state.current_store_path),
            kernel_changed: state.kernel_changed,
            initrd_changed: state.initrd_changed,
        });
        rows.push(Node::Spacer);
    }

    // Always rendered, because it carries the declared policy rather than
    // a complaint - this buffer's equivalent of the Status buffer's System
    // section. What varies is whether its rows are marked.
    rows.push(Node::SectionHeader {
        title: "Store".to_string(),
        kind: SectionKind::NixStore,
        count: None,
    });
    rows.push(Node::NixStore {
        used_percent: store_used_percent,
        free_bytes: store_free_bytes,
        system_generations: count(ProfileKind::System),
        home_generations: count(ProfileKind::Home),
        gc_roots,
    });
    rows.extend(policies);
    rows.push(Node::Spacer);

    for profile in profiles {
        // Absent is not empty. `/nix/var/nix/profiles/per-user/root`
        // exists and holds nothing on a flake host, and a "Channels 0"
        // header sends the operator hunting for channels they do not use.
        if profile.generations.is_empty() {
            continue;
        }
        let kind = SectionKind::Generations(profile.kind);
        let title = section_title(profile.kind, home_mode, profile.generations.len() as u32);
        rows.push(Node::SectionHeader {
            title: title.clone(),
            kind,
            count: None,
        });
        // Folded: the header stays, so the section can be reopened, but
        // none of its rows are built - the same trade `build_unit_rows`
        // makes for a collapsed unit type.
        if !collapsed.contains(&kind.fold_key(&title)) {
            // Newest first. The generation anyone wants is the last one or
            // the one before it, and enough rows accumulate that ascending
            // order would put both off the screen.
            for generation in profile.generations.iter().rev() {
                rows.push(Node::Generation {
                    // `None` stays `None`: an unreadable mtime renders as
                    // `-`, not as an age computed from a fabricated epoch.
                    age_ms: generation
                        .created_ms
                        .map(|created| now_ms.saturating_sub(created)),
                    generation: generation.clone(),
                    kind: profile.kind,
                });
            }
        }
        rows.push(Node::Spacer);
    }

    if let Some(inputs) = inputs {
        let title = inputs_title(inputs, running_nixpkgs_rev);
        rows.push(Node::SectionHeader {
            title: title.clone(),
            kind: SectionKind::Inputs,
            count: None,
        });
        if !collapsed.contains(&SectionKind::Inputs.fold_key(&title)) {
            let now_secs = now_ms / 1000;
            for input in &inputs.inputs {
                rows.push(Node::Input {
                    age_days: input
                        .last_modified_secs
                        .map(|then| (now_secs.saturating_sub(then) / 86_400) as u32),
                    input: input.clone(),
                });
            }
        }
        rows.push(Node::Spacer);
    }

    // Last, because it is the only section about what is *running*:
    // everything above it is what the configuration declares and what was
    // realised from it. Absent rather than empty on a host where the poll
    // found none of them - the same rule every other section here follows.
    if !nix_units.is_empty() {
        // Counted before the fold, and counting only the unit rows: an
        // open unit contributes a `UnitDetail` beside its `Unit`, and a
        // heading that grew by one because somebody pressed `enter` would
        // be reporting the cursor rather than the host.
        let count = nix_units
            .iter()
            .filter(|row| matches!(row, Node::Unit { .. }))
            .count() as u32;
        let title = "Nix units".to_string();
        rows.push(Node::SectionHeader {
            title: title.clone(),
            kind: SectionKind::NixUnits,
            count: Some(count),
        });
        if !collapsed.contains(&SectionKind::NixUnits.fold_key(&title)) {
            rows.extend(nix_units);
        }
        rows.push(Node::Spacer);
    }

    // The spacer between sections is a separator, not a trailing margin -
    // the last one would draw an empty selectable-looking line at the
    // bottom of the buffer. Same trim as `build_io_rows`.
    if matches!(rows.last(), Some(Node::Spacer)) {
        rows.pop();
    }
    rows
}

/// A section's heading. The home-manager one carries how it is deployed,
/// because a Module install has no switch of its own and the header is
/// where that belongs - not on a row.
///
/// The count is folded in here rather than left to `section_header_line`'s
/// `count` parameter, which appends it after whatever the title already
/// says: for the Module case that put the row count after "(activated by
/// nixos-rebuild)" instead of beside the name it counts. Folding it in
/// puts the count where the design's mock has it - beside the name, any
/// further annotation to the right - for every profile kind, not only the
/// one that broke.
fn section_title(kind: ProfileKind, home_mode: HomeMode, count: u32) -> String {
    match kind {
        ProfileKind::System => format!("System generations ({count})"),
        ProfileKind::Channels => format!("Channel generations ({count})"),
        ProfileKind::Home => match home_mode {
            HomeMode::Module => format!("Home-manager ({count})  (activated by nixos-rebuild)"),
            _ => format!("Home-manager ({count})"),
        },
    }
}

/// The Inputs heading, which is where drift is reported.
///
/// Comparing the lock's nixpkgs revision against the running system's
/// answers a question nothing else asks: whether someone ran `nix flake
/// update` and has not rebuilt. That is the same shape as booted differing
/// from current, one level further out - declared, realised, running.
///
/// Both revisions have to be known for the comparison to mean anything. A
/// channel-built system reports no revision, and a lock with no `nixpkgs`
/// pins none; either way the heading says "Inputs" and claims nothing,
/// rather than reporting drift from a revision it never had.
///
/// The input count is folded in beside the name for the same reason
/// `section_title` folds in its own: `section_header_line`'s `count`
/// would otherwise land after "in sync" or "drift - not rebuilt" and read
/// as part of the verdict rather than as the row count it is.
fn inputs_title(inputs: &Inputs, running: Option<&str>) -> String {
    let count = inputs.inputs.len();
    let locked = inputs
        .inputs
        .iter()
        .find(|input| input.name == "nixpkgs")
        .and_then(|input| input.rev.as_deref());
    let short = |rev: &str| rev.chars().take(7).collect::<String>();
    match (locked, running) {
        (Some(locked), Some(running)) if locked == running => {
            format!(
                "Inputs ({count})  lock {} . running {} . in sync",
                short(locked),
                short(running)
            )
        }
        (Some(locked), Some(running)) => {
            format!(
                "Inputs ({count})  lock {} . running {} . drift - not rebuilt",
                short(locked),
                short(running)
            )
        }
        _ => format!("Inputs ({count})"),
    }
}

/// Whether a unit is Nix's own.
///
/// The section this feeds costs one predicate over units masys already
/// polls every tick: `nix-daemon.service` and its socket, `nix-store.mount`,
/// `nix-gc.service`, `nix-optimise.service` and the generated
/// `home-manager-<user>.service` on this host. Prefixes rather than a
/// list, because the home-manager unit's name carries the user, and
/// `nixos-upgrade.service` exists only where `system.autoUpgrade` is set -
/// not here, which is why it is covered by a prefix rather than asserted
/// against a reading.
///
/// The two maintenance *timers* are deliberately out. The Store section
/// already reports both, with their schedules, their last run and their
/// retention, which is more than a unit row says about either; listing
/// them again a few lines below is the same two lines twice. Their
/// *services* stay in, because `nix-gc.service` is where a garbage
/// collection is actually started from and where its last run failed.
pub fn is_nix_unit(name: &str) -> bool {
    if matches!(name, "nix-gc.timer" | "nix-optimise.timer") {
        return false;
    }
    ["nix-", "nixos-", "home-manager-"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// One Nix verb-family's popup, as the buffer's family menus draw them.
///
/// The *shape* only: every row is built live, and `NixBuffer::mark` marks
/// the ones that cannot act here from `offer` - the same function the
/// footer asks, so a marked row and a dim key can never disagree.
/// Building the shape here and marking it there is what keeps this
/// function free of the eleven facts the marking needs.
///
/// `gc now` and `optimise now` are not here, though an earlier mockup drew
/// `o optimise`. Both units have rows in this buffer's own units section
/// and `s` (systemctl start, unchanged) already starts them; routing them
/// through a family menu instead would need an action that names its unit
/// rather than taking the cursor's, for two rows that already work.
pub fn nix_family_transient(
    family: NixFamily,
    generation: Option<u64>,
    retention: Option<&str>,
) -> TransientDef {
    match family {
        NixFamily::Rebuild => nix_rebuild_transient(),
        NixFamily::Generation => nix_generation_transient(generation),
        NixFamily::Inputs => nix_inputs_transient(),
        NixFamily::Store => nix_store_transient(retention),
        NixFamily::Search => nix_search_transient(),
    }
}

/// `switch` is bound directly (`s`, no popup) as well as listed here -
/// two doors to the same row, the way a promoted verb stays in place
/// rather than leaving a gap, so `?` help and muscle memory built from the
/// popup both still find it.
///
/// The `-r` switch replaces the old standalone `Rollback` row.
/// `NixOp::Rollback`'s own doc says `--rollback` is a flag on `switch`,
/// not a fourteenth action - it was a row of its own only because
/// switches did not exist in this popup yet. Armed, `switch` dispatches
/// `NixOp::Rollback` instead; see `App::answer_transient`.
fn nix_rebuild_transient() -> TransientDef {
    let rows = [
        ('s', RebuildVerb::Switch),
        ('b', RebuildVerb::Boot),
        ('t', RebuildVerb::Test),
        ('B', RebuildVerb::Build),
        ('y', RebuildVerb::DryActivate),
    ]
    .into_iter()
    .map(|(chord, verb)| row(chord, NixVerb::Rebuild(verb)))
    .chain([
        row('p', NixVerb::Repl),
        row('v', NixVerb::BuildVm),
        row('i', NixVerb::ListImageVariants),
        row('I', NixVerb::BuildImage),
    ])
    .collect();

    TransientDef {
        title: "Rebuild".to_string(),
        switches: vec![
            SwitchRow {
                group: "Rebuild",
                chord: "-r",
                label: "rollback: activate the previous generation instead",
                supported: true,
                on: false,
            },
            // The same shape as `-r` and for the same reason: `--upgrade`
            // is a flag on `switch`, not a fourteenth action, so it is a
            // switch rather than a row. Listed after `-r`, which is also
            // the order `answer_transient` checks them in - the two are
            // contradictory (`--rollback` builds nothing, so channels
            // updated on the way to it would go unread) and `-r` wins.
            SwitchRow {
                group: "Rebuild",
                chord: "-u",
                label: "upgrade: update the nixos channel first",
                supported: true,
                on: false,
            },
        ],
        groups: vec![DefGroup {
            heading: "Rebuild".to_string(),
            note: None,
            rows,
        }],
    }
}

/// Named for the generation it acts on where one is under the cursor -
/// `activate` and `delete` with no number in the heading would be two of
/// the most destructive rows in masys with nothing on screen saying what
/// they act on. Opens without one too, now that `a` reaches it directly
/// rather than only existing when a generation row supplied it; every row
/// is marked and says why, the same rule every other popup in masys
/// follows.
fn nix_generation_transient(generation: Option<u64>) -> TransientDef {
    let heading = match generation {
        Some(id) => format!("Generation {id}"),
        None => "Generation".to_string(),
    };
    TransientDef {
        title: "Generation".to_string(),
        switches: Vec::new(),
        groups: vec![DefGroup {
            heading,
            note: None,
            rows: vec![
                row('a', NixVerb::Activate),
                row('d', NixVerb::Diff),
                row('x', NixVerb::DeleteHere),
            ],
        }],
    }
}

fn nix_inputs_transient() -> TransientDef {
    TransientDef {
        title: "Inputs".to_string(),
        switches: Vec::new(),
        groups: vec![DefGroup {
            heading: "Inputs".to_string(),
            note: None,
            rows: vec![
                row('f', NixVerb::FlakeUpdate),
                row('F', NixVerb::FlakeCheck),
                row('n', NixVerb::ChannelUpdate),
                row('N', NixVerb::ChannelRollback),
            ],
        }],
    }
}

fn nix_store_transient(retention: Option<&str>) -> TransientDef {
    TransientDef {
        title: "Store".to_string(),
        switches: Vec::new(),
        groups: vec![DefGroup {
            // The declared policy, stated rather than warned about - it
            // is a fact, so it carries no `!`.
            note: retention.map(|period| format!("policy: keep {period}")),
            heading: "Store".to_string(),
            rows: vec![
                row('c', NixVerb::Clean),
                row('D', NixVerb::DeleteGenerations),
            ],
        }],
    }
}

fn nix_search_transient() -> TransientDef {
    TransientDef {
        title: "Search".to_string(),
        switches: Vec::new(),
        groups: vec![DefGroup {
            heading: "Search".to_string(),
            note: None,
            rows: vec![
                row('p', NixVerb::SearchPackages),
                row('v', NixVerb::OptionValue),
            ],
        }],
    }
}

/// One row, live and unannotated. `App` marks it.
fn row(chord: char, verb: NixVerb) -> DefRow {
    DefRow {
        chord,
        label: verb.short_label(),
        note: None,
        dimmed: false,
        action: Action::Nix(verb),
    }
}
