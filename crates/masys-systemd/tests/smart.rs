//! The two decisions in a SMART read that do not need a disk: which
//! devices to ask, and what an answer means.

use masys_systemd::smart::{args_for, asks_smart, health_from_output};

/// The sentence ATA drives print. `smartctl -H /dev/sda` on a healthy
/// SATA disk, trimmed to the lines that matter.
const ATA_PASSED: &str = "\
smartctl 7.4 2023-08-01 r5530 [x86_64-linux-6.6.30] (local build)
Copyright (C) 2002-23, Bruce Allen, Christian Franke, www.smartmontools.org

=== START OF READ SMART DATA SECTION ===
SMART overall-health self-assessment test result: PASSED
";

const ATA_FAILED: &str = "\
=== START OF READ SMART DATA SECTION ===
SMART overall-health self-assessment test result: FAILED!
Drive failure expected in less than 24 hours. SAVE ALL DATA.
";

/// SAS and SCSI print a different sentence for the same question.
const SCSI_OK: &str = "\
=== START OF READ SMART DATA SECTION ===
SMART Health Status: OK
";

const SCSI_FAILING: &str = "\
=== START OF READ SMART DATA SECTION ===
SMART Health Status: HARDWARE IMPENDING FAILURE GENERAL HARD DRIVE FAILURE [asc=5d, ascq=10]
";

#[test]
fn a_drive_that_says_it_is_passing_is_passing() {
    assert_eq!(health_from_output(ATA_PASSED), Some(true));
    assert_eq!(health_from_output(SCSI_OK), Some(true));
}

#[test]
fn a_drive_that_says_it_expects_to_fail_is_failing() {
    assert_eq!(health_from_output(ATA_FAILED), Some(false));
    assert_eq!(
        health_from_output(SCSI_FAILING),
        Some(false),
        "the SCSI wording is a sentence rather than a keyword, so anything \
         that is not OK is not OK"
    );
}

/// **Everything that is not an answer is `None`, and this is the test
/// worth having.**
///
/// A disk in standby, a controller that passes nothing through, a device
/// with no SMART support, `smartctl` refusing for want of root: each
/// prints something, none of it a verdict. Reading any of them as healthy
/// would be a claim about a reading nobody took - and it is the claim an
/// operator would act on.
#[test]
fn everything_that_is_not_a_verdict_is_unknown() {
    for output in [
        "",
        "Device is in STANDBY mode, exit(2)\n",
        "/dev/sda: Unable to detect device type\n",
        "Smartctl open device: /dev/sda failed: Permission denied\n",
        "=== START OF INFORMATION SECTION ===\nDevice Model:     Virtual disk\n",
        // The words are there, but not as this drive's verdict.
        "See SMART Health Status: in the manual page\n ",
    ] {
        assert_eq!(
            health_from_output(output),
            None,
            "not a verdict, so not an answer: {output:?}"
        );
    }
}

#[test]
fn only_devices_with_firmware_to_ask_are_asked() {
    for disk in ["sda", "sdb", "nvme0n1", "hda", "vda", "mmcblk0"] {
        assert!(asks_smart(disk), "{disk} is a disk");
    }
    // Kernel constructs and non-disks. Each costs a process to ask and
    // answers nothing; this host carries eight unused loop devices.
    for not_a_disk in [
        "loop0", "loop7", "ram0", "zram0", "md127", "dm-0", "sr0", "fd0", "nbd3", "",
    ] {
        assert!(
            !asks_smart(not_a_disk),
            "{not_a_disk:?} has no self-assessment to give"
        );
    }
}

/// **A spinning disk is asked politely; an NVMe drive is just asked.**
///
/// `-n standby` is implemented for ATA devices only. Handed an NVMe
/// device, `smartctl` rejects the option and prints no verdict - which
/// reads as "cannot tell", correctly and forever, on a host whose only
/// disk is NVMe. That is most current hardware, and the feature would
/// have looked like it worked while never once firing.
#[test]
fn nvme_is_not_asked_about_a_power_mode_it_does_not_have() {
    assert_eq!(
        args_for("sda"),
        vec!["-H", "-n", "standby", "/dev/sda"],
        "a disk that can spin down is not woken to be asked"
    );
    assert_eq!(
        args_for("nvme0n1"),
        vec!["-H", "/dev/nvme0n1"],
        "and one that cannot is asked without the option that would be refused"
    );
}
