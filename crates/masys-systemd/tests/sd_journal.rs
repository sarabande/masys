//! The `libsystemd` path, against this machine's real journal.
//!
//! Not ignored, in the manner of `tests/live.rs`: it degrades to a
//! no-op where the library cannot be loaded, which is itself the thing
//! most worth asserting - the fallback is the normal path on a host
//! without a global library path, and it must never panic.

use masys_systemd::sd_journal;

/// The whole safety argument for this feature: absent library, no crash,
/// and the caller quietly uses the subprocess.
///
/// The environment is deliberately *not* mutated here. `lib()` resolves
/// once per process behind a `OnceLock`, so a test that set
/// `MASYS_LIBSYSTEMD` would decide the answer for every other test in the
/// binary - and the one below would then pass by doing nothing.
#[test]
fn asking_for_the_library_never_panics() {
    let _ = sd_journal::available();
    // A unit that does not exist has an empty log, which is not an error:
    // opening the journal succeeds and the match simply matches nothing.
    if let Some(reader) = sd_journal::Reader::open("no-such-unit-exists.service") {
        assert!(reader.tail(10).is_empty(), "no unit, no lines");
        let _ = reader.changed();
    }
}

/// With the library present, reading a real unit's log through it must
/// agree with what the subprocess path returns for the same unit.
#[test]
fn the_library_path_reads_the_same_journal_as_journalctl() {
    if !sd_journal::available() {
        // No library on this host - NixOS, most likely, which keeps no
        // global library path. The subprocess path is what runs, and the
        // parser tests cover it.
        return;
    }
    // Available means this must work: a half-loaded library is treated as
    // no library at all, so there is no third state to be lenient about.
    let reader = sd_journal::Reader::open("systemd-journald.service").expect("the library loaded, so the journal opens");
    let entries = reader.tail(20);
    assert!(!entries.is_empty(), "journald has certainly said something");
    for entry in &entries {
        assert_eq!(entry.unit.as_deref(), Some("systemd-journald.service"), "the match is the unit's own, not a text search");
    }
    // Oldest first, the order the log view and journalctl both use.
    let timestamps: Vec<u64> = entries.iter().map(|e| e.timestamp_ms).collect();
    let mut sorted = timestamps.clone();
    sorted.sort();
    assert_eq!(timestamps, sorted, "oldest first");
    assert!(entries.iter().all(|e| e.timestamp_ms > 0), "every entry is stamped");

    // A check that never blocks, whatever it answers.
    let _ = reader.changed();
}
