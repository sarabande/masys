//! `/proc/mounts` - which filesystems are worth a `statvfs`, and which
//! must not be touched.

/// One mount worth measuring. `fstype` is kept because the caller filters
/// on it, and `device` because two mount points backed by the same device
/// report identical numbers and only one should be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub device: String,
    pub mount_point: String,
    pub fstype: String,
    pub read_only: bool,
}

/// Filesystems that carry no capacity worth reporting - kernel interfaces
/// and per-process views, not storage.
const PSEUDO: &[&str] = &[
    "proc",
    "sysfs",
    "cgroup",
    "cgroup2",
    "devpts",
    "securityfs",
    "debugfs",
    "tracefs",
    "fusectl",
    "configfs",
    "pstore",
    "bpf",
    "mqueue",
    "hugetlbfs",
    "binfmt_misc",
    "nsfs",
    "selinuxfs",
    "efivarfs",
    "devtmpfs",
    "ramfs",
    "autofs",
];

/// `autofs` is in that list for a reason that is not tidiness: calling
/// `statvfs` on an autofs mount point *triggers the automount*, so a
/// routine disk poll could block on mounting a network share. It has to be
/// excluded by fstype, before the path is touched at all.
///
/// Escape sequences: the kernel octal-escapes space, tab, newline and
/// backslash in both the device and mount point fields, so a mount point
/// like `/mnt/my disk` arrives as `/mnt/my\040disk`.
pub fn parse_mounts(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            let device = unescape(columns.next()?);
            let mount_point = unescape(columns.next()?);
            let fstype = columns.next()?.to_string();
            let options = columns.next().unwrap_or_default();
            if PSEUDO.contains(&fstype.as_str()) {
                return None;
            }
            Some(Mount { device, mount_point, fstype, read_only: options.split(',').any(|o| o == "ro") })
        })
        .collect()
}

fn unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let octal: String = chars.clone().take(3).collect();
        match u8::from_str_radix(&octal, 8) {
            Ok(byte) if octal.len() == 3 => {
                out.push(byte as char);
                chars.nth(2);
            }
            _ => out.push('\\'),
        }
    }
    out
}

/// Whether a read-only mount means *the kernel remounted it after an I/O
/// error* - the silent failure `Finding::ReadOnlyFilesystem` exists to
/// catch - rather than a deliberate mount option.
///
/// Only a local, block-device-backed filesystem can fail that way. A
/// read-only NFS export, a squashfs, or a `/run/credentials` tmpfs is a
/// configuration choice; reporting those fires five false findings on the
/// development host alone, which is exactly the noise that trains an
/// operator to ignore the tool.
pub fn is_kernel_remount(mount: &Mount) -> bool {
    mount.read_only && mount.device.starts_with("/dev/")
}
