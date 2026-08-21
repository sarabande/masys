use masys_domain::sample::ProcState;
use masys_systemd::proc::diskstats::parse_diskstats;
use masys_systemd::proc::io::parse_io;
use masys_systemd::proc::loadavg::{parse_loadavg, parse_uptime};
use masys_systemd::proc::meminfo::{parse_meminfo, parse_swaps_zram_percent};
use masys_systemd::proc::mounts::{is_kernel_remount, parse_mounts};
use masys_systemd::proc::pressure::parse_pressure;
use masys_systemd::proc::stat::parse_stat;
use masys_systemd::proc::status::parse_status;

/// Captured from this machine, so the field offsets are real ones.
const STAT: &str = "1762908 (cat) R 1762906 1762908 1762906 0 -1 4194304 417 0 0 0 11 7 0 0 20 0 1 0 8591204 17313792 1655 18446744073709551615 97983470743552 97983478882365 140721584465072 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0";

#[test]
fn stat_reads_the_fields_proc_needs() {
    let raw = parse_stat(STAT).expect("a parsed stat line");
    assert_eq!(raw.comm, "cat");
    assert_eq!(raw.state, ProcState::Running);
    assert_eq!(raw.cpu_ticks, 18, "utime 11 + stime 7, children excluded");
    assert_eq!(raw.nice, 0);
    assert_eq!(raw.starttime_ticks, 8_591_204);
    assert_eq!(raw.rss_pages, 1655);
}

/// The trap this parser exists for: `comm` is process-chosen bytes, so it
/// can hold spaces *and* parentheses. Splitting on whitespace, or on the
/// first `)`, shifts every field after it.
#[test]
fn stat_survives_a_comm_containing_spaces_and_parentheses() {
    let line = STAT.replacen("(cat)", "((evil) proc name)", 1);
    let raw = parse_stat(&line).expect("a parsed stat line");
    assert_eq!(raw.comm, "(evil) proc name");
    assert_eq!(raw.cpu_ticks, 18, "fields after comm are still aligned");
    assert_eq!(raw.starttime_ticks, 8_591_204);
}

#[test]
fn stat_maps_every_state_letter_it_can_see() {
    for (letter, expected) in [
        ("R", ProcState::Running),
        ("S", ProcState::Sleeping),
        ("D", ProcState::UninterruptibleWait),
        ("Z", ProcState::Zombie),
        ("T", ProcState::Stopped),
        // A kernel idle thread. Unknown letters must not fail a sample.
        ("I", ProcState::Sleeping),
    ] {
        let line = STAT.replacen(") R ", &format!(") {letter} "), 1);
        assert_eq!(parse_stat(&line).expect("parsed").state, expected, "state {letter}");
    }
}

#[test]
fn stat_rejects_a_line_with_no_comm() {
    assert!(parse_stat("1762908 cat R 1 2 3").is_err());
}

#[test]
fn io_reads_bytes_that_reached_storage_not_syscall_traffic() {
    let text = "rchar: 12063\nwchar: 0\nsyscr: 18\nsyscw: 0\nread_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 0\n";
    assert_eq!(parse_io(text).expect("parsed io"), (4096, 8192), "rchar/wchar are cache traffic, not disk");
}

#[test]
fn io_without_the_byte_counters_is_an_error() {
    assert!(parse_io("rchar: 12063\nwchar: 0\n").is_err());
}

#[test]
fn status_reads_threads_and_rss() {
    let text = "Name:\tcat\nState:\tR (running)\nVmRSS:\t    7720 kB\nThreads:\t2\n";
    let raw = parse_status(text).expect("parsed status");
    assert_eq!(raw.threads, 2);
    assert_eq!(raw.rss_bytes, 7720 * 1024);
}

/// Kernel threads have no VmRSS line at all.
#[test]
fn status_without_vmrss_reports_zero_rss() {
    let raw = parse_status("Name:\tkthreadd\nThreads:\t1\n").expect("parsed status");
    assert_eq!(raw.rss_bytes, 0);
    assert_eq!(raw.threads, 1);
}

