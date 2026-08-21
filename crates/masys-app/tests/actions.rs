mod fake;

use fake::{FakePlatformService, FakeSystemService, proc};
use masys_app::key::{Key, KeyCode};
use masys_app::{Aftermath, App, Buffer, Flow};
use masys_domain::sample::{Pressure, Snapshot, SystemState};
use masys_view::{ModalView, Node, StatusLine};

fn snapshot() -> Snapshot {
    Snapshot {
        taken_at_ms: 0,
        procs: vec![with_cgroup(proc(1, "chrome", 0)), with_cgroup(proc(2, "rust-analyzer", 0))],
        pressure: Some(Pressure::default()),
        filesystems: Vec::new(),
        disks: Vec::new(),
        clock_synced: true,
        utc_offset_secs: -21_600,
        interfaces: Vec::new(),
        oom_kills: Vec::new(),
        system_state: SystemState::Running,
        clock_ticks_per_sec: 100,
        machine: None,
        load: None,
        uptime_secs: None,
        memory: None,
    }
}

fn with_cgroup(mut p: masys_domain::sample::Proc) -> masys_domain::sample::Proc {
    p.cgroup = Some("/user.slice".to_string());
    p
}

type Calls = std::rc::Rc<std::cell::RefCell<Vec<String>>>;

