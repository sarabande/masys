use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use std::collections::BTreeMap;

use masys_systemd::thermal::throttled_ms_by_core_under;

static UNIQUE: AtomicU32 = AtomicU32::new(0);

/// A fake `/sys/devices/system/cpu` under the system temp directory,
/// unique per test and removed on drop - the same pid-plus-counter pair
/// `masys-platform-nixos/tests/profiles.rs` uses, and for the same
/// reason: a fixed name collides across concurrent runs or two users on
/// one machine.
///
/// Not created by `new`, so a test can hand over a path that does not
/// exist at all - which is the shape of every host without the interface.
struct CpuRoot(PathBuf);

impl CpuRoot {
    fn new(label: &str) -> CpuRoot {
        CpuRoot(std::env::temp_dir().join(format!(
            "masys-thermal-{label}-{}-{}",
            std::process::id(),
            UNIQUE.fetch_add(1, Ordering::Relaxed)
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// One core directory holding whatever `counter` says, verbatim -
    /// text rather than a number so a test can write what a kernel
    /// would, trailing newline included, or something masys must refuse.
    fn core(&self, cpu: u32, counter: &str) {
        let dir = self.0.join(format!("cpu{cpu}")).join("thermal_throttle");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("core_throttle_total_time_ms"), counter).unwrap();
    }

    /// A directory beside the cores holding a counter-shaped file -
    /// `cpufreq` and friends, which must not be read as cores.
    fn named_dir(&self, name: &str, counter: &str) {
        let dir = self.0.join(name).join("thermal_throttle");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("core_throttle_total_time_ms"), counter).unwrap();
    }

    /// A core with no throttle accounting at all, which is what every
    /// core looks like on a kernel built without it.
    fn bare_core(&self, cpu: u32) {
        fs::create_dir_all(self.0.join(format!("cpu{cpu}"))).unwrap();
    }
}

impl Drop for CpuRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Every core that answers, keyed by its own number - so the caller can
/// difference each core against itself. Reporting one figure for the
/// machine is what let a stale counter on one core mask another core
/// throttling now; see `rate::derive_thermal`.
#[test]
fn every_core_that_answers_is_keyed_by_its_own_number() {
    let root = CpuRoot::new("bycore");
    root.core(0, "100\n");
    root.core(1, "250\n");
    root.core(2, "40\n");

    let reading = throttled_ms_by_core_under(root.path()).expect("three cores answered");
    assert_eq!(
        reading,
        BTreeMap::from([(0, 100), (1, 250), (2, 40)]),
        "each core's own figure, not one number for the machine"
    );
}

/// Cores are keyed by the number in their directory name, so a host whose
/// cores are not numbered from zero is read correctly rather than by
/// position.
#[test]
fn cores_are_keyed_by_name_not_by_position() {
    let root = CpuRoot::new("sparse");
    root.core(3, "10\n");
    root.core(11, "20\n");

    let reading = throttled_ms_by_core_under(root.path()).expect("two cores answered");
    assert_eq!(reading, BTreeMap::from([(3, 10), (11, 20)]));
}

/// The siblings of the cores in that directory - `cpufreq`, `cpuidle`,
/// `power` - are not cores, and they are kept out by failing to parse
/// rather than by a list of names that would need maintaining.
#[test]
fn siblings_of_the_cores_are_not_read_as_cores() {
    let root = CpuRoot::new("siblings");
    root.core(0, "70\n");
    root.named_dir("cpufreq", "999\n");
    root.named_dir("cpuidle", "999\n");

    let reading = throttled_ms_by_core_under(root.path()).expect("one real core");
    assert_eq!(reading, BTreeMap::from([(0, 70)]));
}

/// The rule the whole reading exists to keep. A host with no
/// `thermal_throttle` has not been measured, and an empty map would say
/// masys looked and found a machine with no cores.
#[test]
fn a_host_without_the_interface_reads_as_no_answer() {
    let absent = CpuRoot::new("absent");
    assert_eq!(
        throttled_ms_by_core_under(absent.path()),
        None,
        "a directory that does not exist"
    );

    let bare = CpuRoot::new("bare");
    bare.bare_core(0);
    bare.bare_core(1);
    assert_eq!(
        throttled_ms_by_core_under(bare.path()),
        None,
        "cores present, none of them accounting for throttling"
    );
}

/// A counter masys cannot parse is one core it did not read, not one core
/// that reported zero - and it is left out of the map entirely, so the
/// caller cannot difference against a number nobody wrote.
#[test]
fn a_counter_that_does_not_parse_is_left_out_not_recorded_as_zero() {
    let root = CpuRoot::new("garbage");
    root.core(0, "not a number\n");
    root.core(1, "");
    root.core(2, "70\n");

    let reading = throttled_ms_by_core_under(root.path()).expect("one core answered");
    assert_eq!(reading, BTreeMap::from([(2, 70)]));
}

/// Every core unreadable is the same answer as no interface at all.
#[test]
fn every_core_unreadable_is_no_answer() {
    let root = CpuRoot::new("allbad");
    root.core(0, "wat");
    root.core(1, "-");

    assert_eq!(throttled_ms_by_core_under(root.path()), None);
}

/// The real path, on whatever host runs the suite.
///
/// The tests above all point at a temp directory, so none of them touches
/// the one string that has to be right in production. This exercises it
/// and asserts only what is true everywhere: an answer or a declining,
/// never a panic. `sd_journal`'s availability test is the same shape and
/// exists for the same reason - a hardcoded path is a claim about the
/// world that no fixture can check.
#[test]
fn the_real_cpu_root_answers_or_declines_without_panicking() {
    let _ = masys_systemd::thermal::throttled_ms_by_core();
}
