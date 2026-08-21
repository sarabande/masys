use masys_systemd::restarts::RestartHistory;

const WINDOW: u64 = 3_600_000;

/// Restarts that predate masys cannot be dated, so the first sighting
/// records none however high the count. The documented cost is that
/// flapping cannot fire until masys has run a full window.
#[test]
fn the_first_sighting_of_a_unit_records_no_restarts() {
    let mut history = RestartHistory::new();
    assert_eq!(history.observe("a.service", 47, 1_000, WINDOW), Vec::<u64>::new());
}

#[test]
fn a_rising_count_appends_one_timestamp_per_restart() {
    let mut history = RestartHistory::new();
    history.observe("a.service", 0, 1_000, WINDOW);
    assert_eq!(history.observe("a.service", 1, 2_000, WINDOW), vec![2_000]);
    assert_eq!(history.observe("a.service", 2, 3_000, WINDOW), vec![2_000, 3_000]);
}

/// Three restarts between two polls are three restarts, not one - the
/// flapping rule counts them, so collapsing them would hide a storm that
/// happens faster than the poll interval.
#[test]
fn several_restarts_between_two_polls_all_count() {
    let mut history = RestartHistory::new();
    history.observe("a.service", 0, 1_000, WINDOW);
    assert_eq!(history.observe("a.service", 3, 2_000, WINDOW), vec![2_000, 2_000, 2_000]);
}

#[test]
fn an_unchanged_count_adds_nothing() {
    let mut history = RestartHistory::new();
    history.observe("a.service", 0, 1_000, WINDOW);
    history.observe("a.service", 1, 2_000, WINDOW);
    assert_eq!(history.observe("a.service", 1, 3_000, WINDOW), vec![2_000]);
}

#[test]
fn timestamps_older_than_the_window_are_pruned() {
    let mut history = RestartHistory::new();
    history.observe("a.service", 0, 0, WINDOW);
    history.observe("a.service", 1, 1_000, WINDOW);
    let stamps = history.observe("a.service", 1, 1_000 + WINDOW + 1, WINDOW);
    assert!(stamps.is_empty(), "the one restart has aged out: {stamps:?}");
}

/// systemd resets NRestarts when a unit is reloaded or the daemon
/// restarts. Treating the drop as negative restarts would underflow.
#[test]
fn a_falling_count_rebaselines_instead_of_underflowing() {
    let mut history = RestartHistory::new();
    history.observe("a.service", 5, 1_000, WINDOW);
    assert_eq!(history.observe("a.service", 0, 2_000, WINDOW), Vec::<u64>::new());
    assert_eq!(history.observe("a.service", 1, 3_000, WINDOW), vec![3_000], "counting resumes from the new baseline");
}

/// A host that churns transient units would otherwise grow this map
/// forever.
#[test]
fn units_that_disappear_are_forgotten() {
    let mut history = RestartHistory::new();
    history.observe("gone.service", 0, 1_000, WINDOW);
    history.observe("gone.service", 1, 2_000, WINDOW);
    history.retain_units(&std::collections::HashSet::from(["kept.service"]));
    assert_eq!(history.observe("gone.service", 9, 3_000, WINDOW), Vec::<u64>::new(), "it is a first sighting again");
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
    assert_eq!(history.observe("a.service", 47, 4_000, WINDOW), vec![1_000, 2_000, 3_000]);
    // And counting resumes from that baseline.
    assert_eq!(history.observe("a.service", 48, 5_000, WINDOW), vec![1_000, 2_000, 3_000, 5_000]);
}

#[test]
fn seeded_timestamps_outside_the_window_are_dropped() {
    let mut history = RestartHistory::new();
    history.seed("a.service", vec![1_000, WINDOW + 2_000]);
    assert_eq!(history.observe("a.service", 0, WINDOW + 2_000, WINDOW), vec![WINDOW + 2_000]);
}

#[test]
fn seeded_timestamps_are_ordered() {
    let mut history = RestartHistory::new();
    history.seed("a.service", vec![3_000, 1_000, 2_000]);
    assert_eq!(history.observe("a.service", 0, 4_000, WINDOW), vec![1_000, 2_000, 3_000]);
}

/// A host with a volatile or empty journal simply gets the old warm-up
/// behaviour rather than an error.
#[test]
fn seeding_nothing_leaves_the_accumulator_untouched() {
    let mut history = RestartHistory::new();
    history.seed("a.service", Vec::new());
    assert_eq!(history.observe("a.service", 5, 1_000, WINDOW), Vec::<u64>::new());
}