fn app_with(fails_with: Option<String>) -> (App, Calls) {
    let calls: Calls = Default::default();
    let system = FakeSystemService {
        snapshot: snapshot(),
        units: Vec::new(),
        calls: calls.clone(),
        fails_with,
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(1_000, "t".to_string()).expect("tick");
    (app, calls)
}

fn press(app: &mut App, keys: &str) {
    for c in keys.chars() {
        app.handle_key(Key::char(c));
    }
}

/// Puts the cursor on a process rather than the group header it starts on.
fn on_a_process(app: &mut App) {
    press(app, "2");
    for _ in 0..8 {
        if matches!(app.view().rows.get(app.view().selected.unwrap_or(0)), Some(Node::Proc { .. })) {
            return;
        }
        app.handle_key(Key::new(KeyCode::Down));
    }
    panic!("no process row reached");
}

/// A signal is destructive and uncatchable in one of its two forms, so it
/// asks first. Nothing reaches the port until it is answered.
#[test]
fn killing_asks_before_it_acts() {
    let (mut app, calls) = app_with(None);
    on_a_process(&mut app);
    press(&mut app, "k");

    assert!(matches!(app.view().modal, Some(ModalView::Confirm { .. })), "a confirmation opens");
    assert!(calls.borrow().is_empty(), "nothing was signalled yet: {:?}", calls.borrow());
}

/// A destructive action needs the one key that means yes, not merely a
/// key that is not `n`.
#[test]
fn only_y_confirms_and_anything_else_declines() {
    for (answer, expected) in [("y", 1), ("n", 0), ("x", 0)] {
        let (mut app, calls) = app_with(None);
        on_a_process(&mut app);
        press(&mut app, "k");
        press(&mut app, answer);
        assert_eq!(calls.borrow().len(), expected, "answering {answer:?}");
        assert!(app.view().modal.is_none(), "the confirmation closes either way");
    }
}

#[test]
fn the_confirmation_names_the_signal_and_the_process() {
    let (mut app, _) = app_with(None);
    on_a_process(&mut app);
    press(&mut app, "K");
    let Some(ModalView::Confirm { prompt }) = app.view().modal else { panic!("no confirmation") };
    assert!(prompt.contains("SIGKILL"), "{prompt}");
    assert!(prompt.contains("pid"), "{prompt}");
}

/// Nudging niceness is reversible, and htop's `[`/`]` taught everyone it
/// is free to try - so it acts immediately rather than asking.
#[test]
fn nicing_acts_without_asking() {
    let (mut app, calls) = app_with(None);
    on_a_process(&mut app);
    press(&mut app, "]");
    assert!(app.view().modal.is_none(), "no confirmation for a reversible nudge");
    assert_eq!(calls.borrow().len(), 1, "{:?}", calls.borrow());
    assert!(calls.borrow()[0].starts_with("renice"), "{:?}", calls.borrow());
}

/// The design's error table: an action's failure is shown and *held*,
/// because a refresh two seconds later must not silently erase the reason
/// something did not happen.
#[test]
fn a_refused_action_reports_and_the_message_survives_a_tick() {
    let (mut app, _calls) = app_with(Some("not permitted for process 1".to_string()));
    on_a_process(&mut app);
    press(&mut app, "]");

    assert!(matches!(app.view().status, StatusLine::Error(_)), "the failure is shown");
    app.tick(2_000, "t".to_string()).expect("tick");
    let StatusLine::Error(message) = app.view().status else { panic!("the error was erased by a refresh") };
    assert!(message.contains("not permitted"), "{message}");
}

/// An action key on a row that is not a process does nothing - folding a
/// group has meaning, signalling one does not.
#[test]
fn an_action_on_a_non_process_row_does_nothing() {
    let (mut app, calls) = app_with(None);
    press(&mut app, "2");
    assert!(matches!(app.view().rows.first(), Some(Node::ProcGroup { .. })), "the cursor starts on a group");
    press(&mut app, "k");
    press(&mut app, "]");
    assert!(app.view().modal.is_none());
    assert!(calls.borrow().is_empty(), "{:?}", calls.borrow());
}

/// Filtering live as you type is htop's behaviour and lnav's, and it is
/// what makes the key worth pressing on a 300-row buffer.
#[test]
fn filtering_narrows_the_buffer_as_it_is_typed() {
    let (mut app, _) = app_with(None);
    press(&mut app, "2");
    let before = app.view().rows.len();

    press(&mut app, "/");
    assert!(app.view().typing, "the filter is taking keys");
    press(&mut app, "chrome");
    assert_eq!(app.view().filter, Some("chrome"), "and shows what was typed");

    let names: Vec<String> = app
        .view()
        .rows
        .iter()
        .filter_map(|r| match r {
            Node::Proc { proc, .. } => Some(proc.comm.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["chrome"], "only the match survives");
    assert!(app.view().rows.len() < before);
}

/// Escape with nothing to go back to leaves the buffer unnarrowed.
#[test]
fn escape_clears_the_filter_and_restores_every_row() {
    let (mut app, _) = app_with(None);
    press(&mut app, "2");
    let before = app.view().rows.len();

    press(&mut app, "/");
    press(&mut app, "chrome");
    app.handle_key(Key::new(KeyCode::Esc));

    assert!(app.view().filter.is_none(), "the filter is gone, not merely closed");
    assert_eq!(app.view().rows.len(), before, "every row is back");
}

/// Escape restores whatever was in effect before this typing session -
/// which, when nothing was, is nothing. The case that distinguishes the
/// two readings is the one below.
#[test]
fn escape_restores_the_filter_that_was_already_in_effect() {
    let (mut app, _) = app_with(None);
    press(&mut app, "2");

    press(&mut app, "/");
    press(&mut app, "chrome");
    app.handle_key(Key::new(KeyCode::Enter));
    let narrowed = app.view().rows.len();

    // The accident: `/` pressed with a filter already narrowing the
    // buffer, then abandoned.
    press(&mut app, "/");
    press(&mut app, "zzz");
    app.handle_key(Key::new(KeyCode::Esc));

    assert_eq!(app.view().filter, Some("chrome"), "the filter that was already there survives");
    assert!(!app.view().typing, "and it is no longer taking keys");
    assert_eq!(app.view().rows.len(), narrowed, "so the rows are the ones it was narrowing to");
}

/// A finding is the status buffer's content, not its scaffolding. If `/`
/// cannot match one, the key empties the buffer for every search an
/// operator would actually type - starting with the name of the unit
/// they can see on screen.
#[test]
fn the_status_buffer_can_be_filtered_by_a_findings_own_text() {
    let mut snapshot = snapshot();
    snapshot.system_state = masys_domain::sample::SystemState::Degraded;
    let units = vec![failed("restic-backup.service")];
    let system = FakeSystemService {
        snapshot,
        units,
        calls: Default::default(),
        fails_with: None,
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(10_000, "t".to_string()).expect("tick");

    press(&mut app, "/");
    press(&mut app, "restic");

    let findings: Vec<&Node> = app.view().rows.iter().filter(|r| matches!(r, Node::Finding(_))).collect();
    assert_eq!(findings.len(), 1, "the failed unit matches its own name: {:#?}", app.view().rows);
}

/// The other half of the same rule: a filter that matches nothing must
/// still be able to *not* match, or the test above would pass on a
/// builder that ignores the filter entirely.
#[test]
fn a_filter_matching_no_finding_empties_the_status_buffer() {
    let mut snapshot = snapshot();
    snapshot.system_state = masys_domain::sample::SystemState::Degraded;
    let system = FakeSystemService {
        snapshot,
        units: vec![failed("restic-backup.service")],
        calls: Default::default(),
        fails_with: None,
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(10_000, "t".to_string()).expect("tick");

    press(&mut app, "/");
    press(&mut app, "nosuchunit");

    assert!(app.view().rows.iter().all(|r| !matches!(r, Node::Finding(_))), "{:#?}", app.view().rows);
}

/// The whole point of the systemd view: narrow to the one unit you care
/// about, then open it where it sits.
///
/// `enter` rather than `tab`, and the two are a pair: `tab` folds a
/// *group* - a section, a cgroup - while `enter` opens a *row*. Keeping
/// them apart means the key that collapses a 267-row section is not the
/// same key you press to look inside one of its rows.
#[test]
fn filter_then_enter_expands_the_unit_in_place() {
    let mut app = systemd_app();
    press(&mut app, "3");

    press(&mut app, "/");
    press(&mut app, "restic");
    app.handle_key(Key::new(KeyCode::Enter));

    // One unit survives the filter; put the cursor on it.
    step_onto_a_unit(&mut app);
    assert_eq!(selected_unit(&app).as_deref(), Some("restic-backup.service"), "{:#?}", app.view().rows);
    assert_eq!(expanded(&app), Some(false), "a unit starts closed");

    app.handle_key(Key::new(KeyCode::Enter));
    assert_eq!(expanded(&app), Some(true), "enter opens it");

    app.handle_key(Key::new(KeyCode::Enter));
    assert_eq!(expanded(&app), Some(false), "and enter closes it again");
}

/// Rows are rebuilt from scratch every tick, so an expansion that lives in
/// the row rather than in the session would shut itself two seconds after
/// being opened.
#[test]
fn an_expanded_unit_stays_open_across_a_tick() {
    let mut app = systemd_app();
    press(&mut app, "3");
    step_onto_a_unit(&mut app);
    app.handle_key(Key::new(KeyCode::Enter));
    assert_eq!(expanded(&app), Some(true));

    app.tick(20_000, "t".to_string()).expect("tick");
    assert_eq!(expanded(&app), Some(true), "still open after a refresh");
}

/// Opening one unit must not open every unit - the set is keyed by name,
/// not a single flag.
#[test]
fn expanding_one_unit_leaves_the_others_closed() {
    let mut app = systemd_app();
    press(&mut app, "3");
    step_onto_a_unit(&mut app);
    app.handle_key(Key::new(KeyCode::Enter));

    let open: Vec<(String, bool)> = app
        .view()
        .rows
        .iter()
        .filter_map(|r| match r {
            Node::Unit { unit, expanded, .. } => Some((unit.name.clone(), *expanded)),
            _ => None,
        })
        .collect();
    assert_eq!(open.iter().filter(|(_, e)| *e).count(), 1, "exactly one is open: {open:?}");
}

/// magit's `TAB`: shows or hides the section at point.
///
/// Two states, not three. This view has one level of section - a type and
/// the units in it - and a row's detail is not a deeper level of the
/// outline, it is the row's own content, which `enter` owns. Cycling it
/// wholesale would open every unit in the section, each costing a read, to
/// show what nobody asked to see.
#[test]
fn tab_shows_and_hides_the_section_under_the_cursor() {
    let mut app = app_with_units(vec![failed("restic-backup.service"), failed("borgmatic.service")]);
    press(&mut app, "3");

    assert!(units_shown(&app) > 0, "rows start visible");

    app.handle_key(Key::new(KeyCode::Tab));
    assert_eq!(units_shown(&app), 0, "tab folds the section away");

    app.handle_key(Key::new(KeyCode::Tab));
    assert!(units_shown(&app) > 0, "and brings it back");
    assert_eq!(units_open(&app), 0, "without opening anything");
}

/// Folding a section shuts what was open inside it: those rows are about
/// to stop existing, and a detail left open would spring back the next
/// time the section did.
#[test]
fn folding_a_section_shuts_the_rows_that_were_open_in_it() {
    let mut app = app_with_units(vec![failed("restic-backup.service")]);
    press(&mut app, "3");
    step_onto_a_unit(&mut app);
    app.handle_key(Key::new(KeyCode::Enter));
    assert_eq!(units_open(&app), 1);

    // Back onto the header, then fold and unfold.
    app.handle_key(Key::new(KeyCode::Up));
    app.handle_key(Key::new(KeyCode::Tab));
    app.handle_key(Key::new(KeyCode::Tab));
    assert!(units_shown(&app) > 0, "the section is back");
    assert_eq!(units_open(&app), 0, "and nothing sprang open with it");
}

/// magit's `S-TAB`: the *buffer* moves, not the section at point. Every
/// section together, whatever the cursor is on - and never into detail,
/// which would be 366 reads on this host to show what nobody asked for.
#[test]
fn shift_tab_folds_and_unfolds_every_section() {
    let mut app = systemd_app();
    press(&mut app, "3");
    let start_shown = units_shown(&app);
    assert!(start_shown >= 2, "more than one unit, in more than one section");

    app.handle_key(Key::new(KeyCode::BackTab));
    assert_eq!(units_shown(&app), 0, "every section folded");

    app.handle_key(Key::new(KeyCode::BackTab));
    assert_eq!(units_shown(&app), start_shown, "and back to rows");
    assert_eq!(units_open(&app), 0, "never into detail");
}

/// The buffer cycle must not care where the cursor is - that is what
/// makes it the *global* one.
#[test]
fn the_buffer_cycle_ignores_the_cursor() {
    let mut app = systemd_app();
    press(&mut app, "3");
    step_onto_a_unit(&mut app);

    app.handle_key(Key::new(KeyCode::BackTab));
    assert_eq!(units_shown(&app), 0, "every section folded, not just the one at point");
}

fn units_shown(app: &App) -> usize {
    app.view().rows.iter().filter(|r| matches!(r, Node::Unit { .. })).count()
}

fn units_open(app: &App) -> usize {
    app.view().rows.iter().filter(|r| matches!(r, Node::Unit { expanded: true, .. })).count()
}

fn systemd_app() -> App {
    app_with_units(vec![failed("restic-backup.service"), unit("sshd.service", masys_domain::unit::ActiveState::Active)])
}

fn app_with_units(units: Vec<masys_domain::unit::Unit>) -> App {
    let mut snapshot = snapshot();
    snapshot.system_state = masys_domain::sample::SystemState::Degraded;
    let system = FakeSystemService {
        snapshot,
        units,
        calls: Default::default(),
        fails_with: None,
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(10_000, "t".to_string()).expect("tick");
    app
}

/// Steps down until the cursor is on a unit rather than a section header.
fn step_onto_a_unit(app: &mut App) {
    for _ in 0..12 {
        if selected_unit(app).is_some() {
            return;
        }
        app.handle_key(Key::new(KeyCode::Down));
    }
    panic!("never landed on a unit: {:#?}", app.view().rows);
}

fn selected_unit(app: &App) -> Option<String> {
    match app.view().rows.get(app.view().selected?) {
        Some(Node::Unit { unit, .. }) => Some(unit.name.clone()),
        _ => None,
    }
}

fn expanded(app: &App) -> Option<bool> {
    match app.view().rows.get(app.view().selected?) {
        Some(Node::Unit { expanded, .. }) => Some(*expanded),
        _ => None,
    }
}

fn failed(name: &str) -> masys_domain::unit::Unit {
    let mut u = unit(name, masys_domain::unit::ActiveState::Failed);
    u.sub_state = "failed".to_string();
    u
}

/// While typing a filter, letters are text - not the buffer jumps and
/// action keys they would otherwise be.
#[test]
fn typing_a_filter_does_not_trigger_the_keys_it_contains() {
    let (mut app, calls) = app_with(None);
    press(&mut app, "2");
    press(&mut app, "/");
    // `k` would kill, `u` would switch buffers, `s` would too.
    press(&mut app, "kus");

    assert_eq!(app.buffer(), Buffer::Procs, "still in Procs");
    assert!(calls.borrow().is_empty(), "nothing was signalled: {:?}", calls.borrow());
    assert_eq!(app.view().filter, Some("kus"), "the letters are text, not actions");
}

fn unit(name: &str, state: masys_domain::unit::ActiveState) -> masys_domain::unit::Unit {
    masys_domain::unit::Unit {
        name: name.to_string(),
        kind: masys_domain::unit::UnitKind::Service,
        active_state: state,
        sub_state: "running".to_string(),
        exit_code: None,
        enabled: true,
        restart_timestamps_ms: Vec::new(),
        since_ms: 0,
        cgroup: None,
        slice: None,
        triggers: Vec::new(),
        timer: None,
    }
}

/// A platform that answers the ownership question however a test needs.
struct Owned(masys_domain::platform::Ownership);

impl masys_domain::service::PlatformService for Owned {
    fn id(&self) -> masys_domain::platform::PlatformId {
        masys_domain::platform::PlatformId::NixOs
    }
    fn packages(&self) -> Result<Vec<masys_domain::platform::Package>, masys_domain::MasysError> {
        unimplemented!()
    }
    fn updates(&self) -> Result<masys_domain::platform::UpdateStatus, masys_domain::MasysError> {
        unimplemented!()
    }
    fn pending_reboot(&self) -> Result<Option<masys_domain::platform::PendingReboot>, masys_domain::MasysError> {
        Ok(None)
    }
    fn unit_ownership(&self, _unit: &str) -> Result<masys_domain::platform::Ownership, masys_domain::MasysError> {
        Ok(self.0.clone())
    }
    fn boot_pressure(&self) -> Result<Option<masys_domain::platform::BootPressure>, masys_domain::MasysError> {
        Ok(None)
    }
}

fn units_app(ownership: masys_domain::platform::Ownership) -> (App, Calls) {
    let calls: Calls = Default::default();
    let system = FakeSystemService {
        snapshot: snapshot(),
        units: vec![unit("sshd.service", masys_domain::unit::ActiveState::Active)],
        calls: calls.clone(),
        fails_with: None,
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    let mut app = App::new(Box::new(system), Box::new(Owned(ownership)), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(1_000, "t".to_string()).expect("tick");
    press(&mut app, "3");
    // Onto the unit row, past its section header.
    app.handle_key(Key::new(KeyCode::Down));
    (app, calls)
}

/// The guard's whole purpose: on a declaratively managed host, enabling a
/// unit is undone by the next rebuild, and the operator has to be told
/// before pressing the key rather than after.
#[test]
fn enabling_warns_and_says_how_the_change_fails() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Declarative {
        source: "configuration.nix".to_string(),
        note: "refused now - the unit directory is read-only".to_string(),
        reverted_by: "nixos-rebuild switch".to_string(),
    });
    press(&mut app, "ee");
    let Some(ModalView::Confirm { prompt }) = app.view().modal else { panic!("no confirmation") };
    assert!(prompt.contains("enable sshd.service"), "{prompt}");
    // The note says *how* it fails, which is the operator's actual
    // question: refused now, or undone by a later rebuild.
    assert!(prompt.contains("read-only"), "{prompt}");
    assert!(prompt.contains("configuration.nix"), "{prompt}");
    assert!(prompt.contains("nixos-rebuild switch"), "{prompt}");
}

/// On a host where enablement genuinely sticks, the warning would be
/// noise - and noise is what trains an operator to stop reading prompts.
#[test]
fn enabling_says_nothing_extra_when_the_change_persists() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "ee");
    let Some(ModalView::Confirm { prompt }) = app.view().modal else { panic!("no confirmation") };
    assert!(!prompt.contains("reverts"), "{prompt}");
}

/// Starting a unit on a declaratively managed host is perfectly normal
/// and nothing reverts it, so only the two boot-affecting verbs consult
/// the guard.
#[test]
fn starting_does_not_consult_the_ownership_guard() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Declarative {
        source: "configuration.nix".to_string(),
        note: "generated".to_string(),
        reverted_by: "nixos-rebuild switch".to_string(),
    });
    press(&mut app, "S");
    let Some(ModalView::Confirm { prompt }) = app.view().modal else { panic!("no confirmation") };
    assert!(!prompt.contains("reverts"), "{prompt}");
}

#[test]
fn a_confirmed_unit_verb_reaches_the_port() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "ee");
    press(&mut app, "y");
    assert_eq!(*calls.borrow(), vec!["enable sshd.service".to_string()]);
}