#[test]
fn pressure_reads_some_and_full() {
    let text = "some avg10=0.04 avg60=0.23 avg300=0.12 total=586715292\nfull avg10=0.00 avg60=1.50 avg300=0.00 total=0\n";
    let (some, full) = parse_pressure(text).expect("parsed pressure");
    assert_eq!((some.avg10, some.avg60, some.avg300), (0.04, 0.23, 0.12));
    assert_eq!(full.avg60, 1.50);
}

/// Older kernels emit no `full` line for cpu at all.
#[test]
fn pressure_without_a_full_line_defaults_it_to_zero() {
    let (some, full) = parse_pressure("some avg10=0.04 avg60=0.23 avg300=0.12 total=1\n").expect("parsed");
    assert_eq!(some.avg60, 0.23);
    assert_eq!(full.avg60, 0.0);
}

const MEMINFO: &str = "MemTotal:       32753552 kB\nMemFree:         4842884 kB\nMemAvailable:   21429936 kB\nBuffers:         2884264 kB\nSwapTotal:       3275260 kB\nSwapFree:        3274104 kB\n";

#[test]
fn meminfo_uses_available_not_free_for_used() {
    let swaps =
        "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n/dev/zram0                              partition\t3275260\t\t1156\t\t5\n";
    let memory = parse_meminfo(MEMINFO, swaps).expect("parsed meminfo");
    assert_eq!(memory.total_bytes, 32_753_552 * 1024);
    // MemTotal - MemAvailable. Using MemFree would report 27.4G used on a
    // machine with 20G of reclaimable cache.
    assert_eq!(memory.used_bytes, (32_753_552 - 21_429_936) * 1024);
    assert_eq!(memory.swap_free_bytes, 3_274_104 * 1024);
}

#[test]
fn zram_percent_is_fullness_of_the_zram_swap_devices() {
    let swaps = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n/dev/zram0                              partition\t1000\t\t250\t\t5\n";
    assert_eq!(parse_swaps_zram_percent(swaps), Some(25.0));
}

#[test]
fn a_host_with_no_zram_reports_none_rather_than_zero() {
    let swaps = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n/dev/sda3                               partition\t8388604\t\t0\t\t-2\n";
    assert_eq!(parse_swaps_zram_percent(swaps), None, "no zram is not the same fact as an empty zram");
    assert_eq!(parse_swaps_zram_percent("Filename\tType\tSize\tUsed\tPriority\n"), None);
}

#[test]
fn loadavg_takes_the_three_averages_and_ignores_the_rest() {
    let load = parse_loadavg("1.37 0.50 0.28 1/1340 1758249\n").expect("parsed loadavg");
    assert_eq!((load.one, load.five, load.fifteen), (1.37, 0.50, 0.28));
}

#[test]
fn uptime_truncates_to_whole_seconds() {
    assert_eq!(parse_uptime("85356.39 429938.13\n").expect("parsed uptime"), 85_356);
}

const MOUNTS: &str = "/dev/sda2 / ext4 rw,relatime 0 0\ntmpfs /run tmpfs rw,nosuid,nodev 0 0\nproc /proc proc rw,nosuid 0 0\n/dev/sda2 /nix/store ext4 ro,relatime 0 0\nsystemd-1 /mnt/backup autofs rw,relatime 0 0\ncgroup2 /sys/fs/cgroup cgroup2 rw,nosuid 0 0\n/dev/sdb1 /mnt/my\\040disk ext4 rw 0 0\n";

/// The hazard this filter exists for: `statvfs` on an autofs mount point
/// triggers the automount, so a routine disk poll could block on mounting
/// a network share.
#[test]
fn mounts_never_yields_an_autofs_entry() {
    let mounts = parse_mounts(MOUNTS);
    assert!(!mounts.iter().any(|m| m.fstype == "autofs"), "{mounts:#?}");
    assert!(!mounts.iter().any(|m| m.mount_point == "/mnt/backup"), "{mounts:#?}");
}

