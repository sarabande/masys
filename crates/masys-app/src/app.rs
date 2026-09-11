//! The session: owns the ports and the two readings every buffer shares,
//! holds one struct per buffer, and produces a `View`.
//!
//! What is left here is what is genuinely session-wide. The ports,
//! because ports belong to the composition root and are handed to the
//! buffers that ask them things. `sample` and `units`, because triage,
//! the overview and three buffers all read them, and one owner is the
//! point. The cursor, the fold set, the filter and the modal stack,
//! because those are the same question in every buffer. Everything else
//! lives with the buffer it belongs to.
//!
//! `Facts` used to hold the rest, and its deletion is what this shape
//! was for: it was a flat bag beside a second flat bag of state, so
//! reuniting a buffer's facts with the behaviour over them meant a
//! positional argument list at every call site - eleven for the Nix
//! rows, eight for Procs, seven for IO.
//!
//! The split between `tick` and `rebuild` is the load-bearing one here.
//! `tick` talks to the ports and stores *facts*; `rebuild` turns facts it
//! already has into the open buffer's rows. Switching buffers, moving the
//! cursor and folding a group all go through `rebuild` alone, so none of
//! them samples the machine - pressing `j` must not cost a D-Bus round
//! trip.
//!
//! Transients (the contextual action popups) are still a later plan; `?`
//! is bound and protected but opens nothing.

use std::collections::{HashMap, HashSet};

use masys_domain::declarative::{DeclarativeService, NixOp, RebuildVerb};
use masys_domain::error::MasysError;
use masys_domain::finding::Thresholds;
use masys_domain::sample::Snapshot;
use masys_domain::scan::DirScanner;
use masys_domain::service::{PackageService, PlatformService, Signal, SystemService};
use masys_domain::unit::{ActiveState, Unit};
use masys_view::{Header, KeyBinding, KeyGroup, ModalView, Node, StatusLine, View};

use crate::buffer::Buffer;
use crate::io_buffer::IoBuffer;
use crate::jump::{Jump, RowTarget, row_named};
use crate::key::{Key, KeyCode};
use crate::keymap::{Action, Keymap, NixFamily, NixVerb, UnitVerb};
use crate::log_buffer::LogBuffer;
use crate::nix_buffer::{NixBuffer, NixOffer};
use crate::packages_buffer::PackagesBuffer;
use crate::procs::ProcsBuffer;
use crate::status::StatusBuffer;
use crate::systemd_buffer::{Persistence, SystemdBuffer, ownership_caveat, unit_transient};
use crate::transient::TransientDef;

/// A transient's open text-entry sub-step.
///
/// Holds the verb it will run rather than a closure or a queued `NixOp`,
/// because the operation cannot be built until the value exists - which
/// is the whole reason this state is here. `NixBuffer::op_typed` turns the
/// pair into an operation at the moment enter is pressed, so a tick that
/// lands mid-typing cannot move a row out from under an argument already
/// resolved.
#[derive(Debug, Clone)]
struct InputState {
    verb: NixVerb,
    prompt: &'static str,
    typed: String,
}

/// An action waiting on confirmation.
#[derive(Debug, Clone)]
enum Pending {
    Kill {
        pid: u32,
        signal: Signal,
    },
    Unit {
        unit: String,
        verb: UnitVerb,
    },
    /// A Nix operation that changes the machine. The arguments are
    /// already resolved: which generation, which profile, which
    /// retention. Resolving them when the key is pressed rather than when
    /// the confirmation is answered is what makes the prompt able to name
    /// them, and it is also what keeps a tick that lands in between from
    /// moving the rows under an answer already given.
    Nix {
        op: NixOp,
    },
}

/// Which popup has the screen, if any.
///
/// `Buffer` is the ordinary state: no popup, keys go to the keymap. The
/// rest are mutually exclusive by construction, which is the whole
/// point. They used to be five independent fields - four popups and a
/// prompt belonging to one of them - whose exclusivity was a sentence in
/// a doc comment, so every question about "which one is open" had to be
/// answered by asking them all in an agreed order.
///
/// Two things stay outside this enum, and both are deliberate.
/// `typing_filter` is not here because a filter is not a popup: it sits
/// at the top of the buffer it narrows, visible while the rows move
/// under it, and a centred modal hid the very rows it was filtering.
/// `suspended` is not here because it is a handoff rather than a mode -
/// it holds what an approved action is waiting to run, from the moment
/// it was approved until the caller has given the terminal up, and no
/// popup is open while it waits.
#[derive(Debug)]
enum Mode {
    Buffer,
    /// The `?` reference card. A variant rather than the flag it was, so
    /// "the help is open" is the same kind of statement as "a transient
    /// is open" instead of a second kind that has to be checked first.
    Help,
    /// A transient row is waiting on a typed value.
    ///
    /// Its own variant rather than something nested in `Transient`,
    /// because the two are independent: a transient closes when it
    /// dispatches the row that asked, and the input outlives it. Nesting
    /// would mean the popup that opened the prompt had to stay open
    /// behind it to hold the prompt's own state.
    Input(InputState),
    /// The open transient. Held rather than rebuilt each draw because a
    /// switch toggled in it has to survive to the next frame, and because
    /// the rows it was built from can move under it - see `TransientDef`.
    Transient(TransientDef),
    /// A destructive action waiting on a yes/no. Only signals reach here:
    /// nudging niceness is reversible and htop taught everyone it is free
    /// to try, so it acts immediately.
    ///
    /// The prompt travels with the action rather than in a field beside
    /// it. It was written on the line after `pending` at all three sites
    /// that asked for a confirmation, which is one thing in two places,
    /// and it is built when the confirmation opens because
    /// `ModalView::Confirm` borrows it and `view` takes `&self`.
    Confirm {
        pending: Pending,
        prompt: String,
    },
}

/// What [`Flow::Suspend`] was asked for.
///
/// A bare unit name until now, because `systemctl edit` was the only
/// thing in masys that needed the terminal. Every Nix operation needs it
/// too, and more so: `nixos-rebuild switch` streams for minutes and
/// `nix store diff-closures` pages, which is why
/// `masys_platform_nixos::ops` spawns them with stdio inherited rather
/// than capturing their output.
///
/// So the field carries *what to run* rather than one operation's
/// argument, and [`App::run_suspended`] stays the single place that turns
/// it into a port call. The alternative - a second `Option` field beside
/// the first - would let both be `Some` at once, and nothing in the type
/// would say which of the two the caller was about to run.
#[derive(Debug, Clone)]
enum Suspended {
    EditUnit(String),
    Nix(NixOp),
}

/// How much of a buffer is showing, for the buffer-wide cycle.
///
/// Two, because the outline has two levels of *section*: the headings and
/// the rows under them. A row's detail is not a third - it is opened one
/// row at a time with `enter`, and cycling it wholesale would open 366 of
/// them, each costing a read, to show what nobody asked to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Sections,
    Rows,
}

/// Whether the session continues after a keypress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
    /// The session needs the terminal back before it can go on.
    ///
    /// Returned when an action has to run a program that owns the screen:
    /// `systemctl edit`, which spawns `$EDITOR`, and every Nix operation,
    /// which streams its own output for as long as it takes. The caller
    /// releases the terminal, calls [`App::run_suspended`], and restores
    /// it. The session cannot do any of that itself: no layer above the
    /// composition root knows what a terminal is, which is the rule that
    /// keeps every crate here testable without one.
    ///
    /// What is waiting is `Suspended`, not this variant: a payload here
    /// would have to be public, and `Flow` is the one thing the binary
    /// matches on. What comes *back* is [`Aftermath`], because by then
    /// the command has run and there is a second fact to report.
    Suspend,
}

/// What the suspended command left on the screen the caller is about to
/// take back.
///
/// The caller cannot work this out for itself and must not guess: it hands
/// the terminal over blind, and only the session knows whether what ran
/// there was an editor the operator drove by hand or a command that
/// printed and exited. `nix store diff-closures` between generations 427
/// and 438 prints 84 lines and exits in 0.32 s on this host - measured
/// three times, and through masys itself - so a caller that re-entered the
/// alternate screen the moment the command returned took the diff with it.
/// The output survives only in the normal screen's scrollback, which is
/// invisible while masys is drawing.
///
/// A `bool` carries the same two states and is the obvious shape. It is
/// rejected for what it forces the *caller* to be named after:
/// the only honest name for a `bool` at that call site is `should_pause`,
/// and pausing is a terminal decision this crate is not allowed to hold an
/// opinion about - the rule [`Flow::Suspend`] states. The fact this crate
/// owns is what became of the output; what to do about it is the
/// composition root's, and a type named for the fact is what keeps the two
/// from being written as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aftermath {
    /// The operator has already read whatever is up there.
    ///
    /// An editor holds the terminal until it is quit, so by the time it
    /// returns its screen has been read and dismissed by the person who
    /// was reading it. Also the answer when nothing ran at all: there is
    /// no output, so none of it is unread.
    Seen,
    /// Output that ends the instant the command does, and that nobody has
    /// had a chance to read.
    Unseen,
}

/// Whether the operation keeps the terminal until the operator leaves it,
/// rather than printing and exiting.
///
/// Exhaustive for the reason `only_reads` is: the two answers differ in
/// what happens *after*, and a new operation that guessed wrong either
/// swallows its own output or charges a keypress for a screen the
/// operator already dismissed.
///
/// Only `Repl` does. Every other operation streams and exits - a rebuild
/// for minutes, a diff instantly - so masys pauses afterwards to let what
/// they printed be read. A repl ends when the operator ends it, at which
/// point they have already seen everything there was to see.
fn holds_the_terminal(op: &NixOp) -> bool {
    match op {
        NixOp::Repl => true,
        // All three build and print. `build-image` with no variant prints
        // a list and exits, which is a stream like any other.
        NixOp::BuildVm
        | NixOp::ListImageVariants
        | NixOp::BuildImage { .. }
        | NixOp::Rebuild(_)
        | NixOp::Activate { .. }
        | NixOp::Rollback
        | NixOp::Upgrade
        | NixOp::Diff { .. }
        | NixOp::Clean { .. }
        | NixOp::DeleteGenerations { .. }
        | NixOp::FlakeUpdate { .. }
        | NixOp::FlakeCheck
        | NixOp::ChannelUpdate
        | NixOp::ChannelRollback
        | NixOp::OptionValue { .. }
        | NixOp::SearchPackages { .. }
        | NixOp::HomeSwitch => false,
    }
}

/// Whether an operation reads and changes nothing.
///
/// The question the confirmation turns on, decided per operation against
/// what `masys_platform_nixos::ops::argv` runs for each, and matched
/// exhaustively rather than with a wildcard: a `NixOp` added later must
/// be looked at here, because the answer a wildcard would give it is the
/// one nobody would notice being wrong.
///
/// The four that only read do so plainly. `nix store diff-closures`
/// prints two closures' difference; `nixos-option` prints this host's
/// value for one option; `nix search nixpkgs` prints matches. `nix flake
/// check` is the one worth arguing about - it *builds* the flake's checks
/// and so adds store paths - but it activates nothing, changes no
/// profile, and leaves nothing an operator would have to undo, which is
/// the line that matters for a prompt.
///
/// This doc sat above `holds_the_terminal` between 2026-08-30 and the
/// review that found it: the new predicate was inserted between these
/// lines and the function they describe, and Rust silently reattached
/// them. Nothing warns about that, so the only guard is putting a
/// function under its own doc.
fn only_reads(op: &NixOp) -> bool {
    match op {
        NixOp::Diff { .. }
        | NixOp::FlakeCheck
        | NixOp::OptionValue { .. }
        // A repl evaluates. It can *print* anything about the
        // configuration and change none of it - there is no `:w` - so
        // there is nothing for a confirmation to be about.
        | NixOp::Repl
        // Asking which image variants exist builds none of them: with no
        // `--image-variant`, `nixos-rebuild build-image` prints the list
        // and stops. `BuildImage` itself is a build and is not here.
        | NixOp::ListImageVariants
        | NixOp::SearchPackages { .. } => true,
        // The rest all change something an operator would have to undo: a
        // running system and its boot default, the generations a rollback
        // needs, or the lock file a configuration is pinned by.
        //
        // `BuildVm` and `BuildImage` change no *system*, and are here for
        // the reason `Rebuild(Build)` is: they write to the store, leave
        // a `./result` in whatever directory masys was started from, and
        // take minutes - which is not a thing to start by accident.
        NixOp::BuildVm
        | NixOp::BuildImage { .. }
        | NixOp::Rebuild(_)
        | NixOp::Activate { .. }
        | NixOp::Rollback
        | NixOp::Upgrade
        | NixOp::Clean { .. }
        | NixOp::DeleteGenerations { .. }
        | NixOp::FlakeUpdate { .. }
        | NixOp::ChannelUpdate
        | NixOp::ChannelRollback
        | NixOp::HomeSwitch => false,
    }
}

