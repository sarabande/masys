use masys_systemd::restarts::RestartHistory;

const WINDOW: u64 = 3_600_000;

/// Restarts that predate masys cannot be dated, so the first sighting
/// records none however high the count. The documented cost is that
/// flapping cannot fire until masys has run a full window.
#[test]
fn the_first_sighting_of_a_unit_records_no_restarts() {
    let mut history = RestartHistory::new();
    assert_eq!(
        history.observe("a.service", Some(47), 1_000, WINDOW),
        Vec::<u64>::new()
    );
}

#[test]
fn a_rising_count_appends_one_timestamp_per_restart() {
    let mut history = RestartHistory::new();
    history.observe("a.service", Some(0), 1_000, WINDOW);
    assert_eq!(
        history.observe("a.service", Some(1), 2_000, WINDOW),
        vec![2_000]
    );
    assert_eq!(
        history.observe("a.service", Some(2), 3_000, WINDOW),
        vec![2_000, 3_000]
    );
}

/// Three restarts between two polls are three restarts, not one - the
/// flapping rule counts them, so collapsing them would hide a storm that
/// happens faster than the poll interval.
#[test]
fn several_restarts_between_two_polls_all_count() {
    let mut history = RestartHistory::new();
    history.observe("a.service", Some(0), 1_000, WINDOW);
    assert_eq!(
        history.observe("a.service", Some(3), 2_000, WINDOW),
        vec![2_000, 2_000, 2_000]
    );
}

#[test]
fn an_unchanged_count_adds_nothing() {
    let mut history = RestartHistory::new();
    history.observe("a.service", Some(0), 1_000, WINDOW);
    history.observe("a.service", Some(1), 2_000, WINDOW);
    assert_eq!(
        history.observe("a.service", Some(1), 3_000, WINDOW),
        vec![2_000]
    );
}

#[test]
fn timestamps_older_than_the_window_are_pruned() {
    let mut history = RestartHistory::new();
    history.observe("a.service", Some(0), 0, WINDOW);
    history.observe("a.service", Some(1), 1_000, WINDOW);
    let stamps = history.observe("a.service", Some(1), 1_000 + WINDOW + 1, WINDOW);
    assert!(
        stamps.is_empty(),
        "the one restart has aged out: {stamps:?}"
    );
}

/// systemd resets NRestarts when a unit is reloaded or the daemon
/// restarts. Treating the drop as negative restarts would underflow.
#[test]
fn a_falling_count_rebaselines_instead_of_underflowing() {
    let mut history = RestartHistory::new();
    history.observe("a.service", Some(5), 1_000, WINDOW);
    assert_eq!(
        history.observe("a.service", Some(0), 2_000, WINDOW),
        Vec::<u64>::new()
    );
    assert_eq!(
        history.observe("a.service", Some(1), 3_000, WINDOW),
        vec![3_000],
        "counting resumes from the new baseline"
    );
}

/// A host that churns transient units would otherwise grow this map
/// forever.
#[test]
fn units_that_disappear_are_forgotten() {
    let mut history = RestartHistory::new();
    history.observe("gone.service", Some(0), 1_000, WINDOW);
    history.observe("gone.service", Some(1), 2_000, WINDOW);
    history.retain_units(&std::collections::HashSet::from(["kept.service"]));
    assert_eq!(
        history.observe("gone.service", Some(9), 3_000, WINDOW),
        Vec::<u64>::new(),
        "it is a first sighting again"
    );
}

/// Seeded history is what lets flapping fire for a unit that was already
/// restarting before masys started - otherwise the tool is blind to the
/// fault it exists to catch for the first hour after you launch it to
/// investigate that fault.
#[test]
fn seeded_history_survives_the_first_sighting() {
    let mut history = RestartHistory::new();
    history.seed("a.service", vec![1_000, 2_000, 3_000]);

    // The first observation is still a baseline - a count of 47 must not
    // manufacture 47 timestamps - but the journal's real ones stand.
    assert_eq!(
        history.observe("a.service", Some(47), 4_000, WINDOW),
        vec![1_000, 2_000, 3_000]
    );
    // And counting resumes from that baseline.
    assert_eq!(
        history.observe("a.service", Some(48), 5_000, WINDOW),
        vec![1_000, 2_000, 3_000, 5_000]
    );
}