#[test]
fn a_declined_unit_verb_does_not() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "D");
    press(&mut app, "n");
    assert!(calls.borrow().is_empty(), "{:?}", calls.borrow());
}

/// A real `journalctl -u` query rather than filtering what the Journal
/// buffer already had. On this host the one failed unit has 320 entries
/// and none fall inside the buffer's one-hour window, so text-filtering
/// showed nothing at all for the exact case the key exists for.
#[test]
fn viewing_a_units_log_queries_that_unit_rather_than_filtering() {
    let (mut app, _) = units_app_with_journal(vec![
        masys_domain::journal::Entry {
            timestamp_ms: 1,
            unit: Some("sshd.service".to_string()),
            priority: masys_domain::journal::Priority::Info,
            message: "Accepted publickey".to_string(),
        },
        masys_domain::journal::Entry {
            timestamp_ms: 2,
            unit: Some("other.service".to_string()),
            priority: masys_domain::journal::Priority::Info,
            // Mentions the unit by name: a text filter would keep this.
            message: "waiting for sshd.service".to_string(),
        },
    ]);
    press(&mut app, "l");
    assert_eq!(app.buffer(), Buffer::Log);

    let messages: Vec<String> = app
        .view()
        .rows
        .iter()
        .filter_map(|r| match r {
            Node::JournalEntry(e) => Some(e.message.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(messages, vec!["Accepted publickey"], "field match, not text match");
    assert!(app.view().filter.is_none(), "no filter is involved at all");
}

/// The log holds the newest 2,000 entries, which on a quiet unit can span
/// weeks - and the time column is `HH:MM:SS`. Without a date somewhere,
/// nothing on screen says which day a line came from.
///
/// A real section rather than a drawn divider: `jump_section` matches any
/// `SectionHeader`, so `n` and `p` move day to day for free.
#[test]
fn the_log_splits_into_a_section_per_local_day() {
    // -06:00, so the local day turns over at 06:00 UTC. These two are two
    // hours apart across that line: one local day each, one UTC day
    // between them.
    let (mut app, _) =
        units_app_with_journal(vec![entry_at(1_779_166_800_000, "late last night"), entry_at(1_779_174_000_000, "early this morning")]);
    press(&mut app, "l");

    let days: Vec<String> = section_titles(&app);
    assert_eq!(days, vec!["2026-05-19".to_string(), "2026-05-18".to_string()], "local days, newest first: {days:?}");
}

/// Grouping on the UTC day would put both of those under 2026-05-19 - the
/// same bug as rendering a journal in UTC, one level up.
#[test]
fn the_day_boundary_is_local_rather_than_utc() {
    let (mut app, _) = units_app_with_journal(vec![entry_at(1_779_166_800_000, "23:00 local, 05:00 UTC")]);
    press(&mut app, "l");
    assert_eq!(section_titles(&app), vec!["2026-05-18".to_string()]);
}

#[test]
fn a_log_inside_one_day_gets_exactly_one_section() {
    let (mut app, _) = units_app_with_journal(vec![entry_at(1_779_174_000_000, "morning"), entry_at(1_779_220_800_000, "afternoon")]);
    press(&mut app, "l");
    assert_eq!(section_titles(&app), vec!["2026-05-19".to_string()]);
}

/// `l` is pressed because something just happened, so the answer should
/// not be two thousand lines down.
#[test]
fn the_log_opens_newest_first() {
    let (mut app, _) = units_app_with_journal(vec![entry_at(1_779_174_000_000, "older"), entry_at(1_779_220_800_000, "newer")]);
    press(&mut app, "l");
    assert_eq!(log_messages(&app), vec!["newer".to_string(), "older".to_string()]);
}

/// `o` reverses *both* levels - the days and the lines within them. A
/// half-reversed view is a bug, not a mode.
#[test]
fn o_reverses_the_days_and_the_lines_within_them() {
    let (mut app, _) = units_app_with_journal(vec![
        entry_at(1_779_166_800_000, "day one"),
        entry_at(1_779_174_000_000, "day two, early"),
        entry_at(1_779_220_800_000, "day two, late"),
    ]);
    press(&mut app, "l");
    assert_eq!(section_titles(&app), vec!["2026-05-19".to_string(), "2026-05-18".to_string()]);
    assert_eq!(log_messages(&app), vec!["day two, late".to_string(), "day two, early".to_string(), "day one".to_string()]);

    press(&mut app, "o");
    assert_eq!(section_titles(&app), vec!["2026-05-18".to_string(), "2026-05-19".to_string()]);
    assert_eq!(log_messages(&app), vec!["day one".to_string(), "day two, early".to_string(), "day two, late".to_string()]);

    press(&mut app, "o");
    assert_eq!(log_messages(&app), vec!["day two, late".to_string(), "day two, early".to_string(), "day one".to_string()], "and back");
}

/// Order changes how the window is shown, never which entries are in it.
/// A sort key that silently refetched would be a different feature.
#[test]
fn reversing_the_order_keeps_the_same_entries() {
    let (mut app, _) =
        units_app_with_journal(vec![entry_at(1_779_166_800_000, "a"), entry_at(1_779_174_000_000, "b"), entry_at(1_779_220_800_000, "c")]);
    press(&mut app, "l");
    let mut before = log_messages(&app);
    press(&mut app, "o");
    let mut after = log_messages(&app);
    before.sort();
    after.sort();
    assert_eq!(before, after);
}

/// Journal entries are often seconds apart, so a reversal nobody announces
/// looks like a key that did nothing - the reason `Header::Procs` carries
/// its direction too.
#[test]
fn the_log_header_names_the_unit_and_the_direction() {
    let (mut app, _) = units_app_with_journal(vec![entry_at(1_779_174_000_000, "anything")]);
    press(&mut app, "l");
    let masys_view::Header::Log { unit, newest_first } = app.view().header else { panic!("not a log header") };
    assert_eq!(unit, "sshd.service");
    assert!(newest_first, "opens newest first");

    press(&mut app, "o");
    let masys_view::Header::Log { newest_first, .. } = app.view().header else { panic!("not a log header") };
    assert!(!newest_first, "and `o` says so");
}

/// The point of flipping is to look at the other end.
#[test]
fn reversing_the_order_puts_the_cursor_at_the_top() {
    let (mut app, _) = units_app_with_journal(vec![entry_at(1_779_174_000_000, "older"), entry_at(1_779_220_800_000, "newer")]);
    press(&mut app, "l");
    app.handle_key(Key::new(KeyCode::End));
    press(&mut app, "o");
    assert_eq!(app.view().selected, Some(0), "back to the top after a flip");
}

/// An empty date heading says nothing. When a filter takes every line of a
/// day, the day goes with them.
#[test]
fn a_day_whose_lines_all_filter_out_loses_its_header() {
    let (mut app, _) = units_app_with_journal(vec![entry_at(1_779_166_800_000, "quiet"), entry_at(1_779_174_000_000, "interesting")]);
    press(&mut app, "l");
    press(&mut app, "/");
    press(&mut app, "interesting");
    assert_eq!(section_titles(&app), vec!["2026-05-19".to_string()], "the emptied day is gone");
}

/// Days are real sections, so the key that folds a section folds a day -
/// no new key, and no new concept to learn.
#[test]
fn tab_folds_a_day() {
    let (mut app, _) = units_app_with_journal(vec![entry_at(1_779_166_800_000, "day one"), entry_at(1_779_174_000_000, "day two")]);
    press(&mut app, "l");
    // Onto the first day's header.
    assert!(matches!(app.view().rows.first(), Some(Node::SectionHeader { .. })), "{:#?}", app.view().rows);
    app.handle_key(Key::new(KeyCode::Tab));

    assert_eq!(log_messages(&app), vec!["day one".to_string()], "the open day's line stays, the folded one's goes");
    assert_eq!(section_titles(&app).len(), 2, "both headers are still there");
}

/// `jump_section` matches any section header, so day-to-day movement came
/// free with the sections. Asserted rather than assumed - it is the reason
/// days are sections at all.
#[test]
fn n_moves_from_one_day_to_the_next() {
    let (mut app, _) = units_app_with_journal(vec![entry_at(1_779_166_800_000, "day one"), entry_at(1_779_174_000_000, "day two")]);
    press(&mut app, "l");
    press(&mut app, "n");
    let row = app.view().selected.and_then(|i| app.view().rows.get(i));
    assert!(matches!(row, Some(Node::SectionHeader { title, .. }) if title == "2026-05-18"), "n landed on the next day: {row:#?}");
}

fn entry_at(timestamp_ms: u64, message: &str) -> masys_domain::journal::Entry {
    masys_domain::journal::Entry {
        timestamp_ms,
        unit: Some("sshd.service".to_string()),
        priority: masys_domain::journal::Priority::Info,
        message: message.to_string(),
    }
}

fn section_titles(app: &App) -> Vec<String> {
    app.view()
        .rows
        .iter()
        .filter_map(|r| match r {
            Node::SectionHeader { title, .. } => Some(title.clone()),
            _ => None,
        })
        .collect()
}

fn log_messages(app: &App) -> Vec<String> {
    app.view()
        .rows
        .iter()
        .filter_map(|r| match r {
            Node::JournalEntry(e) => Some(e.message.clone()),
            _ => None,
        })
        .collect()
}

fn units_app_with_journal(journal: Vec<masys_domain::journal::Entry>) -> (App, Calls) {
    let calls: Calls = Default::default();
    let system = FakeSystemService {
        snapshot: snapshot(),
        units: vec![unit("sshd.service", masys_domain::unit::ActiveState::Active)],
        calls: calls.clone(),
        fails_with: None,
        journal,
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    let mut app = App::new(
        Box::new(system),
        Box::new(Owned(masys_domain::platform::Ownership::Imperative)),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(1_000, "t".to_string()).expect("tick");
    press(&mut app, "3");
    app.handle_key(Key::new(KeyCode::Down));
    (app, calls)
}

/// Backspace was silently doing nothing: `KeyCode` had no variant for it,
/// so the terminal's backspace arrived as `Other` and was ignored - which
/// meant a typo in a filter could only be fixed by starting over.
#[test]
fn backspace_deletes_a_character_from_the_filter() {
    let (mut app, _) = app_with(None);
    press(&mut app, "2");
    press(&mut app, "/");
    press(&mut app, "chrome");

    app.handle_key(Key::new(KeyCode::Backspace));
    assert_eq!(app.view().filter, Some("chrom"));
    for _ in 0..4 {
        app.handle_key(Key::new(KeyCode::Backspace));
    }
    assert_eq!(app.view().filter, Some("c"));
}

/// Backspacing past the start empties the filter rather than underflowing,
/// and leaves it still taking keys - the prompt is open until Esc or
/// Enter says otherwise.
#[test]
fn backspacing_an_empty_filter_is_harmless() {
    let (mut app, _) = app_with(None);
    press(&mut app, "2");
    press(&mut app, "/");
    for _ in 0..5 {
        app.handle_key(Key::new(KeyCode::Backspace));
    }
    assert_eq!(app.view().filter, Some(""));
    assert!(app.view().typing);
}

/// A filter still in effect after Enter is shown without a cursor: it is
/// narrowing the rows, but it is not taking keys.
#[test]
fn a_committed_filter_stays_visible_but_stops_taking_keys() {
    let (mut app, _) = app_with(None);
    press(&mut app, "2");
    press(&mut app, "/");
    press(&mut app, "chrome");
    app.handle_key(Key::new(KeyCode::Enter));

    assert_eq!(app.view().filter, Some("chrome"), "still narrowing");
    assert!(!app.view().typing, "but no longer taking keys");
    // And normal keys work again.
    press(&mut app, "3");
    assert_eq!(app.buffer(), masys_app::Buffer::Systemd);
}

fn procs_app() -> App {
    let (mut app, _) = app_with(None);
    press(&mut app, "2");
    app
}

fn sort_marker(app: &App) -> (String, bool) {
    match app.view().header {
        masys_view::Header::Procs { sort, descending } => (sort.label().to_string(), descending),
        _ => panic!("not the Procs header"),
    }
}

/// htop and top both work this way, and so does every file manager's
/// column header: the same key again reverses.
#[test]
fn pressing_a_sort_key_twice_reverses_it() {
    let mut app = procs_app();
    assert_eq!(sort_marker(&app), ("cpu".to_string(), true), "cpu starts worst-first");

    press(&mut app, "c");
    assert_eq!(sort_marker(&app), ("cpu".to_string(), false), "the same key again reverses");
    press(&mut app, "c");
    assert_eq!(sort_marker(&app), ("cpu".to_string(), true), "and again reverses back");
}

/// A different column takes its own natural direction rather than
/// inheriting whichever way the last one happened to be pointing - so `n`
/// always starts at a-z.
#[test]
fn switching_columns_resets_to_that_columns_natural_direction() {
    let mut app = procs_app();
    press(&mut app, "c");
    assert_eq!(sort_marker(&app), ("cpu".to_string(), false), "cpu is now ascending");

    press(&mut app, "a");
    assert_eq!(sort_marker(&app), ("name".to_string(), false), "name starts a-z");
    press(&mut app, "m");
    assert_eq!(sort_marker(&app), ("memory".to_string(), true), "memory starts biggest-first");
}

/// The direction has to actually move the rows, not just the marker.
#[test]
fn reversing_actually_reorders_the_buffer() {
    let mut app = procs_app();
    press(&mut app, "a");
    let ascending: Vec<String> = comms(&app);
    press(&mut app, "a");
    let descending: Vec<String> = comms(&app);

    assert!(!ascending.is_empty(), "there are rows to order");
    assert_eq!(descending, ascending.iter().rev().cloned().collect::<Vec<_>>(), "reversed");
}

fn comms(app: &App) -> Vec<String> {
    app.view()
        .rows
        .iter()
        .filter_map(|r| match r {
            Node::Proc { proc, .. } => Some(proc.comm.clone()),
            _ => None,
        })
        .collect()
}

fn action_dimming(app: &App) -> Vec<(String, bool)> {
    app.view().actions.iter().map(|b| (b.chord.clone(), b.dimmed)).collect()
}

/// The design's rule: masys never hides an action outright, only marks
/// it. A key that vanishes when it does not apply punishes muscle memory
/// and leaves the operator wondering whether they misremembered it.
#[test]
fn enable_and_disable_dim_where_the_manifest_owns_the_unit() {
    let (app, _) = units_app(masys_domain::platform::Ownership::Declarative {
        source: "configuration.nix".to_string(),
        note: "refused now - the unit directory is read-only".to_string(),
        reverted_by: "nixos-rebuild switch".to_string(),
    });
    let dimming = action_dimming(&app);

    for (chord, dimmed) in &dimming {
        // `E` joins disable: `systemctl edit` writes a drop-in under
        // `/etc/systemd/system`, which on this host resolves into a
        // read-only store. `M` and `U` write a symlink to the same place.
        // `F` dims for its own reason - the unit is healthy, so there is
        // no failure to reset.
        //
        // `e` is not in the set: it opens the transient now, and the
        // guard marks `enable` on the popup's own row. A key that dimmed
        // because the popup it opens contains a dim row would be hiding
        // the explanation the mark exists to give.
        let expected = matches!(chord.as_str(), "D" | "E" | "F" | "M" | "U");
        assert_eq!(*dimmed, expected, "{chord} dimmed={dimmed}, expected {expected}: {dimming:?}");
    }
    // Still present, still in position - dimmed is not hidden.
    assert!(dimming.iter().any(|(c, _)| c == "D"), "{dimming:?}");
}

/// The guard did not go away when `enable` moved into the transient - it
/// moved with it. This is the assertion the footer can no longer make,
/// and the one the design's mockup actually draws.
#[test]
fn the_ownership_guard_marks_enable_inside_the_transient() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Declarative {
        source: "configuration.nix".to_string(),
        note: "refused now - the unit directory is read-only".to_string(),
        reverted_by: "nixos-rebuild switch".to_string(),
    });
    press(&mut app, "e");
    let Some(ModalView::Transient { title, groups, .. }) = app.view().modal else { panic!("no transient") };

    assert!(title.contains("sshd.service"), "the popup names its unit: {title}");
    let persistence = groups.iter().find(|g| g.heading == "Persistence").expect("a Persistence group");
    assert_eq!(persistence.note.as_deref(), Some("! refused now - the unit directory is read-only"));
    for row in &persistence.rows {
        assert!(row.dimmed, "{} is marked: {persistence:?}", row.label);
        assert_eq!(row.note.as_deref(), Some("reverts on nixos-rebuild switch"), "{}", row.label);
    }
    assert_eq!(persistence.rows.iter().map(|r| r.label).collect::<Vec<_>>(), vec!["enable", "disable", "mask"]);

    // And the runtime verbs are untouched, which is the whole point of a
    // guard that asks about persistence rather than about writing.
    let runtime = groups.iter().find(|g| g.heading == "Runtime").expect("a Runtime group");
    assert!(runtime.rows.iter().all(|row| !row.dimmed), "{runtime:?}");
    assert_eq!(runtime.note, None);
}

/// On a host where enablement sticks, nothing in the popup is marked.
#[test]
fn an_imperative_host_marks_nothing_in_the_transient() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "e");
    let Some(ModalView::Transient { groups, .. }) = app.view().modal else { panic!("no transient") };

    let persistence = groups.iter().find(|g| g.heading == "Persistence").expect("a Persistence group");
    assert_eq!(persistence.note, None);
    assert!(persistence.rows.iter().all(|row| !row.dimmed && row.note.is_none()), "{persistence:?}");
}

/// Starting a unit writes nothing and contradicts no manifest, so it is
/// never dimmed - only the two verbs that change what happens at boot.
#[test]
fn the_runtime_verbs_are_never_dimmed() {
    let (app, _) = units_app(masys_domain::platform::Ownership::Declarative {
        source: "configuration.nix".to_string(),
        note: "refused".to_string(),
        reverted_by: "nixos-rebuild switch".to_string(),
    });
    // `alt+r` and `R` join them: try-restart and reload change what is
    // running now and write nothing, so no manifest contradicts them.
    for chord in ["r", "S", "X", "l", "/", "alt+r", "R"] {
        let dimmed = action_dimming(&app).into_iter().find(|(c, _)| c == chord).map(|(_, d)| d);
        assert_eq!(dimmed, Some(false), "{chord} must stay lit");
    }
}

/// On a host where enabling genuinely sticks, dimming would be a lie.
#[test]
fn nothing_dims_where_the_change_would_persist() {
    let (app, _) = units_app(masys_domain::platform::Ownership::Imperative);
    // `F` still dims, for an unrelated reason - this unit is healthy -
    // so the ownership guard is asserted on the two verbs it governs.
    for chord in ["e", "D"] {
        let dimmed = action_dimming(&app).into_iter().find(|(c, _)| c == chord).map(|(_, d)| d);
        assert_eq!(dimmed, Some(false), "{chord} must stay lit where enabling persists");
    }
}

/// The judgement is per unit, not per host: a hand-placed unit on the
/// same machine is genuinely imperative. So it has to follow the cursor,
/// which means recomputing on movement and not only on a tick.
#[test]
fn dimming_follows_the_cursor_rather_than_the_host() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Imperative);
    // On a healthy unit: only the verb that needs a failure is marked.
    let on_unit: Vec<String> = action_dimming(&app).into_iter().filter(|(_, d)| *d).map(|(c, _)| c).collect();
    assert_eq!(on_unit, vec!["F".to_string()]);

    // Onto a row that is not a unit at all - the section header above it.
    app.handle_key(Key::new(KeyCode::Up));
    assert!(
        matches!(app.view().rows.get(app.view().selected.unwrap_or(0)), Some(Node::SectionHeader { .. })),
        "the cursor moved off the unit"
    );
    // Every unit key is marked here, because not one of them will do
    // anything: `ask_unit` and `show_logs` both open by asking for the
    // row's unit and returning when there is not one. The point is that
    // the answer was recomputed on the keypress rather than frozen from
    // the last tick.
    //
    // This asserted the opposite until the Nix view got operations of its
    // own. Nothing was marked on a heading, so the footer offered eleven
    // unit verbs against a row that had no unit - which is the same
    // footer-disagrees-with-the-key failure that has twice been fixed
    // here, sitting in the mechanism built to prevent it.
    let lit: Vec<String> = action_dimming(&app).into_iter().filter(|(_, dimmed)| !dimmed).map(|(chord, _)| chord).collect();
    assert_eq!(lit, vec!["/".to_string()], "only the filter applies to a section header");
}