#[test]
fn mounts_drops_pseudo_filesystems_but_keeps_real_ones() {
    let mounts = parse_mounts(MOUNTS);
    let points: Vec<&str> = mounts.iter().map(|m| m.mount_point.as_str()).collect();
    assert_eq!(points, vec!["/", "/run", "/nix/store", "/mnt/my disk"], "{mounts:#?}");
}

#[test]
fn mounts_reports_read_only_from_the_option_list() {
    let mounts = parse_mounts(MOUNTS);
    let store = mounts.iter().find(|m| m.mount_point == "/nix/store").expect("a /nix/store mount");
    assert!(store.read_only, "ro in the options means remounted read-only");
    let root = mounts.iter().find(|m| m.mount_point == "/").expect("a / mount");
    assert!(!root.read_only);
}

/// Two mount points on one device report identical numbers; only one
/// belongs in the Disk section.
#[test]
fn mounts_keeps_the_device_so_bind_mounts_can_be_deduped() {
    let mounts = parse_mounts(MOUNTS);
    let same: Vec<&str> = mounts.iter().filter(|m| m.device == "/dev/sda2").map(|m| m.mount_point.as_str()).collect();
    assert_eq!(same, vec!["/", "/nix/store"], "both survive parsing; dedupe is the caller's job");
}

/// The kernel octal-escapes space, tab, newline and backslash.
#[test]
fn mounts_unescapes_octal_sequences_in_paths() {
    let mounts = parse_mounts(MOUNTS);
    assert!(mounts.iter().any(|m| m.mount_point == "/mnt/my disk"), "{mounts:#?}");
}

/// A read-only NFS export is a mount option, not the kernel giving up on
/// a failing disk. Treating them alike fires false findings on any host
/// with a read-only network mount - three of them on the development
/// machine.
#[test]
fn only_a_local_block_device_can_be_a_kernel_remount() {
    let text = "/dev/sda2 / ext4 ro,relatime 0 0\n\
                198.51.100.20:/data/photos /mnt/photos nfs4 ro,noatime 0 0\n\
                none /run/credentials/systemd-journald.service tmpfs ro,nosuid 0 0\n\
                /dev/sdb1 /data ext4 rw,relatime 0 0\n";
    let mounts = parse_mounts(text);
    let by_point = |p: &str| mounts.iter().find(|m| m.mount_point == p).expect("a mount").clone();

    assert!(is_kernel_remount(&by_point("/")), "a read-only local disk is the real alarm");
    assert!(!is_kernel_remount(&by_point("/mnt/photos")), "a read-only NFS export is a mount option");
    assert!(!is_kernel_remount(&by_point("/run/credentials/systemd-journald.service")), "a read-only tmpfs is by design");
    assert!(!is_kernel_remount(&by_point("/data")), "a writable disk is not remounted");
}

/// Captured from this host. Partitions and their whole device both appear
/// in `/proc/diskstats` with overlapping counters, so listing all of them
/// would show the same writes two or three times over.
#[test]
fn diskstats_reports_whole_devices_not_partitions() {
    let text = "   8       0 sda 1412811 278825 34105044 491833 1651028 2245460 405386773 3237644 0 849688 3809315 0 0 0 0\n\
                   8       1 sda1 227 2024 14730 97 250 289 533 66 0 112 163 0 0 0 0\n\
                   8       2 sda2 1412504 276801 34087586 491710 1650764 2245171 405386240 3237540 0 978585 3729251 0 0 0 0\n";
    let disks = parse_diskstats(text);
    assert_eq!(disks.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["sda"]);
    assert_eq!(disks[0].reads, 1_412_811);
    assert_eq!(disks[0].write_sectors, 405_386_773);
    assert_eq!(disks[0].io_ms, 849_688);
}