/// What a confirmation must say the operation will do, or `None` for an
/// operation no key produces yet.
///
/// `ask_kill` names the signal because "SIGKILL cannot be caught, and the
/// difference is the whole reason there are two bindings". The same
/// standard here: activating a generation and deleting a fortnight of
/// them are different acts, and a prompt that said only "confirm?" would
/// make them look like one.
fn prompt_for(op: &NixOp) -> Option<String> {
    match op {
        // "and make it the boot default" because that is the second half
        // of what runs: `switch-to-configuration switch` activates *and*
        // installs the bootloader entry, where `test` would activate
        // without touching what boots. An operator who reads this as
        // "just for now" would be wrong in the way that matters.
        NixOp::Activate { generation, .. } => Some(format!("activate system generation {generation} and make it the boot default?")),
        // "in every profile", because that is what happens and it is not
        // what the row under the cursor suggests. nix-collect-garbage(1)
        // for nix 2.34.8 on this host: `--delete-older-than` "deletes all
        // generations of profiles older than the specified amount" across
        // every profile it finds, not the one the cursor is in - and then
        // collects. The generation active at that point in time survives,
        // which is why this does not say "every generation".
        NixOp::Clean { older_than } => Some(format!("delete generations older than {older_than} in every profile and collect the store?")),
        // The transient made the rest reachable, so the rest have
        // sentences now - written here, where there is something to
        // press, exactly as the note this replaces said they would be.
        //
        // Each says what it changes rather than what it is called. A
        // prompt reading "run switch?" tells an operator who already
        // pressed `s` nothing they did not know.
        // `y` at this prompt confirms the pending `Switch`, the same as
        // every other confirmation - it is not a way to reach
        // `DryActivate` from here, so the sentence names the row to run
        // instead of naming the key. Cancelling (`n`) and pressing `b`
        // then `y` is the actual path to it.
        NixOp::Rebuild(RebuildVerb::Switch) => Some(
            "build this configuration, activate it, and make it the boot default? cancelling and running dry-activate first shows which units it would restart."
                .to_string(),
        ),
        NixOp::Rebuild(RebuildVerb::Boot) => {
            Some("build this configuration and make it the boot default, without activating now?".to_string())
        }
        // "until you reboot" is the whole of the difference from
        // `switch`, and it is the reason `test` is the safe one to try.
        NixOp::Rebuild(RebuildVerb::Test) => Some("build this configuration and activate it until you reboot?".to_string()),
        // Confirmed although it activates nothing: a build writes to the
        // store, takes minutes, and is not free to start by accident.
        NixOp::Rebuild(RebuildVerb::Build) => Some("build this configuration without activating it?".to_string()),
        // Confirmed for `build`'s reason, and says where the result lands
        // because that is the half an operator cannot guess: the script
        // is left under `./result` in whatever directory masys was
        // started from, and running it is a second step they take
        // themselves.
        NixOp::BuildVm => Some(
            "build a qemu runner for this configuration, leaving it at ./result/bin/run-*-vm?"
                .to_string(),
        ),
        // Names the variant, because building the wrong one costs the
        // same minutes as building the right one.
        NixOp::BuildImage { variant } => {
            Some(format!("build the `{variant}` disk image for this configuration?"))
        }
        // Same fact `NixVerb::label` now names in the popup row - naming
        // the units is what makes this worth pressing before `s`, and the
        // two are the row's one description read from two places (see
        // `nix_buffer.rs`'s `note`).
        NixOp::Rebuild(RebuildVerb::DryActivate) => {
            Some("print which units activating this configuration would restart?".to_string())
        }
        NixOp::Rollback => Some("activate the previous generation and make it the boot default?".to_string()),
        // Says the channel update out loud, because it is the half an
        // operator does not get from the word "upgrade" and the half that
        // is not undone by a rollback: the generation can be rolled back,
        // the channels stay where `--upgrade` moved them. "the nixos
        // channel" rather than "the channels" because `--upgrade` is the
        // narrow flag - `ChannelUpdate` is the row that takes every one.
        NixOp::Upgrade => Some("update the nixos channel, then build and activate the result and make it the boot default?".to_string()),
        NixOp::HomeSwitch => Some("build and activate the home-manager configuration?".to_string()),
        // Names the spec rather than a count, because masys did not count
        // them: `+5` and `30d` are what `nix-env` was given, and how many
        // generations that is on this host is a question only `nix-env`
        // answers. A prompt saying "delete 9 generations" would be a
        // number nobody measured.
        NixOp::DeleteGenerations { profile, spec } => Some(format!("delete the generations matching `{spec}` from {profile}?")),
        // "and re-lock every input" because that is the file that
        // changes, and the one a rebuild afterwards will build from.
        NixOp::FlakeUpdate { input: None } => Some("update every flake input and re-lock them?".to_string()),
        NixOp::FlakeUpdate { input: Some(name) } => Some(format!("update the flake input `{name}` and re-lock it?")),
        NixOp::ChannelUpdate => Some("update every channel?".to_string()),
        NixOp::ChannelRollback => Some("roll every channel back to its previous state?".to_string()),
        // The ones that only read reach `run` without passing here.
        NixOp::ListImageVariants
        | NixOp::Diff { .. }
        | NixOp::FlakeCheck
        | NixOp::OptionValue { .. }
        | NixOp::Repl
        | NixOp::SearchPackages { .. } => None,
    }
}

/// What one buffer remembers between visits.
///
/// The two travel together because they are answers to the same
/// question - what you were looking at in this buffer - and because
/// separate maps let them disagree about which buffers there are.
#[derive(Debug, Default, Clone)]
struct BufferState {
    /// The row this buffer draws its cursor on: always legal against the
    /// rows as they currently are, because a renderer handed an
    /// out-of-range index panics.
    cursor: usize,
    /// The row the operator last went to, which is not the same thing.
    ///
    /// **The distinction is the whole point of having two.** A buffer can
    /// shrink between ticks - units stop, a read degrades, a sample comes
    /// back short - and the cursor must still land somewhere drawable,
    /// which is what `cursor` above is clamped to. Storing that clamp as
    /// the operator's position was the defect: a value computed to keep
    /// one frame legal replaced where they had actually gone, so a list
    /// that briefly came back shorter left the cursor near the top for
    /// good.
    ///
    /// Clamping from `wanted` every rebuild instead means a buffer that
    /// shrinks and grows back restores the row, and one that shrinks for
    /// good just keeps answering with its last legal row.
    wanted: usize,
    filter: String,
}

pub struct App {
    system: Box<dyn SystemService>,
    platform: Box<dyn PlatformService>,
    /// The declarative port, on the hosts that have one. `None` is the
    /// ordinary case, and the reason the Nix buffer is absent rather than
    /// empty everywhere else - see `crate::buffer::Registry`, which holds
    /// the other half of that fact and is owned by the keymap.
    declarative: Option<Box<dyn DeclarativeService>>,
    /// Summing a directory tree, behind a thread. The one source too slow
    /// for the tick's duty cycle - see `masys-scan`. A port, so it is held
    /// here with the others and handed to the buffer that asks it things.
    scanner: Box<dyn DirScanner>,
    /// The IO buffer: the directory walk it tracks, and the throughput it
    /// derives.
    io: IoBuffer,
    keymap: Keymap,
    hostname: String,
    timestamp: String,
    buffer: Buffer,
    rows: Vec<Node>,
    /// What each buffer remembers while you are somewhere else: where its
    /// cursor was, and what it was narrowed to.
    ///
    /// Keyed on the `Buffer` itself. It used to be keyed on
    /// `Buffer::title()` - the string the header prints - which made a
    /// display name into an identity, and identities are the one thing
    /// that must not change when someone rewords a heading. `title()`
    /// resolves by scanning `BUFFERS` and panics on a miss, a panic added
    /// after the fallback it replaced collapsed every unregistered buffer
    /// into one shared cursor slot. That turned a silent collision into a
    /// loud one without moving identity off the string.
    ///
    /// One map rather than two. The cursor and the filter were separate
    /// `HashMap`s said to be "keyed like" each other, which is a
    /// convention two maps cannot be held to: they could disagree about
    /// which buffers exist and nothing would notice.
    per_buffer: HashMap<Buffer, BufferState>,
    /// Collapsed group paths in the Procs buffer. Storing the *collapsed*
    /// set rather than the expanded one is what makes a cgroup that
    /// appears between two ticks show up expanded, which is the useful
    /// default and costs no extra bookkeeping.
    collapsed: HashSet<String>,
    /// Where the buffer-wide cycle has got to. Only `cycle_all` reads or
    /// writes it; the per-section cycle derives its position instead.
    level: Level,
    /// The systemd buffer: which units are open, and which slice subtrees.
    systemd: SystemdBuffer,
    /// Whether a filter is being typed right now. Filtering live as you
    /// type is htop's behaviour and lnav's, and it is what makes the key
    /// worth pressing on a 300-row buffer.
    ///
    /// The text itself lives on `per_buffer`, one filter per buffer. A
    /// single shared string meant narrowing the systemd buffer to a unit
    /// and pressing `l` carried that unit's name into the log buffer -
    /// where it silently dropped every line systemd itself had logged
    /// about the unit, and where typing a search appended to it and
    /// matched nothing.
    typing_filter: bool,
    /// How many rows the open filter actually matched, or `None` when
    /// nothing is being filtered on.
    ///
    /// Held rather than recomputed in `view()`, which takes `&self` and
    /// would have to re-run the whole match to answer - and could then
    /// disagree with the rows already on screen.
    filter_matches: Option<usize>,
    /// What `filter` held when this typing session opened, so escape can
    /// put it back. Held rather than recomputed because the live filter
    /// is overwritten keystroke by keystroke and there is nowhere else
    /// the old text survives.
    filter_before: String,
    /// Which popup is open, and what it is holding.
    ///
    /// One field for what used to be five - `help_open`, `transient`,
    /// `input`, `pending` and the `confirm_prompt` that belonged to it.
    /// Their exclusivity was real and written down (`transient` carried
    /// "never `Some` at the same time as `pending`") but nothing held it,
    /// so two chains resolved the five by hand: one routing keystrokes
    /// and one choosing what to draw. They agreed by inspection. A `Mode`
    /// makes the second popup unrepresentable rather than merely
    /// unreachable, which is what lets both chains become one match.
    mode: Mode,
    /// The last action's failure, held until something replaces it - the
    /// design's rule that a refresh must not silently erase an error.
    error: Option<String>,
    /// Why the last sample failed, if it did.
    ///
    /// A slot of its own rather than sharing `error`, whose whole
    /// contract is to *hold* what an action said until the operator does
    /// something else. A tick that failed two seconds after a refused
    /// `systemctl disable` would otherwise overwrite the answer they
    /// were waiting for with ambient noise. This one is set and cleared
    /// every tick, so it disappears when the sample comes back; `view`
    /// shows the held error first and this only when there is none.
    sample_error: Option<String>,
    /// The log buffer: whose journal is open, what it said, and which
    /// way it reads.
    log: LogBuffer,
    /// What an approved action is waiting to run, held from the moment it
    /// was approved until the caller has given the terminal up. `None` at
    /// every other moment.
    suspended: Option<Suspended>,
    /// Where `l` was pressed, so `esc` can go back to it.
    ///
    /// The buffer *and* the unit, not just the buffer: the systemd buffer
    /// remembers its own cursor already, but rows are rebuilt every tick
    /// and a unit can move between them, so back means "to that unit"
    /// rather than "to that row number".
    ///
    /// `q` cannot do this job - it quits from everywhere, which is what
    /// makes it predictable - so the drill-down needs a key of its own.
    came_from: Option<(Buffer, String)>,
    /// The Procs buffer: which rows are open, how they are ordered, and
    /// the rates that ordering is against.
    procs: ProcsBuffer,
    /// The footer's offer for the open buffer. Held rather than built in
    /// `view()` because `View` borrows it, and rebuilt whenever the
    /// buffer changes - which is the only thing that changes it.
    hints: Vec<KeyBinding>,
    actions: Vec<KeyBinding>,
    /// The Status buffer: one triage pass, and what frames it.
    status: StatusBuffer,
    /// The Nix buffer's own readings and the operations over them.
    ///
    /// Its nine readings used to sit in `Facts` and its fifteen methods
    /// here, which is what made its row builder an eleven-parameter call:
    /// state on one side of a seam and the behaviour over it on the other. Nothing outside `crate::nix_buffer` writes these - the
    /// keep-last-good rule they all follow is `NixBuffer::refresh`'s, and
    /// it is the only writer in the program.
    nix: NixBuffer,
    packages: PackagesBuffer,
    /// The port behind the Packages buffer, absent on a host that cannot
    /// list any - which is what makes the buffer absent too.
    package_service: Option<Box<dyn PackageService>>,
    /// The last time `tick` was given, so a journal fetch triggered by a
    /// keypress has a lookback to use. A keypress has no clock of its
    /// own, and inventing one here is exactly what `tick` taking `now_ms`
    /// exists to avoid.
    last_now_ms: u64,
    /// What `SystemService::units` last answered.
    ///
    /// Beside the sample rather than inside the systemd buffer, and for
    /// the sample's reason: three buffers and the triage rules all read
    /// it, so one owner is the point. Triage evaluates against it, the
    /// overview counts it, the systemd buffer is a picture of it and the
    /// Nix buffer draws its own units out of it.
    units: Vec<Unit>,
    /// The last sample, whole.
    ///
    /// One copy rather than two. `Facts` used to clone `procs`,
    /// `filesystems`, `disks` and `interfaces` out of every snapshot -
    /// plus two scalars off it - while this field already held the same
    /// snapshot entire. The machine's process table was in the tree
    /// twice, and a row builder read whichever copy its caller reached
    /// for, which is the arrangement `crate::buffer::Registry`'s doc
    /// warns about: two owners of one question, disagreeing where it
    /// matters.
    ///
    /// `None` before the first tick - the state only a test can observe,
    /// since the binary ticks once before its first draw.
    ///
    /// Rates are a derivative and need two samples, so `tick` derives
    /// them against what this field still holds, the previous tick's,
    /// before replacing it. CPU% therefore does not exist before the
    /// second tick - see the design's realtime model.
    sample: Option<Snapshot>,
}

