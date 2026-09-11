//! A disk's own answer to "do you expect to fail", via `smartctl`.
//!
//! The one read in this crate that shells out to something that is not
//! systemd and may not be installed. Everything here is built so that not
//! knowing is easy to say: the two decisions worth testing - which devices
//! to ask, and what an answer means - are pure functions, and the
//! subprocess sits behind them doing nothing else.

use std::process::Command;

/// Whether a `/sys/block` entry is a disk with a self-assessment to give.
///
/// Everything excluded here is a device that either has no firmware to ask
/// (`loop`, `ram`, `zram`, `md`, `dm-` are kernel constructs) or is not a
/// disk (`sr` optical, `fd` floppy, `nbd` network block). Asking them
/// costs a process each and answers nothing, on a host that may have
/// dozens: this one carries eight unused `loop` devices.
pub fn asks_smart(device: &str) -> bool {
    const NOT_DISKS: [&str; 8] = ["loop", "ram", "zram", "md", "dm-", "sr", "fd", "nbd"];
    !device.is_empty() && !NOT_DISKS.iter().any(|prefix| device.starts_with(prefix))
}

/// What one `smartctl -H` run said, or `None` if it did not say.
///
/// **Parsed from the text rather than from the exit status**, which
/// sounds backwards and is not. `smartctl`'s exit code is a bitfield
/// where bit 3 means "disk failing" - but bit 1 means "device open
/// failed" and bit 2 means "some command failed", and a device in standby
/// or behind an unsupported controller sets those while saying nothing at
/// all about health. Reading the sentence the tool prints keeps *failing*
/// and *could not ask* apart, which is the whole distinction this port
/// exists to preserve.
///
/// Two sentences, because there are two device families: ATA prints
/// "SMART overall-health self-assessment test result: PASSED", SCSI and
/// SAS print "SMART Health Status: OK".
pub fn health_from_output(text: &str) -> Option<bool> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(verdict) =
            line.strip_prefix("SMART overall-health self-assessment test result:")
        {
            return match verdict.trim() {
                "PASSED" => Some(true),
                // `FAILED!` on ATA, and the exclamation mark is the
                // tool's, not this comment's.
                v if v.starts_with("FAILED") => Some(false),
                _ => None,
            };
        }
        // Anything that is not `OK` is a failure *here*, where the ATA
        // arm above answers `None` to the same shape. Not an
        // inconsistency: ATA prints one of two tokens, so a third is
        // wording nobody recognises and no verdict. SCSI puts the failure
        // itself in the text - "FAILURE PREDICTION THRESHOLD EXCEEDED",
        // "HARDWARE IMPENDING FAILURE" - so on that family an
        // unrecognised string is the report, not the absence of one.
        if let Some(verdict) = line.strip_prefix("SMART Health Status:") {
            return match verdict.trim() {
                "OK" => Some(true),
                "" => None,
                _ => Some(false),
            };
        }
    }
    None
}

/// Every block device worth asking, from `/sys/block`.
///
/// `None` where the directory could not be read at all, which is not the
/// same as a host with no disks and must not become `Some(true)` by way
/// of an empty list.
fn disks() -> Option<Vec<String>> {
    let entries = std::fs::read_dir("/sys/block").ok()?;
    let mut names = Vec::new();
    for entry in entries {
        // Not `filter_map(Result::ok)`. A directory entry that cannot be
        // read is a disk masys does not know about, and dropping it
        // quietly would let the remaining disks answer `Some(true)` -
        // "every disk is healthy" - with one of them never asked. That is
        // the claim `every_one_answered` exists to withhold, arrived at
        // by a different route.
        let entry = entry.ok()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if asks_smart(&name) {
            names.push(name);
        }
    }
    Some(names)
}

/// How to ask one device, which is not the same question for all of them.
///
/// `-n standby` is what lets this run every few minutes without being
/// rude: a disk that has spun down answers "standby" and is left alone
/// rather than woken to be asked how it feels. A woken disk costs more
/// than the reading is worth, and on a laptop it is audible.
///
/// **Not for NVMe, and this is the part worth stating.** `-n` is
/// implemented for ATA devices only; handed an NVMe device, `smartctl`
/// rejects it and prints no verdict, which this module would read as
/// "cannot tell" - correctly, and forever. A host whose only disk is
/// NVMe would have had the feature silently never fire, which on current
/// hardware is most of them. There is nothing to protect anyway: an NVMe
/// drive has no platters to spin up, so the option guards a cost it
/// cannot incur.
pub fn args_for(device: &str) -> Vec<String> {
    let mut args = vec!["-H".to_string()];
    if !device.starts_with("nvme") {
        args.push("-n".to_string());
        args.push("standby".to_string());
    }
    args.push(format!("/dev/{device}"));
    args
}

/// One device's answer.
fn health_of(device: &str) -> Option<bool> {
    let output = Command::new("smartctl")
        .args(args_for(device))
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    health_from_output(&String::from_utf8_lossy(&output.stdout))
}

/// Whether every disk that answered says it is passing.
///
/// The three answers, and which host gets each:
///
/// - `Some(false)` as soon as one disk says it is failing. One failing
///   disk is the finding; the others do not soften it.
/// - `Some(true)` only where *every* disk answered and every answer was
///   passing. A host with two healthy disks and a third that could not be
///   read gets `None`, because "all healthy" is a claim about all of them
///   and the unread one is the one that matters.
/// - `None` everywhere else, which is most hosts: no `smartctl`, no root,
///   a virtual disk, a disk in standby, an unreadable `/sys/block`.
pub fn health() -> Option<bool> {
    let devices = disks()?;
    if devices.is_empty() {
        return None;
    }
    let mut every_one_answered = true;
    for device in devices {
        match health_of(&device) {
            Some(false) => return Some(false),
            Some(true) => {}
            None => every_one_answered = false,
        }
    }
    every_one_answered.then_some(true)
}
