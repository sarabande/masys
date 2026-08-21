//! The Nix buffer: what the configuration declares, what was realised,
//! and what is running.
//!
//! Ordered by what needs attention, like Status. A pending reboot leads
//! because it is the only section that appears solely because something is
//! wrong. Store follows and always renders: it carries the declared
//! maintenance policy rather than a complaint, which makes it this view's
//! equivalent of the Status buffer's System section. Generations and
//! inputs follow, and each is omitted entirely when it has nothing to
//! show - a profile directory that exists and is empty contributes no
//! section, because "no channels" and "zero channel generations" send the
//! operator looking for two different things. Nix's own units come last:
//! they are what is *running*, where everything above them is what was
//! declared and what was realised.

use masys_domain::declarative::{HomeMode, Inputs, Profile, ProfileKind, RebootState, RebuildVerb};
use masys_view::{Node, SectionKind};

use crate::keymap::{Action, NixVerb};
use crate::transient::{DefGroup, DefRow, TransientDef};
use std::collections::HashSet;

/// Every row the Nix buffer shows, in order.
///
/// A pure function of facts: everything it needs is an argument, including
/// the clock, so the whole layout is testable without a NixOS host - the
/// same shape as `build_io_rows`.
///
/// `policies` and `nix_units` arrive already built, by `App`, which is the
/// layer holding the units both are drawn from; deriving them here would
/// be this function reaching for a systemd poll and an open-detail map it
/// does not have. Both are taken by value rather than by slice because
/// `Node` is not `Clone` - it carries a boxed `UnitDetail` and a dozen
/// sample types, and each is only ever built to be handed straight over.
///
/// `collapsed` is keyed by `SectionKind::fold_key`, never by the title.
/// Two of this view's titles change under the operator: the Inputs
/// heading carries the drift verdict, and the Home-manager one carries
/// how it is deployed, so a rebuild in another terminal would rename a
/// folded section and quietly reopen it. `Store` is never looked up at
/// all: it always renders in full, the same reason `System` is never
/// checked against the Status buffer's own `collapsed`.
#[allow(clippy::too_many_arguments)]
pub fn build_nix_rows(
    profiles: Option<&[Profile]>,
    reboot: Option<&RebootState>,
    inputs: Option<&Inputs>,
    home_mode: HomeMode,
    running_nixpkgs_rev: Option<&str>,
    gc_roots: Option<u32>,
    now_ms: u64,
    store_used_percent: Option<f32>,
    store_free_bytes: Option<u64>,
    policies: Vec<Node>,
    nix_units: Vec<Node>,
    collapsed: &HashSet<String>,
) -> Vec<Node> {
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
    let count = |kind: ProfileKind| known.then(|| find(kind).map(|profile| profile.generations.len() as u32).unwrap_or(0));

    if let Some(state) = reboot {
        // Matched on `Some(path)` only. A generation whose link dangles
        // carries `None`, and must never be mistaken for the booted one
        // just because neither could be resolved.
        let id_of = |path: &str| {
            let generations = &find(ProfileKind::System)?.generations;
            generations.iter().find(|generation| generation.store_path.as_deref() == Some(path)).map(|g| g.id)
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
    // a complaint - this view's equivalent of the Status buffer's System
    // section. What varies is whether its rows are marked.
    rows.push(Node::SectionHeader { title: "Store".to_string(), kind: SectionKind::NixStore, count: None });
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
        rows.push(Node::SectionHeader { title: title.clone(), kind, count: None });
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
                    age_ms: generation.created_ms.map(|created| now_ms.saturating_sub(created)),
                    generation: generation.clone(),
                    kind: profile.kind,
                });
            }
        }
        rows.push(Node::Spacer);
    }

    if let Some(inputs) = inputs {
        let title = inputs_title(inputs, running_nixpkgs_rev);
        rows.push(Node::SectionHeader { title: title.clone(), kind: SectionKind::Inputs, count: None });
        if !collapsed.contains(&SectionKind::Inputs.fold_key(&title)) {
            let now_secs = now_ms / 1000;
            for input in &inputs.inputs {
                rows.push(Node::Input {
                    age_days: input.last_modified_secs.map(|then| (now_secs.saturating_sub(then) / 86_400) as u32),
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
        let count = nix_units.iter().filter(|row| matches!(row, Node::Unit { .. })).count() as u32;
        let title = "Nix units".to_string();
        rows.push(Node::SectionHeader { title: title.clone(), kind: SectionKind::NixUnits, count: Some(count) });
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
    let locked = inputs.inputs.iter().find(|input| input.name == "nixpkgs").and_then(|input| input.rev.as_deref());
    let short = |rev: &str| rev.chars().take(7).collect::<String>();
    match (locked, running) {
        (Some(locked), Some(running)) if locked == running => {
            format!("Inputs ({count})  lock {} . running {} . in sync", short(locked), short(running))
        }
        (Some(locked), Some(running)) => {
            format!("Inputs ({count})  lock {} . running {} . drift - not rebuilt", short(locked), short(running))
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
    ["nix-", "nixos-", "home-manager-"].iter().any(|prefix| name.starts_with(prefix))
}

/// The Nix transient, as the design's `[C2]` mockup draws it.
///
/// The *shape* only: every row is built live, and `App` marks the ones
/// that cannot act here from `nix_offer` - the same function the footer
/// asks, so a marked row and a dim key can never disagree. Building the
/// shape here and marking it there is what keeps this function free of
/// the eleven facts the marking needs.
///
/// The Generation group is contextual: it appears only when the cursor is
/// on a generation row, and names the generation it acts on. Every other
/// group is always listed, because a group that came and went would make
/// the popup unreadable by position - the same rule that keeps a dimmed
/// row in place rather than dropping it.
///
/// `gc now` and `optimise now` are not here, though the mockup draws
/// `o optimise`. The design's own sentence a page earlier is why: "Running
/// garbage collection is `s` on `nix-gc.service`; that operation needs no
/// new code at all." Both units have rows in this buffer and `s` starts
/// them. Routing them through the transient instead would need an action
/// that names its unit rather than taking the cursor's - one new variant
/// for two rows that already work.
pub fn nix_transient(generation: Option<u64>, retention: Option<&str>) -> TransientDef {
    let rebuild = [
        ('s', RebuildVerb::Switch),
        ('b', RebuildVerb::Boot),
        ('t', RebuildVerb::Test),
        ('B', RebuildVerb::Build),
        ('y', RebuildVerb::DryActivate),
    ]
    .into_iter()
    .map(|(chord, verb)| row(chord, NixVerb::Rebuild(verb)))
    .chain([row('R', NixVerb::Rollback)])
    .collect();

    let groups = [
        Some(DefGroup { heading: "Rebuild".to_string(), note: None, rows: rebuild }),
        Some(DefGroup { heading: "Home-manager".to_string(), note: None, rows: vec![row('h', NixVerb::HomeSwitch)] }),
        // Named for the generation it acts on, which is the reason this
        // group is contextual at all: `activate` and `delete` without a
        // number in the heading would be two of the most destructive rows
        // in masys with nothing on screen saying what they act on.
        generation.map(|id| DefGroup {
            heading: format!("Generation {id}"),
            note: None,
            rows: vec![row('a', NixVerb::Activate), row('d', NixVerb::Diff), row('x', NixVerb::DeleteHere)],
        }),
        Some(DefGroup {
            heading: "Inputs".to_string(),
            note: None,
            rows: vec![
                row('f', NixVerb::FlakeUpdate),
                row('F', NixVerb::FlakeCheck),
                row('n', NixVerb::ChannelUpdate),
                row('N', NixVerb::ChannelRollback),
            ],
        }),
        Some(DefGroup {
            // The declared policy, stated rather than warned about - it
            // is a fact, so it carries no `!`.
            note: retention.map(|period| format!("policy: keep {period}")),
            heading: "Store".to_string(),
            rows: vec![row('c', NixVerb::Clean), row('D', NixVerb::DeleteGenerations)],
        }),
        Some(DefGroup {
            heading: "Search".to_string(),
            note: None,
            rows: vec![row('p', NixVerb::SearchPackages), row('v', NixVerb::OptionValue)],
        }),
    ];

    TransientDef { title: "Nix ops".to_string(), switches: Vec::new(), groups: groups.into_iter().flatten().collect() }
}

/// One row, live and unannotated. `App` marks it.
fn row(chord: char, verb: NixVerb) -> DefRow {
    DefRow { chord, label: verb.short_label(), note: None, dimmed: false, action: Action::Nix(verb) }
}
