use masys_platform_nixos::reboot_state;

#[test]
fn identical_store_paths_mean_no_reboot_is_pending() {
    assert_eq!(reboot_state("/nix/store/aaa-sys", "/nix/store/aaa-sys", None, None, None, None), None);
}

/// The whole point of the type. Every other tool prints "reboot required";
/// this one says whether it can wait.
#[test]
fn a_differing_system_with_the_same_kernel_is_activation_only() {
    let state = reboot_state(
        "/nix/store/aaa-sys",
        "/nix/store/bbb-sys",
        Some("/nix/store/k1-linux-6.18.42/bzImage"),
        Some("/nix/store/k1-linux-6.18.42/bzImage"),
        Some("/nix/store/i1-initrd/initrd"),
        Some("/nix/store/i1-initrd/initrd"),
    )
    .unwrap();

    assert!(!state.kernel_changed);
    assert!(!state.initrd_changed);
}

/// The initrd is read once, at boot, so a generation whose initrd moved
/// needs a reboot even though its kernel is bit-identical. Reading only
/// `kernel_changed` would call this activation-only and tell the operator
/// the change was already live, when the part that changed cannot take
/// effect until the next boot.
#[test]
fn a_changed_initrd_under_an_unchanged_kernel_still_needs_a_reboot() {
    let state = reboot_state(
        "/nix/store/aaa-sys",
        "/nix/store/bbb-sys",
        Some("/nix/store/k1-linux-6.18.42/bzImage"),
        Some("/nix/store/k1-linux-6.18.42/bzImage"),
        Some("/nix/store/i1-initrd/initrd"),
        Some("/nix/store/i2-initrd/initrd"),
    )
    .unwrap();

    assert!(!state.kernel_changed, "the kernel is the same store path");
    assert!(state.initrd_changed);
}

#[test]
fn a_changed_kernel_is_reported_as_changed() {
    let state = reboot_state(
        "/nix/store/aaa-sys",
        "/nix/store/bbb-sys",
        Some("/nix/store/k1-linux-6.18.42/bzImage"),
        Some("/nix/store/k2-linux-6.18.44/bzImage"),
        Some("/nix/store/i1-initrd/initrd"),
        Some("/nix/store/i1-initrd/initrd"),
    )
    .unwrap();

    assert!(state.kernel_changed);
}

/// Two builds of the same version are different kernels. Comparing the
/// version string rather than the path would call this unchanged and tell
/// the operator a reboot could wait when it could not.
#[test]
fn the_same_version_from_a_different_build_counts_as_changed() {
    let state = reboot_state(
        "/nix/store/aaa-sys",
        "/nix/store/bbb-sys",
        Some("/nix/store/k1-linux-6.18.42/bzImage"),
        Some("/nix/store/k2-linux-6.18.42/bzImage"),
        Some("/nix/store/i1-initrd/initrd"),
        Some("/nix/store/i1-initrd/initrd"),
    )
    .unwrap();

    assert!(state.kernel_changed);
}

/// A reading that was never taken must never be mistaken for a reading
/// that agreed. If the kernel path could not be read on either side,
/// `kernel_changed` still comes back `true` - masys will not tell somebody
/// a reboot can wait on the strength of a comparison it never actually
/// made.
#[test]
fn an_unread_kernel_is_never_mistaken_for_an_unchanged_one() {
    let state = reboot_state("/nix/store/aaa-sys", "/nix/store/bbb-sys", None, None, None, None).unwrap();

    assert!(state.kernel_changed);
}