/// The footer and the help must not disagree about whether a key applies.
#[test]
fn the_help_dims_the_same_keys_as_the_footer() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Declarative {
        source: "configuration.nix".to_string(),
        note: "refused".to_string(),
        reverted_by: "nixos-rebuild switch".to_string(),
    });
    press(&mut app, "?");
    let Some(ModalView::Keys { groups }) = app.view().modal else { panic!("no help") };
    let dimmed: Vec<String> = groups.iter().flat_map(|g| g.bindings.iter()).filter(|b| b.dimmed).map(|b| b.chord.clone()).collect();
    // `E` because editing writes where the manifest owns; `F` because the
    // unit is healthy and there is nothing to reset. `e` opens the
    // transient and is never dimmed on a unit row.
    assert_eq!(dimmed, vec!["D".to_string(), "F".to_string(), "E".to_string(), "M".to_string(), "U".to_string()]);
}

/// A tick two seconds after `l` used to re-fetch the whole system's
/// journal and silently throw the scope away, so the unit's log was
/// visible only until the next refresh.
#[test]
fn a_tick_does_not_unscope_a_units_log() {
    let entries = vec![
        masys_domain::journal::Entry {
            timestamp_ms: 1,
            unit: Some("sshd.service".to_string()),
            priority: masys_domain::journal::Priority::Info,
            message: "Accepted publickey".to_string(),
        },
        masys_domain::journal::Entry {
            timestamp_ms: 2,
            unit: Some("other.service".to_string()),
            priority: masys_domain::journal::Priority::Info,
            message: "unrelated".to_string(),
        },
    ];
    let (mut app, _) = units_app_with_journal(entries);
    press(&mut app, "l");
    let scoped = journal_messages(&app);
    assert_eq!(scoped, vec!["Accepted publickey"]);

    app.tick(3_000, "t".to_string()).expect("tick");
    assert_eq!(journal_messages(&app), scoped, "still just this unit's log");
}

