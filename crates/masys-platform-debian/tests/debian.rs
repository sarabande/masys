//! The Debian adapter's pure core, which is all of it that can be
//! decided without a Debian host.
//!
//! Every question this adapter answers reduces to a string or a directory
//! listing, and each is passed in rather than read here - the same shape
//! `is_nixos(os_release: &str)` has next door. A test suite that could
//! only run on Debian would be a suite that never runs.

use masys_domain::platform::{Ownership, Package, PlatformId};
use masys_domain::service::PlatformService;
use masys_platform_debian::{
    DebianPlatform, KernelImage, boot_pressure_from, is_debian, packages_from_status, pending_from,
};

/// `ID` for Debian itself, `ID_LIKE` for everything built on it. Ubuntu
/// says `ID=ubuntu` and `ID_LIKE=debian`, and every fact this adapter
/// knows is true there too: `/var/run/reboot-required`, `systemctl enable`
/// persisting, and kernels accumulating in `/boot`.
#[test]
fn debian_and_its_derivatives_are_recognised() {
    assert!(is_debian("ID=debian\nVERSION_ID=\"12\"\n"));
    assert!(is_debian("ID=ubuntu\nID_LIKE=debian\n"));
    assert!(is_debian("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"));
    // Quoted, which os-release permits for any value.
    assert!(is_debian("ID=\"debian\"\n"));
}

#[test]
fn other_distros_are_not() {
    assert!(!is_debian("ID=nixos\n"));
    assert!(!is_debian("ID=arch\n"));
    assert!(!is_debian("ID=fedora\nID_LIKE=\"rhel centos\"\n"));
    assert!(!is_debian(""));
    // `ID_LIKE=debian` is a claim about the family; a distro merely
    // *mentioning* debian in its name is not one.
    assert!(!is_debian("ID=notdebian\n"));
}

/// The marker file is the whole signal: Debian's own `update-notifier`
/// creates `/var/run/reboot-required` and packages append their names to
/// `.pkgs`. Absent means no reboot is pending, which is an answer and not
/// a failure to find out.
#[test]
fn no_marker_file_means_no_reboot_is_pending() {
    assert_eq!(pending_from(None), None);
}

/// The reason names the packages, because that is what the operator is
/// deciding about - a kernel upgrade and a `dbus` upgrade are the same
/// marker file and very different urgencies.
#[test]
fn the_marker_names_the_packages_that_asked_for_it() {
    let pending = pending_from(Some("linux-image-6.1.0-18-amd64\nlibc6\n")).expect("pending");
    assert_eq!(pending.reason, "linux-image-6.1.0-18-amd64, libc6 upgraded");
}

/// An empty `.pkgs`, or none at all, still means a reboot is pending: the
/// marker's existence is the signal and the package list is detail. Saying
/// nothing is pending because the detail is missing would be the reverse
/// of the honest answer.
#[test]
fn a_marker_with_no_package_list_still_reports_a_reboot() {
    let pending = pending_from(Some("")).expect("pending");
    assert_eq!(pending.reason, "a package upgrade needs a reboot");
}

/// Debian's answer, and it is the same for every unit: `systemctl enable`
/// writes a symlink under `/etc/systemd/system` and nothing rewrites it.
/// There is no manifest to revert the change.
#[test]
fn every_unit_is_imperative_because_nothing_owns_it_but_the_admin() {
    for unit in ["ssh.service", "cron.service", "does-not-exist.service"] {
        assert_eq!(
            DebianPlatform.unit_ownership(unit).expect("an answer"),
            Ownership::Imperative,
            "{unit}"
        );
    }
}

#[test]
fn the_platform_identifies_itself() {
    assert_eq!(DebianPlatform.id(), PlatformId::Debian);
}

fn kernel(release: &str, bytes: u64) -> KernelImage {
    KernelImage {
        release: release.to_string(),
        bytes,
    }
}

/// The running kernel is not reclaimable, and on a host that has just
/// booted what it installed there is nothing else - so `None`, meaning
/// "nothing to report", rather than a finding with zero in it.
#[test]
fn only_the_running_kernel_is_no_pressure_at_all() {
    assert_eq!(
        boot_pressure_from(
            &[kernel("6.1.0-18-amd64", 90_000_000)],
            Some("6.1.0-18-amd64")
        ),
        None
    );
    assert_eq!(boot_pressure_from(&[], Some("6.1.0-18-amd64")), None);
}

/// Every kernel that is not the running one is reclaimable, which on
/// Debian is what `apt autoremove` would take.
#[test]
fn old_kernels_are_what_can_be_reclaimed() {
    let pressure = boot_pressure_from(
        &[
            kernel("6.1.0-18-amd64", 90_000_000),
            kernel("6.1.0-17-amd64", 88_000_000),
            kernel("6.1.0-16-amd64", 87_000_000),
        ],
        Some("6.1.0-18-amd64"),
    )
    .expect("two old kernels are pressure");

    assert_eq!(pressure.reclaimable_bytes, Some(175_000_000));
}

