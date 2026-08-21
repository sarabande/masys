use masys_systemd::proc::cgroup::parse_cgroup;

/// Captured from this host, which uses the unified hierarchy.
#[test]
fn the_unified_hierarchy_is_a_single_line() {
    let text = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/kitty-3002-8.scope\n";
    assert_eq!(parse_cgroup(text).as_deref(), Some("/user.slice/user-1000.slice/user@1000.service/app.slice/kitty-3002-8.scope"));
}

/// The bug this exists for: under cgroup v1 the file is one line per
/// controller, and taking the first picks whichever the kernel listed
/// first rather than the hierarchy systemd organises units by.
#[test]
fn a_v1_host_uses_the_unified_line_not_the_first_controller() {
    let text = "12:pids:/user.slice\n11:memory:/user.slice\n1:name=systemd:/user.slice/session-2.scope\n0::/user.slice/session-2.scope\n";
    assert_eq!(parse_cgroup(text).as_deref(), Some("/user.slice/session-2.scope"));
}

/// A pure v1 host has no unified line at all, and systemd's own named
/// hierarchy is the authority there.
#[test]
fn without_a_unified_line_systemds_own_hierarchy_wins() {
    let text = "12:pids:/user.slice\n11:memory:/system.slice/sshd.service\n1:name=systemd:/system.slice/sshd.service\n";
    assert_eq!(parse_cgroup(text).as_deref(), Some("/system.slice/sshd.service"));
}

/// An unusual layout should still group by something rather than not at
/// all - the first controller is a worse answer than the unified line,
/// but a much better one than nothing.
#[test]
fn an_unrecognised_layout_falls_back_to_the_first_controller() {
    assert_eq!(parse_cgroup("7:blkio:/some/path\n").as_deref(), Some("/some/path"));
}

/// A cgroup path may contain colons - systemd escapes device names into
/// unit names, and those reach the path.
#[test]
fn a_path_containing_colons_survives() {
    let text = "0::/system.slice/system-systemd\\x2dfsck.slice/dev-disk-by\\x2dpath-pci\\x2d0000:00:17.0.device\n";
    assert!(parse_cgroup(text).expect("a path").ends_with("pci\\x2d0000:00:17.0.device"));
}

#[test]
fn an_empty_or_malformed_file_yields_nothing() {
    assert_eq!(parse_cgroup(""), None);
    assert_eq!(parse_cgroup("garbage\n"), None);
    assert_eq!(parse_cgroup("0::\n"), None, "an empty path is not a cgroup");
}