fn journal_messages(app: &App) -> Vec<String> {
    app.view()
        .rows
        .iter()
        .filter_map(|r| match r {
            Node::JournalEntry(e) => Some(e.message.clone()),
            _ => None,
        })
        .collect()
}

/// A unit's detail is its own row, and one the cursor cannot land on.
///
/// Two reasons, and the first is visible: the selection highlight covers
/// a whole list item, so a unit and its detail sharing one item lit the
/// entire block. The second is that the cursor belongs on the *unit* -
/// every verb in this view acts on the row under it, and a cursor parked
/// on a property line would have nothing to act on.
#[test]
fn an_open_units_detail_is_a_row_the_cursor_skips() {
    let mut app = systemd_app();
    press(&mut app, "3");
    step_onto_a_unit(&mut app);
    let name = selected_unit(&app).expect("a unit");
    app.handle_key(Key::new(KeyCode::Enter));

    let selected = app.view().selected.expect("a cursor");
    assert!(matches!(app.view().rows.get(selected), Some(Node::Unit { .. })), "the cursor stayed on the unit");
    assert!(matches!(app.view().rows.get(selected + 1), Some(Node::UnitDetail { .. })), "the detail is the next row");

    // Stepping past it lands on the following unit, never on the detail.
    app.handle_key(Key::new(KeyCode::Down));
    let after = app.view().selected.expect("a cursor");
    assert!(
        !matches!(app.view().rows.get(after), Some(Node::UnitDetail { .. })),
        "the cursor stopped on a detail row: {:#?}",
        app.view().rows.get(after)
    );
    let _ = name;
}

/// The log left the fold. `l` opens it in full, scrollable and
/// searchable; duplicating a handful of its lines inside the row meant
/// two places to read the same thing and neither of them the good one.
#[test]
fn an_open_units_detail_carries_no_log() {
    let mut app = systemd_app();
    press(&mut app, "3");
    step_onto_a_unit(&mut app);
    app.handle_key(Key::new(KeyCode::Enter));

    assert!(
        !app.view().rows.iter().any(|r| matches!(r, Node::JournalEntry(_))),
        "no journal rows in the systemd view: {:#?}",
        app.view().rows
    );
}

/// `l` is a drill-down, so there is a way back - and it lands on the unit
/// you drilled from, not merely in the view you left.
///
/// `esc` rather than `q`, which quits from everywhere. The systemd view
/// remembers its own cursor anyway, but that alone is not enough: the
/// rows are rebuilt every tick and a unit can move between them, so back
/// means "to that unit" and not "to that row number".
#[test]
fn esc_returns_from_the_log_to_the_unit_it_was_opened_from() {
    let entries = vec![masys_domain::journal::Entry {
        timestamp_ms: 1,
        unit: Some("sshd.service".to_string()),
        priority: masys_domain::journal::Priority::Info,
        message: "listening".to_string(),
    }];
    let (mut app, _) = units_app_with_journal(entries);
    let from = selected_unit(&app).expect("the cursor starts on a unit");

    press(&mut app, "l");
    assert_eq!(app.buffer(), Buffer::Log);

    app.handle_key(Key::new(KeyCode::Esc));
    assert_eq!(app.buffer(), Buffer::Systemd, "esc goes back");
    assert_eq!(selected_unit(&app).as_deref(), Some(from.as_str()), "and onto the unit it came from");
}

/// Esc with nowhere to go back to does nothing, rather than jumping
/// somewhere arbitrary.
#[test]
fn esc_outside_a_drill_down_is_inert() {
    let mut app = systemd_app();
    press(&mut app, "3");
    app.handle_key(Key::new(KeyCode::Esc));
    assert_eq!(app.buffer(), Buffer::Systemd);
}

/// A detail row survives a filter that keeps the unit it belongs to.
///
/// It has no text of its own to match - it is the row above it, spelled
/// out - so filtering it independently dropped it, and opening a unit
/// inside a filtered view appeared to do nothing at all.
#[test]
fn a_filter_that_keeps_a_unit_keeps_its_open_detail() {
    let mut app = systemd_app();
    press(&mut app, "3");
    press(&mut app, "/");
    press(&mut app, "restic");
    app.handle_key(Key::new(KeyCode::Enter));

    step_onto_a_unit(&mut app);
    app.handle_key(Key::new(KeyCode::Enter));

    assert!(
        app.view().rows.iter().any(|r| matches!(r, Node::UnitDetail { .. })),
        "the detail is gone from a filtered view: {:#?}",
        app.view().rows
    );
}

/// And a filter that drops the unit drops its detail with it, rather than
/// leaving properties under a heading with no row to belong to.
#[test]
fn a_filter_that_drops_a_unit_drops_its_detail() {
    let mut app = systemd_app();
    press(&mut app, "3");
    step_onto_a_unit(&mut app);
    app.handle_key(Key::new(KeyCode::Enter));

    press(&mut app, "/");
    press(&mut app, "nosuchunit");
    assert!(!app.view().rows.iter().any(|r| matches!(r, Node::UnitDetail { .. })), "an orphaned detail survived: {:#?}", app.view().rows);
}

/// Search reaches units inside folded sections.
///
/// The filter runs over the rows, and the rows are built after folding -
/// so a collapsed section contributed nothing to search, and `/` could
/// not find a unit you had just folded out of sight. Searching is how you
/// find a unit you cannot see; a fold is exactly the state in which you
/// cannot see it.
#[test]
fn search_finds_units_inside_folded_sections() {
    let mut app = systemd_app();
    press(&mut app, "3");

    // Fold everything away.
    app.handle_key(Key::new(KeyCode::BackTab));
    assert_eq!(units_shown(&app), 0, "nothing is visible");

    press(&mut app, "/");
    press(&mut app, "restic");
    assert_eq!(units_shown(&app), 1, "the match surfaces anyway: {:#?}", app.view().rows);
}

/// And clearing the filter puts the folds back rather than leaving the
/// buffer permanently unfolded by a search.
#[test]
fn clearing_a_search_restores_the_folds() {
    let mut app = systemd_app();
    press(&mut app, "3");
    app.handle_key(Key::new(KeyCode::BackTab));

    press(&mut app, "/");
    press(&mut app, "restic");
    assert_eq!(units_shown(&app), 1);

    app.handle_key(Key::new(KeyCode::Esc));
    assert_eq!(units_shown(&app), 0, "folded again");
}

/// `E` edits a unit - `systemctl edit`, which spawns $EDITOR.
///
/// Unlike every other verb here it needs the terminal masys is holding,
/// so it cannot simply be run: the session asks to be suspended, and the
/// composition root - which owns the terminal - hands it over and takes
/// it back.
#[test]
fn editing_asks_first_and_then_asks_for_the_terminal() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "E");

    let Some(ModalView::Confirm { prompt }) = app.view().modal else { panic!("no confirmation") };
    assert!(prompt.contains("edit sshd.service"), "{prompt}");
    assert!(calls.borrow().is_empty(), "nothing has run yet");

    assert_eq!(app.handle_key(Key::char('y')), Flow::Suspend, "the session asks for the terminal back");
    assert!(calls.borrow().is_empty(), "and still nothing has run - the caller runs it");

    // What the binary does once it has restored the terminal.
    app.run_suspended();
    assert_eq!(calls.borrow().as_slice(), ["edit sshd.service"]);
}