impl App {
    /// Uses the built-in default thresholds and keymap - real usage loads
    /// the user's config file instead, once masys-app owns config loading.
    pub fn new(
        system: Box<dyn SystemService>,
        platform: Box<dyn PlatformService>,
        scanner: Box<dyn DirScanner>,
        hostname: String,
    ) -> Self {
        Self::with_keymap(system, platform, scanner, hostname, Keymap::default())
    }

    pub fn with_keymap(
        system: Box<dyn SystemService>,
        platform: Box<dyn PlatformService>,
        scanner: Box<dyn DirScanner>,
        hostname: String,
        keymap: Keymap,
    ) -> Self {
        App {
            system,
            platform,
            declarative: None,
            scanner,
            io: IoBuffer::default(),
            keymap,
            hostname,
            timestamp: String::new(),
            buffer: Buffer::Status,
            rows: Vec::new(),
            per_buffer: HashMap::new(),
            // The mockup draws the kernel bucket folded, and it is by far
            // the largest group on a normal host - 145 of 353 processes here.
            collapsed: HashSet::from([crate::procs::KERNEL_GROUP.to_string()]),
            level: Level::Rows,
            systemd: SystemdBuffer::default(),
            typing_filter: false,
            filter_matches: None,
            filter_before: String::new(),
            mode: Mode::Buffer,
            error: None,
            sample_error: None,
            log: LogBuffer::default(),
            procs: ProcsBuffer::default(),
            came_from: None,
            suspended: None,
            hints: Vec::new(),
            actions: Vec::new(),
            last_now_ms: 0,
            status: StatusBuffer::default(),
            nix: NixBuffer::default(),
            packages: PackagesBuffer::default(),
            package_service: None,
            units: Vec::new(),
            sample: None,
        }
    }

    /// The composition root's entry point once a declarative adapter may
    /// exist. `App::new` keeps its signature and passes `None`, so every
    /// existing call site is unaffected.
    ///
    /// The caller pairs the service with a keymap built against a matching
    /// registry - `Keymap::for_registry(Registry::new(gates))` where a
    /// service exists - and this is the only moment both are in one hand,
    /// so it is the only place the pairing can be checked. A mismatch is
    /// silent otherwise, and silent in both directions: a service with a
    /// default keymap builds a buffer no key reaches, and a Nix-bearing
    /// keymap with no service gives `5` a buffer with nothing on it, which
    /// is the exact failure `Registry` was added to prevent.
    ///
    /// `debug_assert` rather than `assert`: the suite and every debug
    /// build catch it, while a release binary degrades to one wrong buffer
    /// rather than refusing to start. A tool for looking at a sick machine
    /// that will not launch is worse than one missing a buffer.
    pub fn with_declarative(
        system: Box<dyn SystemService>,
        platform: Box<dyn PlatformService>,
        scanner: Box<dyn DirScanner>,
        hostname: String,
        keymap: Keymap,
        declarative: Option<Box<dyn DeclarativeService>>,
    ) -> Self {
        let mut app = Self::with_keymap(system, platform, scanner, hostname, keymap);
        debug_assert_eq!(
            app.keymap.registry().contains(Buffer::Nix),
            declarative.is_some(),
            "the keymap's registry and the declarative service disagree about whether this host has a Nix buffer"
        );
        app.declarative = declarative;
        app
    }

    /// Applies the triage thresholds this session evaluates against.
    ///
    /// Set after construction rather than passed to one. `with_declarative`
    /// already takes six arguments, and this crate has just spent a change
    /// removing positional lists of that shape rather than lengthening
    /// them - see `crate::status::StatusBuffer`, which is where these end
    /// up. Nothing about building a session needs them either:
    /// `Thresholds::default()` is a working set, so a host with no config
    /// file and every test that does not care can leave it alone.
    ///
    /// Takes them whole rather than one at a time, because a config file
    /// is read whole: a partial application would leave the session
    /// evaluating against a mixture no file describes.
    pub fn set_thresholds(&mut self, thresholds: Thresholds) {
        self.status.thresholds = thresholds;
    }

    /// Hands over the package port, if this host has one.
    ///
    /// A setter rather than a seventh constructor parameter, following
    /// `set_thresholds`. The assertion is `with_declarative`'s, for the
    /// same reason it is there: the registry decides whether `6` exists
    /// and the port decides whether anything can answer it, and a host
    /// where those two disagree offers a key that opens an empty buffer
    /// or hides one that would have worked.
    pub fn set_package_service(&mut self, packages: Option<Box<dyn PackageService>>) {
        debug_assert_eq!(
            self.keymap.registry().contains(Buffer::Packages),
            packages.is_some(),
            "the keymap's registry and the package service disagree about whether this host has a Packages buffer"
        );
        self.package_service = packages;
    }

    /// Which buffer is active. Nothing in the binary calls this - it reads
    /// state through `view()` instead. Tests use it to assert which buffer
    /// a key sequence landed on, since the field itself is private.
    pub fn buffer(&self) -> Buffer {
        self.buffer
    }

    /// Whether the caller should skip its timed tick.
    ///
    /// An open process row is being *read*, and its block is the one part
    /// of masys that both moves under the eye - a command line, a
    /// directory, an fd table - and costs the most to re-read: chrome
    /// holds 928 descriptors, each a `readlink`, and the socket tables on
    /// top of that. Freezing while it is open is what makes it readable
    /// and what stops the tick paying for a table nobody is watching
    /// change.
    ///
    /// Advisory, not enforced inside `tick`: `g` must still refresh on
    /// demand, and the caller owns the clock. Folding the row resumes it,
    /// with no state to remember either way - the pause *is* the open
    /// row.
    pub fn auto_refresh_paused(&self) -> bool {
        self.procs.paused()
    }

    /// Swaps the system port. Exists for tests that need a second sample
    /// with different contents; nothing in the binary calls it.
    pub fn replace_system(&mut self, system: Box<dyn SystemService>) {
        self.system = system;
    }