/// **The seam's verdict, asserted rather than described.**
///
/// `BootPressure::generations` is `None` here, and it is the reason this
/// adapter was written. Debian's `/boot` is not generation-based - the
/// domain's own doc says a real adapter "would report `generations: 0`" -
/// and `0` is exactly the answer masys refuses everywhere else: the
/// renderer prints `. {n} generations` on the finding, so a Debian host
/// would have read `0 generations`, which is a claim about a NixOS
/// concept this host does not have.
///
/// One adapter could not surface that, because NixOS's own
/// `boot_pressure` answers `Ok(None)` and the field is never populated.
#[test]
fn a_host_with_no_generations_says_so_rather_than_saying_zero() {
    let pressure = boot_pressure_from(
        &[
            kernel("6.1.0-18-amd64", 90_000_000),
            kernel("6.1.0-17-amd64", 88_000_000),
        ],
        Some("6.1.0-18-amd64"),
    )
    .expect("pressure");

    assert_eq!(
        pressure.generations, None,
        "`Some(0)` would render as `0 generations` on a host that has none"
    );
}

/// A `/boot` holding only the running kernel is the common healthy case
/// and must not be reported as pressure with nothing in it.
#[test]
fn the_running_kernel_is_excluded_by_release_not_by_position() {
    let pressure = boot_pressure_from(
        &[
            kernel("6.1.0-16-amd64", 87_000_000),
            kernel("6.1.0-18-amd64", 90_000_000),
            kernel("6.1.0-17-amd64", 88_000_000),
        ],
        Some("6.1.0-18-amd64"),
    )
    .expect("pressure");
    assert_eq!(pressure.reclaimable_bytes, Some(175_000_000));
}

const STATUS: &str = "\
Package: adduser
Status: install ok installed
Priority: important
Version: 3.134

Package: removed-but-configured
Status: deinstall ok config-files
Version: 1.0-2

Package: zlib1g
Status: install ok installed
Version: 1:1.2.13.dfsg-1
";

/// dpkg's status file is the package list. Read rather than shelled out
/// to: `dpkg-query` would be the only subprocess in this crate, and the
/// file it reads is right there.
#[test]
fn installed_packages_come_from_the_dpkg_status_file() {
    let packages = packages_from_status(STATUS);
    assert_eq!(
        packages,
        vec![
            Package {
                name: "adduser".to_string(),
                version: "3.134".to_string()
            },
            Package {
                name: "zlib1g".to_string(),
                version: "1:1.2.13.dfsg-1".to_string()
            },
        ]
    );
}

/// **`config-files` is not installed.** A package purged but for its
/// configuration still has a `Package:` and a `Version:` paragraph, and
/// counting it would report software this host cannot run - which is the
/// dpkg equivalent of the empty list the port refuses to invent.
#[test]
fn a_package_removed_but_for_its_config_is_not_installed() {
    let names: Vec<String> = packages_from_status(STATUS)
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert!(
        !names.iter().any(|n| n == "removed-but-configured"),
        "config-files is a leftover, not an install: {names:?}"
    );
}

/// A paragraph missing either field describes nothing usable.
#[test]
fn a_paragraph_without_both_fields_is_skipped() {
    assert!(packages_from_status("Package: lonely\nStatus: install ok installed\n").is_empty());
    assert!(packages_from_status("Version: 1.0\nStatus: install ok installed\n").is_empty());
    assert!(packages_from_status("").is_empty());
}

/// **An unreadable running release is not a release nothing matches.**
///
/// `running_release()` collapsed a failed read of
/// `/proc/sys/kernel/osrelease` to `""`, which matches no image - so
/// every kernel in `/boot`, the running one included, was summed as
/// reclaimable. The operator was then told to free space that holds the
/// kernel they booted from.
///
/// `None` means no figure, which is what the port's `Option` is for.
#[test]
fn an_unknown_running_release_reports_no_reclaimable_figure() {
    let kernels = [
        kernel("6.1.0-18-amd64", 90_000_000),
        kernel("6.1.0-17-amd64", 88_000_000),
    ];
    assert_eq!(
        boot_pressure_from(&kernels, None),
        None,
        "not knowing which kernel is running is not knowing what is reclaimable"
    );
    // And with the release known, the running one is excluded as before.
    let pressure = boot_pressure_from(&kernels, Some("6.1.0-18-amd64"))
        .expect("one older kernel is reclaimable");
    assert_eq!(pressure.reclaimable_bytes, Some(88_000_000));
}