/// An editor is the one suspended thing that needs no pause afterwards.
///
/// `$EDITOR` holds the terminal until the operator quits it, so its screen
/// has already been read by the person who was reading it, and stopping to
/// ask for a keypress would be asking them to dismiss a screen they just
/// left. Every other suspended command prints and exits, which is the
/// opposite shape - `Aftermath` is how the composition root tells them
/// apart without knowing what either one runs.
#[test]
fn an_editor_leaves_nothing_unread_because_the_operator_was_reading_it() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "E");
    app.handle_key(Key::char('y'));

    assert_eq!(app.run_suspended(), Aftermath::Seen);
    assert_eq!(calls.borrow().as_slice(), ["edit sshd.service"], "the editor really ran");
}

/// A refused edit is read like any other output, because there was no
/// editor.
///
/// `systemctl edit` refuses outright where the unit directory is
/// read-only, and what reaches the terminal then is one line of
/// systemctl's own text - printed and exited, in exactly the position
/// `nix store diff-closures` output is in. The reason an editor needs no
/// pause is that it held the screen, and an editor that never opened held
/// nothing.
#[test]
fn an_edit_that_was_refused_leaves_its_refusal_unread() {
    let calls: Calls = Default::default();
    let system = FakeSystemService {
        snapshot: snapshot(),
        units: vec![unit("sshd.service", masys_domain::unit::ActiveState::Active)],
        calls: calls.clone(),
        fails_with: Some("Failed to edit sshd.service: Read-only file system".to_string()),
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    let mut app = App::new(
        Box::new(system),
        Box::new(Owned(masys_domain::platform::Ownership::Imperative)),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(1_000, "t".to_string()).expect("tick");
    press(&mut app, "3");
    app.handle_key(Key::new(KeyCode::Down));
    press(&mut app, "E");
    app.handle_key(Key::char('y'));

    assert_eq!(app.run_suspended(), Aftermath::Unseen);
    assert_eq!(calls.borrow().as_slice(), ["edit sshd.service"], "it was attempted");
    let StatusLine::Error(message) = app.view().status else { panic!("the refusal was not held on the status line") };
    assert!(message.contains("Read-only"), "{message}");
}

/// Nothing queued leaves nothing to read - so a composition root that
/// calls this unconditionally never pauses on an empty screen.
#[test]
fn a_suspension_that_was_never_asked_for_leaves_nothing_unread() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    assert_eq!(app.run_suspended(), Aftermath::Seen);
    assert!(calls.borrow().is_empty(), "{:?}", calls.borrow());
}

/// Declining leaves the terminal alone. An editor that opens because you
/// pressed the wrong key is a worse surprise than most.
#[test]
fn declining_an_edit_does_not_suspend() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "E");
    assert_eq!(app.handle_key(Key::char('n')), Flow::Continue);
    app.run_suspended();
    assert!(calls.borrow().is_empty(), "{:?}", calls.borrow());
}

/// Editing changes what happens at boot, so it consults the same guard
/// enable and disable do. On a host where the unit directory resolves
/// into a read-only store, `systemctl edit` cannot write its drop-in at
/// all - and finding that out after the editor has opened is the failure
/// the guard exists to prevent.
#[test]
fn editing_warns_where_the_manifest_owns_the_unit() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Declarative {
        source: "configuration.nix".to_string(),
        note: "refused now - the unit directory is read-only".to_string(),
        reverted_by: "nixos-rebuild switch".to_string(),
    });
    press(&mut app, "E");
    let Some(ModalView::Confirm { prompt }) = app.view().modal else { panic!("no confirmation") };
    assert!(prompt.contains("read-only"), "{prompt}");
    assert!(prompt.contains("nixos-rebuild switch"), "{prompt}");
}

/// And it is dimmed there, like the other two verbs the manifest owns.
#[test]
fn edit_dims_where_the_manifest_owns_the_unit() {
    let (app, _) = units_app(masys_domain::platform::Ownership::Declarative {
        source: "configuration.nix".to_string(),
        note: "generated".to_string(),
        reverted_by: "nixos-rebuild switch".to_string(),
    });
    let edit = app.view().actions.iter().find(|b| b.chord == "E").expect("an edit key");
    assert!(edit.dimmed, "edit is not marked as owned by the manifest");
}

/// `l` on a timer opens the log of the unit it *runs*, not the timer's.
///
/// A timer logs almost nothing of its own - `journalctl -u foo.timer` on
/// this host returns 23 lines, every one of them pid 1 saying "Started
/// foo.timer". The output anyone pressing `l` on a timer wants is the
/// job's, and the timer already knows which unit that is. It is the same
/// reason the Timers section exists at all: a timer's own state answers
/// the wrong question.
#[test]
fn l_on_a_timer_opens_the_log_of_what_it_runs() {
    let mut timer = unit("nightly-backup.timer", masys_domain::unit::ActiveState::Active);
    timer.kind = masys_domain::unit::UnitKind::Timer;
    timer.timer =
        Some(masys_domain::unit::Timer { next_ms: Some(9_000), last_ms: Some(1_000), activates: "nightly-backup.service".to_string() });
    let entries = vec![masys_domain::journal::Entry {
        timestamp_ms: 1,
        unit: Some("nightly-backup.service".to_string()),
        priority: masys_domain::journal::Priority::Info,
        message: "backup finished".to_string(),
    }];
    let (mut app, _) = app_with_units_and_journal(vec![timer], entries);
    press(&mut app, "3");
    for _ in 0..6 {
        if matches!(app.view().rows.get(app.view().selected.unwrap_or(0)), Some(Node::Timer { .. })) {
            break;
        }
        app.handle_key(Key::new(KeyCode::Down));
    }
    press(&mut app, "l");
    assert_eq!(journal_messages(&app), vec!["backup finished"], "the job's log, not the timer's");
}