/// The digit-suffix test alone is wrong: `zram0` and `nvme0n1` end in a
/// digit and are whole devices. It only counts as a partition when the
/// device it would belong to is present too.
#[test]
fn a_device_whose_name_ends_in_a_digit_is_not_a_partition() {
    let row = |name: &str| format!("   1       0 {name} 1 2 3 4 5 6 7 8 0 9 10 0 0 0 0\n");
    let text = format!("{}{}{}{}", row("zram0"), row("nvme0n1"), row("nvme0n1p1"), row("nvme0n1p2"));
    let names: Vec<String> = parse_diskstats(&text).into_iter().map(|d| d.name).collect();
    assert_eq!(names, vec!["zram0", "nvme0n1"], "nvme partitions drop, the devices stay");
}

/// Kernels since 4.18 append discard and flush statistics. Reading by
/// index from the left is what makes that forward-compatible.
#[test]
fn extra_trailing_fields_are_ignored() {
    let short = "   8       0 sda 1 2 3 4 5 6 7 8 0 9 10 0 0 0 0\n";
    let long = "   8       0 sda 1 2 3 4 5 6 7 8 0 9 10 0 0 0 0 11 12 13 14\n";
    assert_eq!(parse_diskstats(short), parse_diskstats(long));
}

#[test]
fn a_truncated_diskstats_line_is_skipped() {
    assert!(parse_diskstats("   8       0 sda 1 2 3\n").is_empty());
}

/// `/proc/net/dev`'s two header lines are not a table anyone can parse by
/// column name - the `Receive`/`Transmit` split is positional, and the
/// interface name carries a trailing colon.
#[test]
fn net_dev_is_parsed_by_position() {
    let text = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 3794998   50414    0    0    0     0          0         0  3794998   50414    0    0    0     0       0          0
enp0s31f6: 6736398191 8876129    0    2    0     0          0    327165 9585007885 8957151    0   37    0     0       0          0
";
    let parsed = masys_systemd::proc::net_dev::parse_net_dev(text);
    assert_eq!(parsed.len(), 2);

    let lo = &parsed[0];
    assert_eq!(lo.name, "lo", "the trailing colon is not part of the name");
    assert_eq!(lo.rx_bytes, 3_794_998);
    assert_eq!(lo.tx_bytes, 3_794_998);

    let nic = &parsed[1];
    assert_eq!(nic.name, "enp0s31f6");
    assert_eq!(nic.rx_bytes, 6_736_398_191, "past 32 bits, so u64 or it wraps");
    assert_eq!(nic.rx_packets, 8_876_129);
    assert_eq!(nic.rx_drop, 2);
    assert_eq!(nic.tx_bytes, 9_585_007_885);
    assert_eq!(nic.tx_drop, 37, "the transmit columns start over, eight fields along");
    assert_eq!(nic.tx_errs, 0);
}

/// A name with no space before the colon is the common case for a long
/// interface name, and one *with* a space is the common case for a short
/// one. Both are the same line format.
#[test]
fn a_short_interface_name_is_padded_away_from_its_colon() {
    let text = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
  eth0: 100 2 0 0 0 0 0 0 200 4 0 0 0 0 0 0
";
    let parsed = masys_systemd::proc::net_dev::parse_net_dev(text);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].name, "eth0");
    assert_eq!(parsed[0].rx_bytes, 100);
    assert_eq!(parsed[0].tx_bytes, 200);
}

/// Anything that is not a counter line is skipped rather than guessed at.
#[test]
fn net_dev_headers_and_junk_are_skipped() {
    assert!(masys_systemd::proc::net_dev::parse_net_dev("").is_empty());
    assert!(masys_systemd::proc::net_dev::parse_net_dev("Inter-|   Receive\n face |bytes\n").is_empty());
    assert!(masys_systemd::proc::net_dev::parse_net_dev("garbage with no colon\n").is_empty());
}