#[test]
fn seeded_timestamps_outside_the_window_are_dropped() {
    let mut history = RestartHistory::new();
    history.seed("a.service", vec![1_000, WINDOW + 2_000]);
    assert_eq!(
        history.observe("a.service", Some(0), WINDOW + 2_000, WINDOW),
        vec![WINDOW + 2_000]
    );
}

#[test]
fn seeded_timestamps_are_ordered() {
    let mut history = RestartHistory::new();
    history.seed("a.service", vec![3_000, 1_000, 2_000]);
    assert_eq!(
        history.observe("a.service", Some(0), 4_000, WINDOW),
        vec![1_000, 2_000, 3_000]
    );
}

/// A host with a volatile or empty journal simply gets the old warm-up
/// behaviour rather than an error.
#[test]
fn seeding_nothing_leaves_the_accumulator_untouched() {
    let mut history = RestartHistory::new();
    history.seed("a.service", Vec::new());
    assert_eq!(
        history.observe("a.service", Some(5), 1_000, WINDOW),
        Vec::<u64>::new()
    );
}

/// **A read that failed is not a counter that reset.**
///
/// This is the shape that fabricated findings. `NRestarts` was read with
/// `unwrap_or(0)`, so one failed D-Bus `Get` looked like the counter
/// falling to nothing; `observe` re-baselined on the fall; and the next
/// good poll replayed the unit's entire lifetime count as restarts dated
/// to that moment. A service up for months with three lifetime restarts
/// was then flapping, on the default `flapping_restart_count: 3`.
///
/// The history has to survive the gap untouched: no re-baseline, no new
/// stamps, and the next real reading measured against the count from
/// before the failure rather than against zero.
#[test]
fn a_failed_read_does_not_replay_the_counter_as_restarts() {
    let mut history = RestartHistory::new();
    // Baseline: a long-stable service with three restarts in its past,
    // none of them dated (first sighting records none).
    history.observe("a.service", Some(3), 1_000, WINDOW);

    // The poll that could not read the property.
    assert_eq!(
        history.observe("a.service", None, 2_000, WINDOW),
        Vec::<u64>::new(),
        "a failed read contributes no restarts"
    );

    // The next good poll reads the same three. Nothing restarted, so
    // nothing is dated to now.
    assert_eq!(
        history.observe("a.service", Some(3), 3_000, WINDOW),
        Vec::<u64>::new(),
        "the count is unchanged, so the gap invented no restarts"
    );
}

/// A genuine restart across the gap still counts, exactly once.
///
/// The fix must not buy its correctness by going deaf: the reading after
/// a failed one is still measured against the last count masys actually
/// saw.
#[test]
fn a_restart_across_a_failed_read_is_still_counted_once() {
    let mut history = RestartHistory::new();
    history.observe("a.service", Some(3), 1_000, WINDOW);
    history.observe("a.service", None, 2_000, WINDOW);
    assert_eq!(
        history.observe("a.service", Some(4), 3_000, WINDOW),
        vec![3_000],
        "one restart happened, so one timestamp - not four"
    );
}

/// A unit whose very first sighting fails records no baseline.
///
/// Taking one from a failed read would make the first good poll look
/// like a rise from zero, which is the same fabrication one step earlier.
#[test]
fn a_unit_first_seen_through_a_failed_read_takes_no_baseline() {
    let mut history = RestartHistory::new();
    assert_eq!(
        history.observe("a.service", None, 1_000, WINDOW),
        Vec::<u64>::new()
    );
    assert_eq!(
        history.observe("a.service", Some(9), 2_000, WINDOW),
        Vec::<u64>::new(),
        "the first real reading is a baseline, not nine restarts at once"
    );
}