/// A failed fetch must not leave the previous unit's log on screen under
/// the previous unit's name.
///
/// The old code assigned entries and the scope only on success, so an
/// error kept both - and then switched to the log view anyway. The result
/// was a log that belonged to whatever unit was opened last, titled after
/// that unit, with an error line under it that most people would not read
/// before believing the rows.
#[test]
fn a_failed_log_fetch_does_not_show_another_units_log() {
    let good = vec![masys_domain::journal::Entry {
        timestamp_ms: 1,
        unit: Some("first.service".to_string()),
        priority: masys_domain::journal::Priority::Info,
        message: "from the first unit".to_string(),
    }];
    let (mut app, _) = app_with_units_and_journal(
        vec![
            unit("first.service", masys_domain::unit::ActiveState::Active),
            unit("second.service", masys_domain::unit::ActiveState::Active),
        ],
        good,
    );
    press(&mut app, "3");
    step_onto_a_unit(&mut app);
    press(&mut app, "l");
    assert_eq!(journal_messages(&app), vec!["from the first unit"]);

    // Now make every read fail, and open the *other* unit's log.
    press(&mut app, "3");
    app.handle_key(Key::new(KeyCode::Down));
    let failing = FakeSystemService {
        snapshot: snapshot(),
        units: Vec::new(),
        calls: Default::default(),
        fails_with: Some("journalctl: exit 1".to_string()),
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    app.replace_system(Box::new(failing));
    press(&mut app, "l");

    assert!(journal_messages(&app).is_empty(), "the other unit's lines are still on screen: {:?}", journal_messages(&app));
}

fn app_with_units_and_journal(units: Vec<masys_domain::unit::Unit>, journal: Vec<masys_domain::journal::Entry>) -> (App, Calls) {
    let calls: Calls = Default::default();
    let system = FakeSystemService {
        snapshot: snapshot(),
        units,
        calls: calls.clone(),
        fails_with: None,
        journal,
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(10_000, "t".to_string()).expect("tick");
    (app, calls)
}

/// A matching process keeps the group that owns it.
///
/// A cgroup row's text is its own name, so searching for a pid - or for a
/// kernel thread, or any process whose command differs from its unit -
/// dropped the group and left the process floating with nothing to say
/// which unit it belonged to. On this host `/897` returned one process row
/// and no group at all, which is the one fact a process row cannot supply
/// about itself.
#[test]
fn a_matching_process_keeps_its_group() {
    let mut app = grouped_procs_app();
    press(&mut app, "/");
    press(&mut app, "1234");

    let rows = app.view().rows;
    assert!(rows.iter().any(|r| matches!(r, Node::Proc { proc, .. } if proc.pid == 1234)), "the match survives: {rows:#?}");
    assert!(matches!(rows.first(), Some(Node::ProcGroup { .. })), "and its group is above it: {rows:#?}");
}

/// A group whose name matches stays even when nothing under it does -
/// searching for a unit is a way of asking what that unit is running.
#[test]
fn a_matching_group_survives_without_matching_children() {
    let mut app = grouped_procs_app();
    press(&mut app, "/");
    press(&mut app, "user.slice");
    assert!(app.view().rows.iter().any(|r| matches!(r, Node::ProcGroup { .. })), "the group is gone: {:#?}", app.view().rows);
}

/// And a group with neither is dropped, so the buffer does not fill with
/// empty headings - the rule every section already follows.
#[test]
fn a_group_matching_nothing_is_dropped_entirely() {
    let mut app = grouped_procs_app();
    press(&mut app, "/");
    press(&mut app, "nosuchthing");
    assert!(app.view().rows.is_empty(), "{:#?}", app.view().rows);
}

fn grouped_procs_app() -> App {
    let mut snapshot = snapshot();
    snapshot.procs = vec![
        with_group(proc(1234, "chrome", 0), "/user.slice/app.slice"),
        with_group(proc(5678, "firefox", 0), "/user.slice/app.slice"),
        with_group(proc(9, "sshd", 0), "/system.slice/sshd.service"),
    ];
    let system = FakeSystemService {
        snapshot,
        units: Vec::new(),
        calls: Default::default(),
        fails_with: None,
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    let platform = FakePlatformService { boot_pressure: None, pending_reboot: None };
    let mut app = App::new(Box::new(system), Box::new(platform), Box::new(fake::NoScanner), "devbox".to_string());
    app.tick(1_000, "t".to_string()).expect("tick");
    press(&mut app, "2");
    app
}

fn with_group(mut p: masys_domain::sample::Proc, cgroup: &str) -> masys_domain::sample::Proc {
    p.cgroup = Some(cgroup.to_string());
    p
}

/// `esc` clears a filter that is in effect.
///
/// Committing one with Enter used to leave no way out of it but `/`,
/// backspace held down, Enter - and a filter you cannot lift is a filter
/// that quietly hides rows for the rest of the session. Escape is the key
/// every editor uses to back out of a state; this is one.
#[test]
fn esc_clears_a_committed_filter() {
    let mut app = systemd_app();
    press(&mut app, "3");
    press(&mut app, "/");
    press(&mut app, "restic");
    app.handle_key(Key::new(KeyCode::Enter));
    assert_eq!(app.view().filter, Some("restic"));

    app.handle_key(Key::new(KeyCode::Esc));
    assert_eq!(app.view().filter, None, "the filter is lifted");
    assert!(!app.view().typing);
}

/// Escape peels one layer at a time. In a drill-down with a filter on it,
/// the first clears the filter and the second comes back - rather than
/// leaving the view behind with a filter still set on it.
#[test]
fn esc_peels_the_filter_before_the_drill_down() {
    let entries = vec![masys_domain::journal::Entry {
        timestamp_ms: 1,
        unit: Some("sshd.service".to_string()),
        priority: masys_domain::journal::Priority::Info,
        message: "listening".to_string(),
    }];
    let (mut app, _) = units_app_with_journal(entries);
    press(&mut app, "l");
    press(&mut app, "/");
    press(&mut app, "listening");
    app.handle_key(Key::new(KeyCode::Enter));

    app.handle_key(Key::new(KeyCode::Esc));
    assert_eq!(app.buffer(), Buffer::Log, "still in the log");
    assert_eq!(app.view().filter, None, "but the filter is gone");

    app.handle_key(Key::new(KeyCode::Esc));
    assert_eq!(app.buffer(), Buffer::Systemd, "and now it comes back");
}

/// `l` arrives at an unfiltered log, whatever the last one was narrowed
/// to.
///
/// Filters are per view, which is right for the four fixed views - but
/// the log view's *identity* changes every time `l` names a new unit, so
/// a filter that made sense for one unit's log carried onto the next
/// one's and silently hid most of it. Leaving one view narrowed and
/// coming back to it is a state you chose; arriving somewhere new already
/// narrowed is not.
#[test]
fn l_arrives_at_an_unfiltered_log() {
    let entries = vec![
        masys_domain::journal::Entry {
            timestamp_ms: 1,
            unit: Some("first.service".to_string()),
            priority: masys_domain::journal::Priority::Info,
            message: "started cleanly".to_string(),
        },
        masys_domain::journal::Entry {
            timestamp_ms: 2,
            unit: Some("second.service".to_string()),
            priority: masys_domain::journal::Priority::Info,
            message: "quite different".to_string(),
        },
    ];
    let (mut app, _) = app_with_units_and_journal(
        vec![
            unit("first.service", masys_domain::unit::ActiveState::Active),
            unit("second.service", masys_domain::unit::ActiveState::Active),
        ],
        entries,
    );
    press(&mut app, "3");
    step_onto_a_unit(&mut app);
    press(&mut app, "l");
    press(&mut app, "/");
    press(&mut app, "started");
    app.handle_key(Key::new(KeyCode::Enter));
    assert_eq!(app.view().filter, Some("started"));

    // Leave without lifting it, and open a different unit's log.
    press(&mut app, "3");
    app.handle_key(Key::new(KeyCode::Down));
    step_onto_a_unit(&mut app);
    press(&mut app, "l");

    assert_eq!(app.view().filter, None, "the previous unit's filter came along");
    assert_eq!(journal_messages(&app), vec!["quite different"], "so every line is there");
}

/// A filter belongs to the view it was typed in.
///
/// The exact path this broke on: narrow the systemd view to one unit,
/// press `l`, press `/` and type. The old filter was one string shared by
/// every view, so the new text appended to `sshd.service` and the search
/// matched nothing - while the log view had silently been narrowed by a
/// unit name the whole time.
#[test]
fn each_view_keeps_its_own_filter() {
    let entries = vec![masys_domain::journal::Entry {
        timestamp_ms: 1,
        unit: Some("sshd.service".to_string()),
        priority: masys_domain::journal::Priority::Info,
        message: "Accepted publickey for user".to_string(),
    }];
    let (mut app, _) = units_app_with_journal(entries);

    press(&mut app, "/");
    press(&mut app, "sshd");
    app.handle_key(Key::new(KeyCode::Enter));
    assert_eq!(app.view().filter, Some("sshd"), "the systemd view is narrowed");

    press(&mut app, "l");
    assert_eq!(app.view().filter, None, "the log view arrives unfiltered");
    assert_eq!(journal_messages(&app), vec!["Accepted publickey for user"], "so every line is there");

    press(&mut app, "/");
    press(&mut app, "publickey");
    assert_eq!(app.view().filter, Some("publickey"), "and typing starts from empty");
    assert_eq!(journal_messages(&app).len(), 1, "the search matches");

    // Commit it first: while a filter is being typed, a digit is text.
    app.handle_key(Key::new(KeyCode::Enter));
    press(&mut app, "3");
    assert_eq!(app.view().filter, Some("sshd"), "and the systemd view still has its own");
}

/// `l` is the only way into the log view, and `esc` the way out.
///
/// No digit reaches it. Every other digit lands on a fixed view; this one
/// would land on "whichever unit you opened last", which is a destination
/// that depends on what you did rather than on what you pressed. It is a
/// drill-down, and drill-downs are entered from the thing they are about.
#[test]
fn no_digit_reaches_the_log_view() {
    let entries = vec![masys_domain::journal::Entry {
        timestamp_ms: 1,
        unit: Some("sshd.service".to_string()),
        priority: masys_domain::journal::Priority::Info,
        message: "listening".to_string(),
    }];
    let (mut app, _) = units_app_with_journal(entries);
    for digit in ["1", "2", "3", "4", "5"] {
        press(&mut app, digit);
        assert_ne!(app.buffer(), Buffer::Log, "`{digit}` reached the log view");
    }

    press(&mut app, "3");
    press(&mut app, "l");
    assert_eq!(app.buffer(), Buffer::Log, "and `l` still gets there");
    assert_eq!(journal_messages(&app), vec!["listening"]);

    app.handle_key(Key::new(KeyCode::Esc));
    assert_eq!(app.buffer(), Buffer::Systemd, "esc is the way back");
}

/// The unit's own last words, not systemd's report that it died:
/// journald tags a unit's output with that unit, while pid 1's messages
/// about it come from init.scope.
#[test]
fn a_failed_units_reason_is_its_own_output_not_systemds() {
    let calls: Calls = Default::default();
    let mut failed = unit("broken.service", masys_domain::unit::ActiveState::Failed);
    failed.exit_code = Some(6);
    let system = FakeSystemService {
        snapshot: snapshot(),
        units: vec![failed],
        calls,
        fails_with: None,
        appended: Default::default(),
        journal: vec![
            masys_domain::journal::Entry {
                timestamp_ms: 1,
                unit: Some("broken.service".to_string()),
                priority: masys_domain::journal::Priority::Error,
                message: "curl: (6) Could not resolve host".to_string(),
            },
            masys_domain::journal::Entry {
                timestamp_ms: 2,
                unit: Some("init.scope".to_string()),
                priority: masys_domain::journal::Priority::Error,
                message: "broken.service: Failed with result 'exit-code'.".to_string(),
            },
        ],
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    let mut app = App::new(
        Box::new(system),
        Box::new(Owned(masys_domain::platform::Ownership::Imperative)),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(1_000, "t".to_string()).expect("tick");

    let reason = app
        .view()
        .rows
        .iter()
        .find_map(|r| match r {
            Node::Finding(masys_domain::finding::Finding::FailedUnit { reason, .. }) => Some(reason.clone()),
            _ => None,
        })
        .expect("a failed-unit finding");
    assert_eq!(reason, Some("curl: (6) Could not resolve host".to_string()), "systemd's own 'Failed with result' is not the reason");
}

/// systemd keeps a unit failed until something resets it, which is what
/// makes a host report `degraded` hours after a transient fault. masys
/// could report that condition and not act on it.
#[test]
fn reset_failed_reaches_the_port() {
    let (mut app, calls) = failed_units_app();
    press(&mut app, "F");
    press(&mut app, "y");
    assert_eq!(*calls.borrow(), vec!["reset-failed broken.service".to_string()]);
}

/// It only makes sense on a unit that has actually failed - so on a
/// healthy one the key is marked rather than removed, the same rule the
/// ownership guard follows.
#[test]
fn reset_failed_dims_on_a_unit_that_has_not_failed() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Imperative);
    // units_app's single unit is Active.
    let dimmed = app.view().actions.iter().find(|b| b.chord == "F").map(|b| b.dimmed);
    assert_eq!(dimmed, Some(true), "nothing to reset on a healthy unit");

    // And lit where there is something to clear.
    let (app2, _) = failed_units_app();
    let _ = &mut app;
    let dimmed = app2.view().actions.iter().find(|b| b.chord == "F").map(|b| b.dimmed);
    assert_eq!(dimmed, Some(false), "the failed unit can be reset");
}

/// The lifecycle verbs are unaffected: a healthy unit can still be
/// restarted, and only the verb that needs a failure is marked.
#[test]
fn only_reset_failed_dims_for_a_healthy_unit() {
    let (app, _) = units_app(masys_domain::platform::Ownership::Imperative);
    for chord in ["r", "S", "X", "l"] {
        let dimmed = app.view().actions.iter().find(|b| b.chord == chord).map(|b| b.dimmed);
        assert_eq!(dimmed, Some(false), "{chord} must stay lit on a healthy unit");
    }
}

fn failed_units_app() -> (App, Calls) {
    let calls: Calls = Default::default();
    let mut broken = unit("broken.service", masys_domain::unit::ActiveState::Failed);
    broken.exit_code = Some(1);
    let system = FakeSystemService {
        snapshot: snapshot(),
        units: vec![broken],
        calls: calls.clone(),
        fails_with: None,
        journal: Vec::new(),
        appended: Default::default(),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    let mut app = App::new(
        Box::new(system),
        Box::new(Owned(masys_domain::platform::Ownership::Imperative)),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(1_000, "t".to_string()).expect("tick");
    press(&mut app, "3");
    app.handle_key(Key::new(KeyCode::Down));
    (app, calls)
}

/// Masking is the verb that answers "stop this from ever starting", and
/// it was the one gap in the operations table: the design lists it, the
/// keymap never had it, and the port could not express it.
#[test]
fn masking_reaches_the_port() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "M");
    press(&mut app, "y");
    assert_eq!(*calls.borrow(), vec!["mask sshd.service".to_string()]);
}

#[test]
fn unmasking_reaches_the_port() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "U");
    press(&mut app, "y");
    assert_eq!(*calls.borrow(), vec!["unmask sshd.service".to_string()]);
}

/// A mask is a symlink under `/etc/systemd/system`, which is exactly what
/// enable and disable write - so it consults the same guard, and the
/// operator learns before pressing the key that a rebuild will undo it.
#[test]
fn masking_warns_where_the_manifest_owns_the_unit() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Declarative {
        source: "configuration.nix".to_string(),
        note: "refused now - the unit directory is read-only".to_string(),
        reverted_by: "nixos-rebuild switch".to_string(),
    });
    press(&mut app, "M");
    let Some(ModalView::Confirm { prompt }) = app.view().modal else { panic!("no confirmation") };
    assert!(prompt.contains("mask sshd.service"), "{prompt}");
    assert!(prompt.contains("read-only"), "{prompt}");
    assert!(prompt.contains("configuration.nix"), "{prompt}");
}