    /// Samples the machine, runs the v1 triage rules, and rebuilds the
    /// open buffer. `now_ms` and `timestamp` come from the caller rather
    /// than being read here, so `tick` stays as testable as
    /// `masys_domain::triage::evaluate` already is - no hidden clock read
    /// to fake around.
    pub fn tick(&mut self, now_ms: u64, timestamp: String) {
        // The sample is the one read that still ends the tick. Without a
        // `Snapshot` there is no reading to degrade - every buffer's
        // refresh takes one - so this tick genuinely did not happen.
        //
        // Reported here rather than returned, and that is why nothing is
        // returned at all: every one of the binary's four call sites was
        // `let _ = tick(app)`, so the failure that ends a pass was the
        // one thing on this screen nobody could see. Four callers that
        // must each remember to route an error is four chances to write
        // that again; reporting it here makes discarding it impossible
        // rather than merely discouraged.
        let snapshot = match self.system.sample() {
            Ok(snapshot) => {
                // Cleared on recovery, or the echo line would go on
                // asserting a failure the latest reading contradicts -
                // the one rule in reverse.
                self.sample_error = None;
                snapshot
            }
            Err(why) => {
                self.sample_error = Some(why.to_string());
                return;
            }
        };
        // The unit list degrades instead. An empty one renders as a host
        // with zero units and nothing failed, which is a confident
        // answer drawn from a read that did not work, so the last good
        // list stands and the Status buffer reports that it is stale.
        let units_unreadable = match self.system.units() {
            Ok(units) => {
                self.units = units;
                None
            }
            // The error itself, not its text: `unreadable_finding`
            // stringifies once, where the other two sources already hand
            // it a `Result` and do the same. A `String` here made the
            // message cross the seam allocated, borrowed and allocated
            // again.
            Err(why) => Some(why),
        };
        let units = std::mem::take(&mut self.units);
        // Throughput against the sample this field still holds - the
        // previous tick's - and whatever the walk has counted since.
        //
        // Before the status buffer rather than after, which is where it
        // used to sit: the Status header shows what this machine is
        // moving, and it takes the totals from here rather than summing
        // its own. One derivation, two places.
        self.io
            .refresh(self.scanner.as_ref(), self.sample.as_ref(), &snapshot);
        self.status.refresh(
            self.system.as_ref(),
            self.platform.as_ref(),
            crate::status::Tick {
                previous: self.sample.as_ref(),
                snapshot: &snapshot,
                units: &units,
                units_unreadable: units_unreadable.as_ref(),
                net_throughput: self.io.net_total,
                disk_throughput: self.io.disk_total,
                now_ms,
            },
        );

        self.last_now_ms = now_ms;
        self.units = units;
        // Sampled on the ordinary tick, at whatever cadence the caller
        // runs one - two seconds in the binary.
        //
        // The design's table puts a platform read on 60s, and an earlier
        // draft of this block carried a comment saying so while reading
        // every tick. Only one of the two could be true, and it is this
        // one, because the alternative costs more than it saves: `g`,
        // "refresh now", reaches the session through this same `tick`, so
        // a 60-second gate here would quietly make the one key that
        // promises a fresh sample not deliver one. Telling a timed call
        // from a keyed one means a `tick` signature the composition root
        // has to thread a flag through, for a read this cheap.
        // `masys::TICK` records the same decision for the same reason:
        // one interval for everything until a buffer needs otherwise.
        //
        // What it costs, per tick: `canonicalize` on a handful of links, a
        // `read_dir` of two profile directories and the unit directory, a
        // `flake.lock` parse, and two small file reads. Closure sizes -
        // the one genuinely expensive read - are deferred to `enter` on a
        // row, not read on every tick.
        if let Some(declarative) = self.declarative.as_ref() {
            self.nix.refresh(declarative.as_ref());
        }
        self.procs.refresh(self.sample.as_ref(), &snapshot);
        if self.buffer == Buffer::Log {
            self.log.refresh(self.system.as_ref());
        }
        // Only while the buffer is open, which is the rule the log buffer
        // above follows and for a sharper reason: this walks 1408 symlinks
        // on the development host and canonicalises every one, where the
        // rest of a tick is a handful of small reads. Nothing else in
        // masys consumes the package list, so reading it on a tick that
        // is showing Procs would be the most expensive thing masys does
        // and invisible.
        if self.buffer == Buffer::Packages
            && let Some(packages) = self.package_service.as_ref()
        {
            self.packages.refresh(packages.as_ref());
        }
        self.timestamp = timestamp;
        self.sample = Some(snapshot);
        // Open rows are re-read *after* the sample is stored, because
        // whether a row should still be open is asked of the sample: a
        // `proc_detail` that fails means "gone" or "another user's",
        // and only the sample tells the two apart. Asking the previous
        // tick's would keep a dead pid's row a tick past its process.
        //
        // Each buffer re-reads its own, and only while it is the one
        // being looked at - a detail block that stopped being re-read
        // would sit there showing the memory figure it had when it was
        // opened. The Nix buffer draws units too, which is why it asks
        // the systemd buffer.
        match self.buffer {
            Buffer::Systemd | Buffer::Nix => self.systemd.refresh_open(self.system.as_ref()),
            Buffer::Procs => self
                .procs
                .refresh_open(self.system.as_ref(), self.sample.as_ref()),
            _ => {}
        }
        self.rebuild();
    }

    /// Turns the facts already held into the open buffer's rows, then puts
    /// the cursor somewhere legal. Never samples.
    fn rebuild(&mut self) {
        // A search reaches every row, folded or not. The filter runs over
        // the rows, and the rows are built after folding - so a collapsed
        // section used to contribute nothing to search, and `/` could not
        // find a unit you had just folded out of sight. Searching is how
        // you find a unit you cannot see, and a fold is exactly the state
        // in which you cannot see it.
        //
        // The fold state itself is untouched, so clearing the filter puts
        // the sections back rather than leaving a search having silently
        // unfolded the buffer.
        let folded = HashSet::new();
        let collapsed = if self.filter().is_empty() {
            &self.collapsed
        } else {
            &folded
        };
        // Read from the one sample rather than from copies of it, and
        // empty where there is no sample yet - the state only a test can
        // observe, since the binary ticks once before its first draw. The
        // empties are what `Facts::default()` used to supply here, so a
        // buffer drawn before the first tick renders exactly as it did.
        let sample = self.sample.as_ref();
        let procs = sample.map_or(&[][..], |sample| &sample.procs);
        let filesystems = sample.map_or(&[][..], |sample| &sample.filesystems);
        let disks = sample.map_or(&[][..], |sample| &sample.disks);
        let interfaces = sample.map_or(&[][..], |sample| &sample.interfaces);
        let clock_ticks = sample.map_or(0, |sample| sample.clock_ticks_per_sec);
        let utc_offset_secs = sample.map_or(0, |sample| sample.utc_offset_secs);
        self.rows = match self.buffer {
            Buffer::Status => self.status.rows(),
            Buffer::Procs => self
                .procs
                .rows(procs, clock_ticks, collapsed, self.last_now_ms),
            Buffer::Systemd => self.systemd.rows(&self.units, collapsed, self.last_now_ms),
            Buffer::Io => self.io.rows(filesystems, disks, interfaces),
            Buffer::Log => self.log.rows(utc_offset_secs, collapsed),
            Buffer::Packages => self.packages.rows(),
            Buffer::Nix => self.nix.rows(
                filesystems,
                &self.units,
                &self.systemd,
                collapsed,
                self.last_now_ms,
            ),
        };
        self.filter_matches = if self.filter().is_empty() {
            // No query, no count. A blank filter line that reported the
            // whole buffer would be answering a question nobody asked.
            None
        } else {
            let needle = self.filter().to_string();
            let (rows, matched) = crate::filter::apply(std::mem::take(&mut self.rows), &needle);
            self.rows = rows;
            Some(matched)
        };
        self.hints = self.keymap.hints(self.buffer);
        // From `wanted`, not from `cursor`: clamping the clamp is what
        // loses the row. An empty buffer needs no special case here
        // either - it clamps to 0 like anything else, `View::selected`
        // reports `None` while there are no rows, and `wanted` still
        // holds the row to come back to.
        let wanted = self.per_buffer.entry(self.buffer).or_default().wanted;
        let clamped = self.legal_cursor(wanted);
        self.per_buffer.entry(self.buffer).or_default().cursor = clamped;
        // After the clamp, because what is dimmed depends on the row the
        // cursor ends up on rather than the one it started from.
        self.refresh_actions();
    }

    fn filter(&self) -> &str {
        self.per_buffer
            .get(&self.buffer)
            .map(|state| state.filter.as_str())
            .unwrap_or("")
    }

    fn filter_mut(&mut self) -> &mut String {
        &mut self.per_buffer.entry(self.buffer).or_default().filter
    }

    fn cursor(&self) -> usize {
        self.per_buffer
            .get(&self.buffer)
            .map(|state| state.cursor)
            .unwrap_or(0)
    }

    /// Moves the cursor, and records that this is where the operator
    /// meant to be.
    ///
    /// Both, always. Movement reads `cursor()` - the drawn row - so a
    /// move made from a clamped position re-anchors `wanted` to where
    /// they actually are, rather than stepping from a remembered index
    /// they can no longer see.
    fn set_cursor(&mut self, index: usize) {
        let state = self.per_buffer.entry(self.buffer).or_default();
        state.cursor = index;
        state.wanted = index;
    }

    /// Lifts a buffer's filter and leaves the rest of what it remembers
    /// alone.
    ///
    /// Clears the text rather than dropping the entry, which would take
    /// the cursor with it now that the two share one. Only the filter is
    /// being asked for at each of the three call sites - a jump reveals
    /// what a filter was hiding, it does not send you back to the top of
    /// a buffer you had scrolled.
    fn clear_filter(&mut self, buffer: Buffer) {
        if let Some(state) = self.per_buffer.get_mut(&buffer) {
            state.filter.clear();
        }
    }

    /// Whether the cursor may rest on `index`.
    ///
    /// The row answers for itself - [`Node::selectable`] holds the rule
    /// and holds it exhaustively. What is left here is the only part that
    /// is about the index rather than the row: an index past the end is
    /// not selectable, and rows are rebuilt every tick, so that case is
    /// reached rather than theoretical.
    fn selectable(&self, index: usize) -> bool {
        self.rows.get(index).is_some_and(Node::selectable)
    }