/// `try-restart` restarts a unit only if it is already running, which is
/// the safe form for a config reload sweep. Nothing is written, so the
/// ownership guard has nothing to say about it.
#[test]
fn try_restart_reaches_the_port() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    app.handle_key(Key::alt(KeyCode::Char('r')));
    press(&mut app, "y");
    assert_eq!(*calls.borrow(), vec!["try-restart sshd.service".to_string()]);
}

/// Reload has been on the port since the domain crate landed and was
/// never bound to anything, so the one verb that re-reads a unit's
/// configuration without dropping its connections was unreachable.
#[test]
fn reloading_reaches_the_port() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "R");
    press(&mut app, "y");
    assert_eq!(*calls.borrow(), vec!["reload sshd.service".to_string()]);
}

/// The loop wakes four times a second while a log is open, and needs to
/// know when that is - "which view is open" is the app's to answer, the
/// same reason `wants_refresh` is asked rather than assumed.
#[test]
fn the_session_follows_only_while_a_log_is_open() {
    let (mut app, _) = units_app_with_journal(vec![entry_at(1_779_174_000_000, "before")]);
    assert!(!app.following(), "the systemd view is not a log");

    press(&mut app, "l");
    assert!(app.following(), "and this one is");

    app.handle_key(Key::new(KeyCode::Esc));
    assert!(!app.following(), "back out, and the fast poll stops with it");
}

/// A line appended after the view opened appears without waiting for the
/// tick - the whole point of following.
#[test]
fn a_line_appended_while_watching_appears() {
    let (mut app, appended) = units_app_following(vec![entry_at(1_779_174_000_000, "before")]);
    press(&mut app, "l");
    assert_eq!(log_messages(&app), vec!["before".to_string()]);

    // Nothing has arrived: the answer four times out of four.
    assert!(!app.follow_log(), "nothing new, nothing redrawn");
    assert_eq!(log_messages(&app), vec!["before".to_string()]);

    *appended.borrow_mut() = Some(vec![entry_at(1_779_174_000_000, "before"), entry_at(1_786_950_060_000, "after")]);
    assert!(app.follow_log(), "something arrived");
    assert_eq!(log_messages(&app), vec!["after".to_string(), "before".to_string()], "newest first, as the view reads");
}

/// The same fixture, with the fake's "arrived since you last looked" slot
/// handed back so a test can write into it.
///
/// Through the fake rather than through a method on `App`: the session
/// holds a `Box<dyn SystemService>`, and giving the real type a way to
/// inject entries would put test machinery in production code.
type Appended = std::rc::Rc<std::cell::RefCell<Option<Vec<masys_domain::journal::Entry>>>>;

fn units_app_following(journal: Vec<masys_domain::journal::Entry>) -> (App, Appended) {
    let appended: Appended = Default::default();
    let system = FakeSystemService {
        snapshot: snapshot(),
        units: vec![unit("sshd.service", masys_domain::unit::ActiveState::Active)],
        calls: Default::default(),
        fails_with: None,
        journal,
        appended: std::rc::Rc::clone(&appended),
        journal_queries: Default::default(),
        queued: Default::default(),
        proc_details: Default::default(),
        detail_queries: Default::default(),
    };
    let mut app = App::new(
        Box::new(system),
        Box::new(Owned(masys_domain::platform::Ownership::Imperative)),
        Box::new(fake::NoScanner),
        "devbox".to_string(),
    );
    app.tick(1_000, "t".to_string()).expect("tick");
    press(&mut app, "3");
    app.handle_key(Key::new(KeyCode::Down));
    (app, appended)
}

/// Escape closes a transient, and closes only the transient. Checked
/// before any chord so no definition can bind its way out of being
/// closeable - the same rule the buffer escape ladder follows.
#[test]
fn escape_closes_the_transient_and_leaves_the_buffer_alone() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    let before = app.view().selected;

    press(&mut app, "e");
    assert!(matches!(app.view().modal, Some(ModalView::Transient { .. })), "it opened");

    app.handle_key(Key::new(KeyCode::Esc));
    assert!(app.view().modal.is_none(), "and closed");
    assert_eq!(app.view().selected, before, "the cursor did not move");
    assert!(calls.borrow().is_empty(), "and nothing ran");
}

/// A transient takes every key. Without that, a chord it does not bind
/// falls through to the buffer underneath and the popup becomes a way to
/// run something you cannot see.
#[test]
fn a_transient_swallows_keys_the_buffer_would_have_taken() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "e");

    // `3` opens the systemd buffer and `q` quits, from anywhere - except
    // under a popup.
    press(&mut app, "2");
    assert!(matches!(app.view().modal, Some(ModalView::Transient { .. })), "still open");
    assert_eq!(app.handle_key(Key::char('q')), Flow::Continue, "and `q` did not quit");
    assert!(matches!(app.view().modal, Some(ModalView::Transient { .. })), "still open");
}

/// Dispatching closes the transient before the action runs, so the
/// confirmation a row raises is answered against the rows it names
/// rather than through a popup still covering them.
#[test]
fn dispatching_a_row_closes_the_transient_before_the_action_runs() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "e");
    press(&mut app, "s");

    let Some(ModalView::Confirm { prompt }) = app.view().modal else { panic!("the transient gave way to a confirmation") };
    assert!(prompt.contains("start sshd.service"), "{prompt}");

    press(&mut app, "y");
    assert_eq!(*calls.borrow(), vec!["start sshd.service".to_string()]);
    assert!(app.view().modal.is_none(), "and nothing is left open");
}

/// A marked row runs. The ownership guard says the effect will not last;
/// it does not say the key is dead, and a popup that refused to dispatch
/// its dim rows would make the mark mean something the design never said.
#[test]
fn a_marked_row_still_runs() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Declarative {
        source: "configuration.nix".to_string(),
        note: "generated from configuration".to_string(),
        reverted_by: "nixos-rebuild switch".to_string(),
    });
    press(&mut app, "e");
    let Some(ModalView::Transient { groups, .. }) = app.view().modal else { panic!("no transient") };
    assert!(groups.iter().flat_map(|g| &g.rows).find(|r| r.chord == 'e').is_some_and(|r| r.dimmed), "enable is marked on this host");

    press(&mut app, "e");
    press(&mut app, "y");
    assert_eq!(*calls.borrow(), vec!["enable sshd.service".to_string()], "and ran anyway");
}

/// A chord the definition does not bind does nothing and leaves the popup
/// open, rather than closing it on a typo.
#[test]
fn an_unbound_chord_leaves_the_transient_open() {
    let (mut app, calls) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "e");
    press(&mut app, "z");

    assert!(matches!(app.view().modal, Some(ModalView::Transient { .. })), "still open");
    assert!(calls.borrow().is_empty(), "and nothing ran");
}

/// The transient opens on a unit row and nowhere else yet. A row with no
/// transient opens nothing rather than an empty popup - the same choice
/// `crate::buffer` makes about a digit with no buffer behind it.
#[test]
fn a_row_with_no_transient_opens_nothing() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Imperative);
    // Back onto the section header the cursor started above.
    app.handle_key(Key::new(KeyCode::Up));
    press(&mut app, "e");

    assert!(app.view().modal.is_none(), "no popup, and no empty one either");
}

/// The popup lists the design's groups in the design's order: what runs
/// now, what survives a reboot, what to look at.
#[test]
fn the_unit_popup_lists_the_designs_groups_in_order() {
    let (mut app, _) = units_app(masys_domain::platform::Ownership::Imperative);
    press(&mut app, "e");
    let Some(ModalView::Transient { groups, .. }) = app.view().modal else { panic!("no transient") };

    assert_eq!(groups.iter().map(|g| g.heading.as_str()).collect::<Vec<_>>(), vec!["Runtime", "Persistence", "Inspect"]);
    assert_eq!(groups[0].rows.iter().map(|r| r.label).collect::<Vec<_>>(), vec!["start", "stop", "restart", "reload"]);
}