    /// The nearest selectable row at or after `wanted`, falling back to
    /// searching backwards. Rows are rebuilt every tick and a buffer can
    /// shrink between them, so a cursor that was legal a moment ago may
    /// now be past the end - and a renderer handed an out-of-range index
    /// would panic.
    fn legal_cursor(&self, wanted: usize) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let start = wanted.min(self.rows.len() - 1);
        (start..self.rows.len())
            .find(|i| self.selectable(*i))
            .or_else(|| (0..=start).rev().find(|i| self.selectable(*i)))
            .unwrap_or(0)
    }

    /// Moves to the next or previous section heading.
    ///
    /// A buffer's sections are what its rows are *about* - failed units,
    /// timers, one cgroup - so jumping between them is how you skim a
    /// screen of two hundred rows. magit binds `n`/`p` for this and wagit
    /// followed; masys does too.
    ///
    /// A section here is any heading row: a `SectionHeader`, or a
    /// `ProcGroup`, which is the Procs buffer's heading in everything but
    /// name. Running off either end stays put rather than wrapping, the
    /// same rule ordinary movement follows.
    fn jump_section(&mut self, delta: isize) {
        let mut index = self.cursor() as isize;
        loop {
            index += delta;
            if index < 0 || index as usize >= self.rows.len() {
                return;
            }
            if self.rows.get(index as usize).is_some_and(Node::heading) {
                self.set_cursor(index as usize);
                return;
            }
        }
    }

    /// Steps the cursor by one selectable row in `delta`'s direction,
    /// stopping at either end rather than wrapping. Wrapping would make
    /// holding `j` silently loop a buffer that does not fit the screen.
    fn step(&mut self, delta: isize) {
        let mut index = self.cursor() as isize;
        loop {
            let next = index + delta;
            if next < 0 || next as usize >= self.rows.len() {
                return;
            }
            index = next;
            if self.selectable(index as usize) {
                self.set_cursor(index as usize);
                return;
            }
        }
    }

    pub fn handle_key(&mut self, key: Key) -> Flow {
        match self.mode {
            // The help is a reference card, not a mode with its own
            // verbs: any key dismisses it. Making the reader learn a
            // second set of keys to leave the list of keys would be its
            // own small joke.
            Mode::Help => {
                self.mode = Mode::Buffer;
                Flow::Continue
            }
            // The input sub-step is innermost: it is open only while a
            // transient row is waiting on a value, and every printable
            // key belongs to it.
            Mode::Input(_) => self.answer_input(key),
            // A transient answers its own keys, ahead of the keymap and
            // ahead of the escape ladder: a popup taking keystrokes must
            // not let one through to the buffer underneath.
            Mode::Transient(_) => self.answer_transient(key),
            // Including `esc`, which is why the escape ladder below is in
            // the arm it is in rather than ahead of this one. Backing a
            // filter out from under a question still on screen would
            // leave a `y / n` over rows that had just changed.
            Mode::Confirm { .. } => self.answer_confirm(key),
            Mode::Buffer => {
                // Inside this arm rather than ahead of the match, which
                // it used to be: a filter is typed *in* a buffer. Nothing
                // sets `typing_filter` except `dispatch`, which only runs
                // here, and `type_filter` opens no popup - so a filter
                // being typed and a popup being open cannot coincide, and
                // the position it lost in the old chain was one it could
                // never have used.
                if self.typing_filter {
                    return self.type_filter(key);
                }
                // Escape peels one layer at a time, outermost first: the
                // filter being typed (handled just above), then a filter
                // in effect, then a drill-down. Committing a filter with
                // Enter used to leave no way out of it but `/`, backspace
                // held down, Enter - and a filter you cannot lift is one
                // that quietly hides rows for the rest of the session.
                //
                // Checked before the keymap so it needs no binding and
                // cannot be rebound away from: there has to be one key
                // that always means "back out of this".
                if key.code == KeyCode::Esc && self.escape() {
                    return Flow::Continue;
                }
                self.dispatch(self.keymap.resolve(self.buffer, key))
            }
        }
    }

    /// Performs one resolved action.
    ///
    /// Split out of [`App::handle_key`] because a transient row dispatches
    /// the same `Action` a keypress does - that is what the definition
    /// carrying its own `Action` buys - and two copies of this match would
    /// be two answers to "what does this verb do".
    ///
    /// Takes an `Option` because `Keymap::resolve` returns one and an
    /// unbound key is not an error: it does nothing, and doing nothing
    /// still refreshes the footer.
    fn dispatch(&mut self, action: Option<Action>) -> Flow {
        match action {
            Some(Action::MoveDown) => self.step(1),
            Some(Action::MoveUp) => self.step(-1),
            // A page is a fixed jump rather than the rendered height: the
            // renderer measures the height and reports it as `Metrics`,
            // but nothing observes that yet.
            Some(Action::PageDown) => (0..PAGE).for_each(|_| self.step(1)),
            Some(Action::PageUp) => (0..PAGE).for_each(|_| self.step(-1)),
            Some(Action::NextSection) => self.jump_section(1),
            Some(Action::PrevSection) => self.jump_section(-1),
            Some(Action::MoveTop) => {
                let top = self.legal_cursor(0);
                self.set_cursor(top);
            }
            Some(Action::MoveBottom) => {
                let bottom = self.legal_cursor(self.rows.len().saturating_sub(1));
                self.set_cursor(bottom);
            }
            Some(Action::Cycle) => self.cycle_section(),
            Some(Action::CycleAll) => self.cycle_all(),
            Some(Action::ToggleDetail) => self.toggle_detail(),
            // From every buffer, not just Status: a digit reaches any buffer
            // directly, so there is nothing left for a bury to do.
            Some(Action::Quit) => return Flow::Quit,
            Some(Action::Open(buffer)) => self.open(buffer),
            Some(Action::Help) => self.mode = Mode::Help,
            Some(Action::Filter) => {
                self.filter_before = self.filter().to_string();
                self.typing_filter = true;
            }
            Some(Action::Kill(signal)) => self.ask_kill(signal),
            Some(Action::Nice(delta)) => self.nudge_nice(delta),
            Some(Action::Unit(verb)) => self.ask_unit(verb),
            // Returns rather than falling through to the footer refresh,
            // for the reason `Action::Quit` does: this is the only
            // buffer-local action that can ask for the terminal, and a
            // `Flow` the match swallowed would leave the caller holding
            // it. Nothing is lost - `run_suspended` rebuilds once the
            // command is done, and the next keypress refreshes the offer.
            Some(Action::Nix(verb)) => {
                if self.nix(verb) == Flow::Suspend {
                    return Flow::Suspend;
                }
            }
            Some(Action::NixMenu(family)) => self.open_nix_family(family),
            Some(Action::Transient) => self.open_row_transient(),
            Some(Action::Logs) => self.show_logs(),
            Some(Action::ToggleOrder) => self.toggle_log_order(),
            Some(Action::GoToUnit) => self.go_to_unit(),
            Some(Action::JumpToFinding) => self.jump_to_finding(),
            Some(Action::SortBy(sort)) => {
                // The same key again reverses; a different one switches
                // column and takes that column's natural direction, so
                // `n` always starts at a-z rather than inheriting
                // whichever way the last column happened to be pointing.
                self.procs.sort_by(sort);
                self.rebuild();
            }
            // Refresh is the caller's to perform: it owns the clock, the
            // same reason `tick` takes `now_ms` rather than reading one.
            Some(Action::Refresh) | None => {}
        }
        self.refresh_actions();
        Flow::Continue
    }

    /// Shows or hides the section under the cursor - magit's `TAB`.
    ///
    /// Two states, not three, because this buffer has one level of section:
    /// a type and the units in it. A row's detail is not a deeper level of
    /// the outline, it is the row's own content, and `enter` owns it.
    ///
    /// A row that is not a section falls through to opening itself, which
    /// is what `TAB` does at a leaf in magit too.
    fn cycle_section(&mut self) {
        // A slice owns a subtree, so `tab` folds it - the same act as
        // folding a section, one level further in. Every other row has no
        // subtree, and `tab` there opens the row itself.
        let subtree = match self.rows.get(self.cursor()) {
            Some(Node::Unit { unit, .. })
                if unit.kind == masys_domain::unit::UnitKind::Slice
                    || !unit.triggers.is_empty() =>
            {
                Some(unit.name.clone())
            }
            // A timer's row is its schedule, but it opens like any other.
            Some(Node::Timer { name, .. }) => Some(name.clone()),
            _ => None,
        };
        if let Some(name) = subtree {
            self.systemd.toggle_slice(name);
            return self.rebuild();
        }
        let name = match self.rows.get(self.cursor()) {
            Some(Node::ProcGroup { name, .. }) => name.clone(),
            // Units in the systemd buffer, days in the log, generations and
            // inputs in the Nix buffer: all are sections whose rows fold
            // away under their heading. Which kinds those are, and what
            // each folds under, both live on `SectionKind` - the two used
            // to be spelled out here and again in `group_names`, kept in
            // step by a comment.
            Some(Node::SectionHeader { title, kind, .. }) if kind.folds() => kind.fold_key(title),
            _ => return self.toggle_detail(),
        };
        // Shutting a section shuts what was open inside it: those rows are
        // about to stop existing, and a detail left open would spring back
        // the next time the section did.
        if self.collapsed.remove(&name) {
        } else {
            self.systemd.shut(self.section_members(self.cursor()));
            self.collapsed.insert(name);
        }
        self.rebuild();
    }

    /// Cycles every section together - magit's `S-TAB`.
    ///
    /// Deliberately ignores the cursor: a global cycle that depended on
    /// where you were standing would be a second per-section cycle with a
    /// harder key. The level is stored rather than derived, because
    /// "everything" has no single state to read back once a row has been
    /// opened by hand.
    fn cycle_all(&mut self) {
        self.level = match self.level {
            Level::Rows => Level::Sections,
            Level::Sections => Level::Rows,
        };
        match self.level {
            Level::Sections => {
                self.collapsed = self.group_names();
                // What was open is inside sections that are about to
                // close, so it shuts with them rather than springing back
                // when they reopen.
                self.systemd.shut_all();
            }
            Level::Rows => self.collapsed.clear(),
        }
        self.rebuild();
    }

    /// The units listed under the section header at `index`, in the rows
    /// as they currently stand. Empty for a collapsed section, which is
    /// correct: the only move available there is to show it.
    fn section_members(&self, index: usize) -> Vec<String> {
        self.rows
            .iter()
            .skip(index + 1)
            .take_while(|row| !matches!(row, Node::SectionHeader { .. } | Node::ProcGroup { .. }))
            .filter_map(|row| match row {
                Node::Unit { unit, .. } => Some(unit.name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Opens or closes the detail of the row under the cursor.
    ///
    /// Distinct from `toggle_fold`, which collapses a *group*. This opens
    /// a *row*, and keeping the two apart is what stops the key that hides
    /// a 267-row section from being the same key that looks inside one of
    /// its rows. A row with no detail does nothing rather than reporting
    /// that it has none.
    fn toggle_detail(&mut self) {
        match self.rows.get(self.cursor()) {
            Some(Node::Unit { unit, .. }) => {
                let name = unit.name.clone();
                self.systemd.toggle(self.system.as_ref(), &name);
            }
            Some(Node::Proc { proc, .. }) => {
                let pid = proc.pid;
                self.procs.toggle(self.system.as_ref(), pid);
            }
            // Opening a filesystem is the question "what is using this",
            // and it is the only thing that starts a scan: masys never
            // walks a filesystem nobody asked about.
            Some(Node::Filesystem { filesystem, .. }) => {
                self.io.open_filesystem(
                    self.scanner.as_ref(),
                    std::path::PathBuf::from(&filesystem.mount_point),
                );
            }
            // A directory opens from the tree the scan already built, so
            // drilling in costs nothing further - the same reason `dust`
            // can show any depth after one pass.
            Some(Node::DirEntry { path, .. }) => {
                let path = path.clone();
                self.io.toggle_dir(path);
            }
            _ => return,
        }
        self.rebuild();
    }

    /// Backs out of one layer, and reports whether there was one.
    ///
    /// A filter in effect goes before a drill-down, so leaving a buffer
    /// never leaves a filter set on it - coming back to a buffer you had
    /// narrowed, and finding rows still missing, is the confusing half of
    /// per-buffer filters.
    fn escape(&mut self) -> bool {
        if !self.filter().is_empty() {
            self.filter_mut().clear();
            self.rebuild();
            return true;
        }
        self.go_back()
    }

    /// Returns to where `l` was pressed, cursor on the unit it was pressed
    /// on. Reports whether there was anywhere to go, so a stray `esc`
    /// outside a drill-down does nothing rather than jumping somewhere
    /// arbitrary.
    fn go_back(&mut self) -> bool {
        let Some((buffer, unit)) = self.came_from.take() else {
            return false;
        };
        self.buffer = buffer;
        self.rebuild();
        // An empty name is what `u` leaves behind: a process row has no
        // unit of its own, and the Procs buffer's own cursor is already
        // sitting on the row that was jumped from.
        if unit.is_empty() {
            self.refresh_actions();
            return true;
        }
        // By name rather than by index: the rows were rebuilt while the
        // log was open, and the unit may have moved. Asked through
        // `jump::row_named` rather than matched here, because the finding
        // drill-down asks the identical question about three more kinds
        // of row and two owners of one question is how the footer and the
        // registry came to disagree.
        if let Some(index) = row_named(&self.rows, &RowTarget::Unit(unit)) {
            self.set_cursor(index);
            self.refresh_actions();
        }
        true
    }

    /// The unit that owns the row under the cursor, if any.
    ///
    /// Walks the cgroup path from the leaf upward and takes the first
    /// segment that names a unit the poll actually saw. Leaf-only would
    /// miss a process in a delegated sub-cgroup - a container runtime or
    /// a service that manages its own tree puts processes several levels
    /// below the unit that owns them, and `/system.slice/foo.service/bar`
    /// has a leaf of `bar`, which names nothing.
    ///
    /// Checked against the poll rather than against a list of suffixes,
    /// so a jump only ever offers a unit there is somewhere to jump to.
    fn unit_of_selected_row(&self) -> Option<String> {
        let cgroup = match self.rows.get(self.cursor())? {
            Node::Proc { proc, .. } => proc.cgroup.clone()?,
            // A group row *is* a cgroup, so it needs no process to ask.
            Node::ProcGroup { name, .. } => name.clone(),
            _ => return None,
        };
        cgroup
            .split('/')
            .rev()
            .find(|segment| self.units.iter().any(|unit| unit.name == *segment))
            .map(str::to_string)
    }

    /// Opens the systemd buffer at the unit that owns the row under the
    /// cursor.
    ///
    /// The association already existed in the data - the Procs buffer
    /// groups by cgroup, and on a systemd host a cgroup *is* a unit - it
    /// simply was not reachable without reading the path off the screen
    /// and finding the row by hand.
    ///
    /// Silent when there is no unit: the kernel bucket and a process in
    /// no cgroup have none, and an error line saying so on every stray
    /// keypress would be noise on the one buffer where `u` is bound.
    fn go_to_unit(&mut self) {
        let Some(unit) = self.unit_of_selected_row() else {
            return;
        };
        // Back to the row that was under the cursor, not to the unit -
        // `came_from` names a unit because that is what `l` needs, and a
        // process row has no unit name of its own. The Procs buffer keeps
        // its own cursor, so returning to the buffer returns to the row.
        self.came_from = Some((self.buffer, String::new()));
        self.buffer = Buffer::Systemd;

        // Reveal rather than merely navigate. A jump that lands on
        // nothing because the target was folded away, or filtered out, is
        // worse than no jump: it moves you somewhere and shows you
        // nothing, and gives no hint which of the two hid it.
        self.clear_filter(Buffer::Systemd);
        if let Some(kind) = self.units.iter().find(|u| u.name == unit).map(|u| u.kind) {
            self.collapsed.remove(kind.plural());
        }
        self.rebuild();

        // By name rather than by index, for the same reason `go_back`
        // does it: the rows were just rebuilt and nothing guarantees the
        // unit is where it was. Through `row_named` rather than a match
        // of its own - this was a third inline copy, and it already
        // disagreed with the other two by not matching `Node::Timer`.
        if let Some(index) = row_named(&self.rows, &RowTarget::Unit(unit)) {
            self.set_cursor(index);
        }
        self.refresh_actions();
    }

    /// Goes to the row the finding under the cursor is about.
    ///
    /// Does nothing where there is nothing to go to - the cursor is not
    /// on a finding, the finding names no row, or the row it names is no
    /// longer there. All three are the same answer to the operator and
    /// all three leave the cursor where it is, because the alternative is
    /// moving it somewhere unasked-for and calling that the destination.
    /// `dim_unavailable` marks the key for the first two; the third
    /// cannot be known until the jump is taken.
    ///
    /// Remembers Status as the origin so `esc` comes back, the same
    /// mechanism `l` and `u` already use. The remembered name is empty
    /// because Status keeps its own cursor: there is nothing to find on
    /// the way back, the row is already under it.
    fn jump_to_finding(&mut self) {
        let Some(jump) = self.finding_jump() else {
            return;
        };
        // The Log buffer holds no rows until it has been told whose
        // log it is, so this destination is opened rather than moved
        // to - by the same call `l` makes.
        if let RowTarget::UnitLog(unit) = &jump.row {
            let unit = unit.clone();
            self.open_log(&unit, String::new());
            self.refresh_actions();
            return;
        }
        self.came_from = Some((self.buffer, String::new()));
        self.buffer = jump.buffer;
        // Before the rows are built, so the order the finding implies is
        // the order they are built in - sorting afterwards would move the
        // row out from under the cursor.
        //
        // `set_sort`, not `sort_by`. `sort_by` is the *key's* behaviour
        // and reverses when handed the column already showing: Procs
        // defaults to cpu-descending, so `sort_by(Cpu)` on a fresh
        // session flips it to ascending and a cpu-pressure jump lands on
        // the *least* busy process.
        if let RowTarget::TopOfProcs(sort) = jump.row {
            self.procs.set_sort(sort);
        }
        // Reveal rather than merely navigate, the rule `go_to_unit`
        // already follows: a jump that lands on nothing because the
        // target was filtered out is worse than no jump, because it moves
        // you somewhere and shows you nothing while giving no hint which
        // of the two hid it. Without this, `.` on a failed-unit finding
        // with a filter left on the systemd buffer parked the cursor on a
        // section header with the unit invisible below it.
        self.clear_filter(jump.buffer);
        self.rebuild();
        match jump.row {
            // Not a named row: the answer is a position, and the first
            // one the cursor may rest on is it. `legal_cursor` walks past
            // the section header every one of these buffers opens with -
            // `set_cursor` stores an index and clamps nothing.
            RowTarget::TopOfProcs(_) => {
                let top = self.legal_cursor(0);
                self.set_cursor(top);
            }
            row => {
                if let Some(index) = row_named(&self.rows, &row) {
                    self.set_cursor(index);
                }
            }
        }
        self.refresh_actions();
    }

    /// The jump the row under the cursor offers, if it offers one.
    ///
    /// One source for the handler above and for the footer's dim check,
    /// which asks the identical question. Two copies is how a footer
    /// comes to advertise a key its handler will refuse.
    fn finding_jump(&self) -> Option<Jump> {
        match self.rows.get(self.cursor()) {
            Some(Node::Finding { presentation, .. }) => presentation.jump().cloned(),
            _ => None,
        }
    }

    /// Switches to `buffer` and rebuilds.
    ///
    /// Nothing is fetched on arrival, except by the Packages buffer
    /// below, which is read only while it is open. The log buffer is the
    /// only other one with content it does not already hold, and it is
    /// reached by naming a unit with `l` rather than by jumping to it -
    /// so jumping to it shows whichever unit was last opened, and says so
    /// when that is none.
    fn open(&mut self, buffer: Buffer) {
        self.buffer = buffer;
        // Opening the Packages buffer *is* its first read: it is read only
        // while it is open, so nothing has asked the port yet. Reported
        // rather than kept quiet, unlike the tick underneath it - the
        // operator pressed a key, and a host whose package database
        // cannot be read has to say so instead of showing an empty list
        // that means "not read yet" and looks like "nothing installed".
        if buffer == Buffer::Packages {
            let read = self
                .package_service
                .as_ref()
                .map(|service| self.packages.open(service.as_ref()));
            if let Some(result) = read {
                self.report(result);
            }
        }
        self.rebuild();
    }

    /// Types into the filter, applying it on every keystroke.
    ///
    /// Escape restores what was there before rather than clearing it: an
    /// accidental `/` should not throw away a filter that was already
    /// narrowing a 300-row buffer.
    fn type_filter(&mut self, key: Key) -> Flow {
        match key.code {
            KeyCode::Esc => {
                self.typing_filter = false;
                let restored = std::mem::take(&mut self.filter_before);
                *self.filter_mut() = restored;
            }
            KeyCode::Enter => self.typing_filter = false,
            KeyCode::Char(c) => self.filter_mut().push(c),
            KeyCode::Backspace => {
                self.filter_mut().pop();
            }
            // Every other key is ignored rather than guessed at: an
            // unmodelled key arrives as `Other`, and treating that as
            // "delete one" would make arrow keys eat the filter.
            _ => {}
        }
        self.rebuild();
        Flow::Continue
    }

    /// Answers a pending confirmation. `y` confirms; anything else does
    /// not - a destructive action should need the one key that means yes,
    /// not merely a key that is not `n`.
    /// Opens the transient for the row the cursor is on.
    ///
    /// Only a unit row has one today. A row that has none opens nothing
    /// rather than opening an empty popup, which is the same choice
    /// `crate::buffer` makes about a digit with no buffer behind it: a
    /// buffer that explains why it is empty is worse than a key that does
    /// nothing.
    ///
    /// A failed ownership read is passed on as `None` rather than being
    /// flattened into `Imperative`. The two are different answers, and
    /// only one of them was measured.
    ///
    /// Unit rows only, in both buffers that show one - the same popup
    /// either way, since `nix_buffer.rs` builds the Nix buffer's units
    /// section out of the identical `Node::Unit`. The Nix buffer's own
    /// operations have their own doorway now: [`App::open_nix_family`].
    fn open_row_transient(&mut self) {
        let Some(unit) = self.selected_unit() else {
            return;
        };
        let ownership = self.platform.unit_ownership(&unit).ok();
        self.mode = Mode::Transient(unit_transient(&unit, ownership.as_ref()));
    }

    /// One Nix verb-family's popup, for whatever row the cursor is on.
    ///
    /// Unlike [`App::open_row_transient`] this never refuses: Rebuild,
    /// Inputs, Store and Search need no row at all, and Generation opens
    /// dimmed and marked rather than not opening when the cursor is off a
    /// generation - the same never-hide rule every other popup in masys
    /// follows.
    fn open_nix_family(&mut self, family: NixFamily) {
        self.mode = Mode::Transient(
            self.nix
                .family_transient(family, self.rows.get(self.cursor())),
        );
    }

    /// One keypress against the open transient.
    ///
    /// Four things can happen, and the order is the point. An action row
    /// wins over a switch, so a transient that gives a switch a letter
    /// its actions already use loses the switch rather than the action.
    /// Dispatching *closes* the transient first, the way magit's does:
    /// the action may raise a confirmation, and answering one through a
    /// popup that is still covering the rows it names is worse than
    /// having to press the key again.
    fn answer_transient(&mut self, key: Key) -> Flow {
        // Escape leaves. Checked before the chords so no transient can
        // bind its way out of being closeable - the same rule the buffer
        // escape ladder follows.
        if key.code == KeyCode::Esc {
            self.mode = Mode::Buffer;
            self.refresh_actions();
            return Flow::Continue;
        }
        let KeyCode::Char(chord) = key.code else {
            return Flow::Continue;
        };
        // The caller matched this arm to get here, so the state is
        // settled before the call. Said rather than silently returned:
        // a `Flow::Continue` here would be a third answer to "which
        // popup is open", quietly disagreeing with the two above it.
        let Mode::Transient(def) = &mut self.mode else {
            unreachable!("answer_transient is only reached from Mode::Transient")
        };

        match def.action(chord) {
            // `-r` reroutes `switch` to `rollback` rather than gating a
            // second `NixOp` behind the same row: `nixos-rebuild switch
            // --rollback` *is* `NixOp::Rollback`, the domain's own doc on
            // that variant says so plainly, and the two were only ever
            // separate rows because switches did not exist yet when this
            // popup was one function. The row's own label still reads
            // "switch" - the switch is what changed, not the row - so the
            // rename lives here, at dispatch, rather than in the row.
            Some(Action::Nix(NixVerb::Rebuild(RebuildVerb::Switch)))
                if def
                    .switches
                    .iter()
                    .any(|switch| switch.chord == "-r" && switch.on) =>
            {
                self.mode = Mode::Buffer;
                self.dispatch(Some(Action::Nix(NixVerb::Rollback)))
            }
            // `-u` reroutes the same row for the same reason `-r` does:
            // `nixos-rebuild switch --upgrade` is `NixOp::Upgrade`, a flag
            // on `switch` rather than an action of its own.
            //
            // After `-r`, and the order is the decision. Both switches on
            // is a contradiction - `--rollback` builds nothing, so
            // channels updated on the way to it would be updated and then
            // not read - and rollback is the safer of the two to honour:
            // it touches no channel and the generation it activates
            // already exists. An operator who armed both gets the one that
            // changes least.
            Some(Action::Nix(NixVerb::Rebuild(RebuildVerb::Switch)))
                if def
                    .switches
                    .iter()
                    .any(|switch| switch.chord == "-u" && switch.on) =>
            {
                self.mode = Mode::Buffer;
                self.dispatch(Some(Action::Nix(NixVerb::Upgrade)))
            }
            Some(action) => {
                self.mode = Mode::Buffer;
                self.dispatch(Some(action))
            }
            // A switch toggles in place: the popup stays open, because
            // the point of a switch is to set several before running
            // anything.
            None => {
                def.toggle(chord);
                Flow::Continue
            }
        }
    }

    /// One keypress against the open input sub-step.
    ///
    /// Escape backs out, enter commits, backspace deletes, and every
    /// other printable character is typed. Nothing else is bound: this is
    /// a prompt, and a prompt that swallowed a movement key would leave
    /// the operator unable to tell it from the buffer.
    ///
    /// An empty value does not commit. `nix search nixpkgs ""` matches
    /// every package in nixpkgs, and `nix-env --delete-generations ""`
    /// is an argument error - neither is what pressing enter on an empty
    /// prompt meant.
    fn answer_input(&mut self, key: Key) -> Flow {
        let Mode::Input(state) = &mut self.mode else {
            unreachable!("answer_input is only reached from Mode::Input")
        };
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Buffer;
                self.refresh_actions();
                Flow::Continue
            }
            KeyCode::Backspace => {
                state.typed.pop();
                Flow::Continue
            }
            KeyCode::Enter => {
                if state.typed.is_empty() {
                    return Flow::Continue;
                }
                let (verb, typed) = (state.verb, state.typed.clone());
                self.mode = Mode::Buffer;
                match self.nix.op_typed(verb, &typed) {
                    Some(op) => self.run_nix(op),
                    None => Flow::Continue,
                }
            }
            KeyCode::Char(typed) => {
                state.typed.push(typed);
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    fn answer_confirm(&mut self, key: Key) -> Flow {
        // Taken whatever the answer was: any key closes the
        // confirmation, and `y` is the only one that also runs it.
        let Mode::Confirm { pending, .. } = std::mem::replace(&mut self.mode, Mode::Buffer) else {
            unreachable!("answer_confirm is only reached from Mode::Confirm")
        };
        if key.code == KeyCode::Char('y') {
            match pending {
                Pending::Kill { pid, signal } => {
                    let result = self.system.kill(pid, signal);
                    self.report(result);
                }
                // Edit is the one verb not run here: it needs the
                // terminal, which the session does not own. It is recorded
                // and the caller is asked for it.
                Pending::Unit {
                    unit,
                    verb: UnitVerb::Edit,
                } => {
                    self.suspended = Some(Suspended::EditUnit(unit));
                    self.rebuild();
                    return Flow::Suspend;
                }
                // Like edit, and for the same reason: every one of these
                // owns the screen while it runs, so the session records
                // it and asks the caller for the terminal.
                Pending::Nix { op } => {
                    self.suspended = Some(Suspended::Nix(op));
                    self.rebuild();
                    return Flow::Suspend;
                }
                Pending::Unit { unit, verb } => {
                    let result = match verb {
                        UnitVerb::Start => self.system.start(&unit),
                        UnitVerb::Stop => self.system.stop(&unit),
                        UnitVerb::Restart => self.system.restart(&unit),
                        UnitVerb::TryRestart => self.system.try_restart(&unit),
                        UnitVerb::Reload => self.system.reload(&unit),
                        UnitVerb::Enable => self.system.enable(&unit),
                        UnitVerb::Disable => self.system.disable(&unit),
                        UnitVerb::Mask => self.system.mask(&unit),
                        UnitVerb::Unmask => self.system.unmask(&unit),
                        UnitVerb::ResetFailed => self.system.reset_failed(&unit),
                        UnitVerb::Edit => unreachable!("handled above"),
                    };
                    self.report(result);
                }
            }
        }
        self.rebuild();
        Flow::Continue
    }

    /// Runs whatever `Flow::Suspend` was asked for, with the terminal in
    /// the caller's hands, and says what it left on the screen.
    ///
    /// Called by the composition root between releasing the terminal and
    /// restoring it. Does nothing if nothing is waiting, so a caller that
    /// calls it unconditionally is not wrong - which is what makes the
    /// contract hard to get subtly wrong.
    ///
    /// A failure is held on the status line like any other action's, and
    /// is worth holding: `systemctl edit` refuses outright where the unit
    /// directory is read-only, and `nixos-rebuild` exits non-zero on an
    /// evaluation error. In both cases the command's own message has
    /// already reached the terminal the caller handed over; what the
    /// status line adds is that it failed at all, which the operator
    /// would otherwise lose the moment the screen was redrawn.
    ///
    /// The same failure is also why an `Err` is always [`Aftermath::Unseen`],
    /// including the editor's. `Seen` rests entirely on the editor having
    /// held the terminal until the operator quit it - and a `systemctl
    /// edit` that was *refused* never opened one. What is on the screen
    /// then is a single line of systemctl's own text, in exactly the
    /// position the diff was in: printed and then swallowed. The two
    /// mistakes are not the same size either. Pausing where it was not
    /// needed costs one keypress; not pausing costs the only full account
    /// of why an operation failed, so this leans toward pausing.
    pub fn run_suspended(&mut self) -> Aftermath {
        let Some(suspended) = self.suspended.take() else {
            return Aftermath::Seen;
        };
        // Whether the program holds the terminal until the operator leaves
        // it, or prints and exits. Answered beside the call rather than by
        // matching on the variant again further down: a third variant
        // added to `Suspended` cannot then compile without an answer.
        let (result, interactive) = match suspended {
            Suspended::EditUnit(unit) => (self.system.edit(&unit), true),
            Suspended::Nix(op) => {
                let result = match &self.declarative {
                    Some(port) => port.run(&op),
                    // Unreachable as things stand: `with_declarative` asserts
                    // that the registry and the port agree, so a host without
                    // one has no Nix buffer and no key that could queue this.
                    // An `Err` rather than a panic anyway, on the reasoning
                    // `ops::spawn` gives for its own unreachable guard - the
                    // cost of being wrong is masys aborting with the terminal
                    // already handed away, and a future caller that queues an
                    // operation from somewhere else should surface as a
                    // refusal on the status line.
                    None => Err(MasysError::Platform(
                        "this host has no declarative service".to_string(),
                    )),
                };
                (result, holds_the_terminal(&op))
            }
        };
        let aftermath = if interactive && result.is_ok() {
            Aftermath::Seen
        } else {
            Aftermath::Unseen
        };
        self.report(result);
        self.rebuild();
        aftermath
    }

    /// The process under the cursor, if the cursor is on one.
    fn selected_proc(&self) -> Option<(u32, String, i32)> {
        crate::procs::selected(self.rows.get(self.cursor()))
    }

    fn ask_kill(&mut self, signal: Signal) {
        let Some((pid, name, _)) = self.selected_proc() else {
            return;
        };
        // The signal is named in the prompt rather than implied by which
        // key was pressed: SIGKILL cannot be caught, and the difference
        // is the whole reason there are two bindings.
        let verb = if signal == Signal::Kill {
            "SIGKILL"
        } else {
            "SIGTERM"
        };
        self.mode = Mode::Confirm {
            pending: Pending::Kill { pid, signal },
            prompt: format!("{verb} {name} (pid {pid})?  y / n"),
        };
    }

    /// Runs what the key names, or does nothing where the row under the
    /// cursor cannot supply the arguments.
    ///
    /// Returning early on the wrong row is `ask_kill`'s shape - `k` on a
    /// group row resolves and then quietly does nothing - and the footer
    /// dims the key in exactly that case, so quiet is not silent.
    ///
    /// What changes the machine asks first, through the same `Pending`
    /// and `ModalView::Confirm` the ten unit verbs and both signals
    /// already use. What only reads suspends straight from the keypress:
    /// a confirmation on an operation that changes nothing teaches the
    /// operator that confirmations are noise, and the next one they
    /// dismiss will be a rebuild.
    fn nix(&mut self, verb: NixVerb) -> Flow {
        let op = match self.nix.offer(self.rows.get(self.cursor()), verb) {
            NixOffer::Ready(op) => op,
            // The row is live and the value is what is missing, so the
            // input sub-step opens rather than the operation running.
            NixOffer::Asks { prompt } => {
                self.mode = Mode::Input(InputState {
                    verb,
                    prompt,
                    typed: String::new(),
                });
                return Flow::Continue;
            }
            NixOffer::No => return Flow::Continue,
        };
        self.run_nix(op)
    }

    /// Confirms an operation, or suspends straight into it.
    fn run_nix(&mut self, op: NixOp) -> Flow {
        if only_reads(&op) {
            self.suspended = Some(Suspended::Nix(op));
            return Flow::Suspend;
        }
        // An operation with no sentence is not offered. Unreachable while
        // `NixVerb` names three - both of the ones that change something
        // have one below - and the fail-safe direction if a fourth
        // arrives without one: refusing costs a keypress, while
        // approving an act nobody could describe costs a machine.
        let Some(what) = prompt_for(&op) else {
            return Flow::Continue;
        };
        self.mode = Mode::Confirm {
            pending: Pending::Nix { op },
            prompt: format!("{what}  y / n"),
        };
        Flow::Continue
    }

    /// Whether the unit under the cursor has failed.
    fn selected_unit_failed(&self) -> bool {
        matches!(self.rows.get(self.cursor()), Some(Node::Unit { unit, .. }) if unit.active_state == ActiveState::Failed)
    }

    /// The unit under the cursor, if the cursor is on one.
    fn selected_unit(&self) -> Option<String> {
        match self.rows.get(self.cursor()) {
            Some(Node::Unit { unit, .. }) => Some(unit.name.clone()),
            Some(Node::Timer { name, .. }) => Some(name.clone()),
            _ => None,
        }
    }

    /// Asks before running a systemctl verb, and consults the
    /// persistence-ownership guard first for the two that change what
    /// happens at boot.
    ///
    /// The guard is the point: on a declaratively managed host, enabling
    /// a unit either fails outright or is undone by the next rebuild, and
    /// an operator finding that out afterwards is exactly what the design
    /// built `Ownership::Declarative` to prevent. It is named in the
    /// prompt rather than reported after the fact.
    fn ask_unit(&mut self, verb: UnitVerb) {
        let Some(unit) = self.selected_unit() else {
            return;
        };
        let mut prompt = format!("{} {unit}?", verb.label());
        if verb.is_persistent() {
            // Asked of `systemd_buffer` rather than matched here, for
            // `dim_unavailable`'s reason one level along: this was the
            // third place in the program deciding what a failed ownership
            // read means, and #12 was two of the other two disagreeing.
            if let Some(caveat) = ownership_caveat(self.platform.unit_ownership(&unit).as_ref()) {
                prompt.push_str(&caveat);
            }
        }
        prompt.push_str("  y / n");
        self.mode = Mode::Confirm {
            pending: Pending::Unit { unit, verb },
            prompt,
        };
    }

    /// Reads the log the other way round.
    ///
    /// The cursor goes back to the top rather than following the row it
    /// was on: the point of flipping is to look at the other end, and
    /// landing where you already were would make the key look inert.
    fn toggle_log_order(&mut self) {
        self.log.flip_order();
        self.rebuild();
        let top = self.legal_cursor(0);
        self.set_cursor(top);
    }

    /// Whose log `l` should open for the row under the cursor.
    ///
    /// A unit's own, except for a timer, where it is the unit the timer
    /// *runs*. A timer logs almost nothing itself - on this host
    /// `journalctl -u foo.timer` returns two dozen lines, every one of
    /// them pid 1 saying "Started foo.timer" - while the output anyone
    /// pressing `l` on a timer wants is the job's. It is the same reason
    /// the Timers section exists: a timer's own state, and its own log,
    /// answer the wrong question.
    fn log_target(&self) -> Option<String> {
        match self.rows.get(self.cursor()) {
            Some(Node::Unit { unit, .. }) => Some(unit.name.clone()),
            Some(Node::Timer { activates, .. }) => Some(activates.clone()),
            _ => None,
        }
    }

    /// Shows the log for the unit under the cursor.
    ///
    /// A real `journalctl -u` query rather than filtering the entries the
    /// Journal buffer already had. Text-filtering was measurably useless
    /// for the case this key exists for: on this host the one failed unit
    /// has 320 journal entries and none of them fall inside the Journal
    /// buffer's one-hour window, so pressing `l` on it showed nothing at
    /// all - while a text match would also have caught every other
    /// service that happened to mention the name.
    fn show_logs(&mut self) {
        let Some(unit) = self.log_target() else {
            return;
        };
        // Back goes to the row that was under the cursor, which for a
        // timer is the timer - not the unit whose log this is.
        let Some(from) = self.selected_unit() else {
            return;
        };
        self.open_log(&unit, from);
    }

    /// Opens the Log buffer on one unit, from wherever you were.
    ///
    /// The one path to this buffer. `l` reaches it with the unit under
    /// the cursor; a jump from a recent-error finding reaches it with
    /// the unit that logged the line, and that row is a finding rather
    /// than a unit so it has no `selected_unit` to offer. Two ways of
    /// opening a log would be two places for the filter rule and the
    /// failed-fetch rule below to drift apart.
    fn open_log(&mut self, unit: &str, from: String) {
        self.came_from = Some((self.buffer, from));

        // A fresh look at a unit is a fresh view of it. Filters are per
        // buffer, which is right for the four fixed ones - but this buffer's
        // *identity* is whichever unit `l` last named, so a filter that
        // made sense for one unit's log would carry onto the next one's
        // and silently hide most of it. Leaving a buffer narrowed and coming
        // back is a state you chose; arriving somewhere new already
        // narrowed is not.
        //
        // A tick or `g` does not come through here, so a filter set on the
        // log you are reading survives its own refreshes.
        self.clear_filter(Buffer::Log);

        // The scope moves before the read, and the entries are replaced
        // whatever the read returned. Assigning them only on success left
        // a failed fetch showing the *previous* unit's log under the
        // previous unit's name, with an error line underneath that most
        // people would not read before believing the rows.
        match self.log.open(self.system.as_ref(), unit) {
            Ok(()) => self.error = None,
            Err(e) => self.report(Err(e)),
        }
        self.buffer = Buffer::Log;
        self.rebuild();
    }

    /// Nudges niceness by one, immediately.
    ///
    /// No confirmation: it is reversible, and htop's `[`/`]` taught
    /// everyone that trying it is free. Lowering niceness needs privilege,
    /// so the refusal is reported rather than swallowed.
    fn nudge_nice(&mut self, delta: i32) {
        let Some((pid, name, nice)) = self.selected_proc() else {
            return;
        };
        let _ = name;
        let result = self.system.renice(pid, nice + delta);
        self.report(result);
    }

    /// Holds a failure for display, and clears the last one on success.
    ///
    /// The design's error table: an action's failure is shown and *held*,
    /// because a refresh two seconds later must not silently erase the
    /// reason something did not happen.
    fn report(&mut self, result: Result<(), MasysError>) {
        self.error = result.err().map(|e| e.to_string());
    }

    /// Recomputes the footer's action keys for wherever the cursor is.
    ///
    /// Called after every keypress rather than from `set_cursor`, which
    /// runs once per step: a page-down would otherwise redo this ten
    /// times to land in one place.
    fn refresh_actions(&mut self) {
        self.actions = self.dim_unavailable(self.keymap.actions(self.buffer));
    }

    /// Marks the keys that will not do what they promise here.
    ///
    /// Dimmed rather than removed: the design's rule is that masys never
    /// hides an action outright, only marks it. A key that vanishes when
    /// it is inapplicable punishes muscle memory and leaves the operator
    /// wondering whether they misremembered the binding; a dimmed one in
    /// its usual place answers the question on sight.
    ///
    /// Every guard here asks one question: will this key act on the row
    /// the cursor is on. A unit verb needs a unit under it, needs the
    /// manifest not to own that unit where the verb writes to boot - the
    /// judgement is per *unit*, not per host, because a hand-placed unit
    /// on a declarative machine is genuinely imperative - and, for reset
    /// failed, needs a failure. A Nix key needs `nix_op` to be able to
    /// build its operation - the row supplying the arguments, and the
    /// profile being writable where the operation writes one - which is
    /// asked by calling `nix_op` rather than by restating it.
    fn dim_unavailable(&self, actions: Vec<KeyBinding>) -> Vec<KeyBinding> {
        let on_a_unit = self.selected_unit().is_some();
        // `.ok()` throws the error away, so this is asked of
        // `Persistence` rather than of the `Option` directly: a read that
        // failed lands on `Unknown` and marks the row, where
        // `is_some_and` landed it on `false` and drew `D`, `M` and `U`
        // live - telling the operator their disable would stick, which
        // nobody had measured.
        let persistence = Persistence::of(
            self.selected_unit()
                .and_then(|unit| self.platform.unit_ownership(&unit).ok())
                .as_ref(),
        );
        let failed = self.selected_unit_failed();

        actions
            .into_iter()
            .map(
                |binding| match self.keymap.action_for(self.buffer, &binding.chord) {
                    // Three reasons a unit verb will not do what it says.
                    // The row under the cursor is not a unit at all, which is
                    // most of the Nix buffer: these keys resolve there because
                    // that buffer has a units section, but the cursor spends
                    // its time on generations and inputs, where `ask_unit`
                    // returns without asking anything. Or the manifest owns
                    // the unit. Or the verb applies only to a failure that has
                    // not happened. All three are marked rather than removed.
                    Some(Action::Unit(verb)) => {
                        let dimmed = !on_a_unit
                            || (verb.is_persistent() && persistence.marked())
                            || (verb.needs_failure() && !failed);
                        KeyBinding { dimmed, ..binding }
                    }
                    // One question for a Nix key, and it is the same one the
                    // handler asks: can `nix_op` build the operation here.
                    // Asking it rather than restating its conditions is what
                    // keeps the footer from claiming a key the handler will
                    // refuse - and it is why the privilege check needed no
                    // second home in the footer.
                    Some(Action::Nix(verb)) => KeyBinding {
                        dimmed: self.nix.offer(self.rows.get(self.cursor()), verb) == NixOffer::No,
                        ..binding
                    },
                    // And `l`, which needs a unit whose log to open. Asked of
                    // `log_target` rather than of `selected_unit` because
                    // they differ on a timer row: `l` there opens the log of
                    // the unit the timer *runs*, so the key applies where the
                    // other question would have said it did not.
                    Some(Action::Logs) => KeyBinding {
                        dimmed: self.log_target().is_none(),
                        ..binding
                    },
                    // The transient opens on a unit row and nowhere else, in
                    // both buffers that show one, so off one it is dim - and
                    // only for that reason. It does *not* dim because the
                    // popup's own rows are dim: the ownership guard marks
                    // `enable` inside the popup, and a key that refused to
                    // open a popup whose rows are marked would hide the very
                    // explanation the mark exists to give.
                    Some(Action::Transient) => KeyBinding {
                        dimmed: !on_a_unit,
                        ..binding
                    },
                    // The finding jump, asked the same question its own
                    // handler asks: is the cursor on a finding, and does
                    // that finding name a row. Most findings do; the four
                    // `jump_target` lists as answering `None` permanently
                    // do not, and on those
                    // the key is marked rather than offered and then
                    // silently doing nothing - which is the shape that
                    // put `[5] nix` in the footer of a host where `5`
                    // did nothing.
                    Some(Action::JumpToFinding) => KeyBinding {
                        dimmed: self.finding_jump().is_none(),
                        ..binding
                    },
                    // A Nix family menu, asked the same question `Action::Nix`
                    // is asked just above: build the popup it is about to
                    // show and see whether anything in it is live. Rebuild,
                    // Inputs, Store and Search need no row at all, so this is
                    // `false` on nearly every row of the Nix buffer; only
                    // Generation, off a generation, comes back dim.
                    Some(Action::NixMenu(family)) => {
                        let live = self
                            .nix
                            .family_transient(family, self.rows.get(self.cursor()))
                            .groups
                            .iter()
                            .flat_map(|group| &group.rows)
                            .any(|row| !row.dimmed);
                        KeyBinding {
                            dimmed: !live,
                            ..binding
                        }
                    }
                    _ => binding,
                },
            )
            .collect()
    }

    /// Every group currently in the buffer, for collapse-all.
    fn group_names(&self) -> HashSet<String> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Node::ProcGroup { name, .. } => Some(name.clone()),
                // The same two questions `cycle_section` asks, asked of
                // the same two methods, so collapse-all and one section's
                // `TAB` cannot disagree about what a section is or about
                // what it is filed under.
                Node::SectionHeader { title, kind, .. } if kind.folds() => {
                    Some(kind.fold_key(title))
                }
                _ => None,
            })
            .collect()
    }

    /// True while a log is on screen, so the session can wake often
    /// enough to follow it.
    ///
    /// Asked of the app rather than assumed by the loop, because "which
    /// buffer is open" is the app's to know - the same reason `wants_refresh`
    /// exists.
    pub fn following(&self) -> bool {
        self.buffer == Buffer::Log && self.log.unit.is_some()
    }

    /// Picks up whatever has been appended to the open log.
    ///
    /// Reports whether anything arrived, so the session can skip a redraw
    /// it does not need - this runs four times a second and answers "no"
    /// almost every time.
    pub fn follow_log(&mut self) -> bool {
        if !self.log.follow(self.system.as_ref()) {
            return false;
        }
        self.rebuild();
        true
    }

    /// True when the key means "sample now", so the caller can do it with
    /// its own clock rather than the session inventing one.
    pub fn wants_refresh(&self, key: Key) -> bool {
        self.keymap.resolve(self.buffer, key) == Some(Action::Refresh)
    }

    /// The popup, if any.
    ///
    /// At most one is ever open, and now that is a property of the type
    /// rather than of the order this function asks its questions in. It
    /// used to be a chain of four `if`s mirroring the one in `handle_key`,
    /// so what was drawn and what took the keystrokes were two answers to
    /// one question that happened to agree.
    fn modal(&self) -> Option<ModalView<'_>> {
        match &self.mode {
            // The filter is not a popup: it belongs at the top of the
            // buffer it is narrowing, where it stays visible while the
            // rows move under it. A centred modal hid the very rows it
            // was filtering.
            Mode::Buffer => None,
            Mode::Help => {
                // The same dimming as the footer: the two must not
                // disagree about whether a key applies.
                let groups = self
                    .keymap
                    .describe(self.buffer)
                    .into_iter()
                    .map(|group| KeyGroup {
                        bindings: self.dim_unavailable(group.bindings),
                        ..group
                    })
                    .collect();
                Some(ModalView::Keys { groups })
            }
            Mode::Input(state) => Some(ModalView::Input {
                prompt: state.prompt,
                typed: &state.typed,
            }),
            Mode::Transient(def) => Some(def.view()),
            Mode::Confirm { prompt, .. } => Some(ModalView::Confirm { prompt }),
        }
    }

    /// Everything the renderer needs for one frame, and nothing else.
    pub fn view(&self) -> View<'_> {
        View {
            header: match self.buffer {
                Buffer::Status => Header::Status {
                    hostname: &self.hostname,
                    timestamp: &self.timestamp,
                },
                Buffer::Procs => Header::Procs {
                    sort: self.procs.sort,
                    descending: self.procs.descending,
                },
                Buffer::Systemd => Header::Titled("systemd"),
                Buffer::Packages => Header::Titled("packages"),
                Buffer::Log => Header::Log {
                    unit: self.log.unit.as_deref().unwrap_or("log"),
                    newest_first: self.log.newest_first,
                },
                Buffer::Io => Header::Titled("IO"),
                Buffer::Nix => Header::Titled("Nix"),
            },
            rows: &self.rows,
            selected: (!self.rows.is_empty()).then(|| self.cursor()),
            // The operator's answer first. An action's failure is held
            // until they do something else - the design's error table
            // says a refresh must not erase the reason something did not
            // happen - so an ambient sample failure waits its turn
            // rather than overwriting what they are owed.
            status: match (&self.error, &self.sample_error) {
                (Some(message), _) | (None, Some(message)) => StatusLine::Error(message),
                (None, None) => StatusLine::Hints,
            },
            modal: self.modal(),
            hints: &self.hints,
            actions: &self.actions,
            filter: (!self.filter().is_empty() || self.typing_filter).then_some(self.filter()),
            typing: self.typing_filter,
            filter_matches: self.filter_matches,
            auto_refresh_paused: self.auto_refresh_paused(),
        }
    }
}

/// Rows a page key moves by. Fixed rather than the rendered height for the
/// reason given at its call site.
const PAGE: usize = 10;
