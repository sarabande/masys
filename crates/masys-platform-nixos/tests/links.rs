use masys_platform_nixos::links::{generation_id, version_label};

#[test]
fn generation_id_reads_the_number_between_prefix_and_suffix() {
    assert_eq!(generation_id("system-437-link", "system"), Some(437));
    assert_eq!(generation_id("profile-47-link", "profile"), Some(47));
    assert_eq!(generation_id("channels-3-link", "channels"), Some(3));
}

/// The profile symlink itself sits in the same directory as its
/// generations and must not be read as one.
#[test]
fn generation_id_rejects_the_profile_link_and_foreign_names() {
    assert_eq!(generation_id("system", "system"), None);
    assert_eq!(generation_id("system-437-link", "profile"), None);
    assert_eq!(generation_id("system-abc-link", "system"), None);
    assert_eq!(generation_id("system-437", "system"), None);
}

#[test]
fn version_label_reads_what_follows_the_host_name() {
    let path = "/nix/store/iqmqj06r37ma91cvp1xhw281vjv0kqln-nixos-system-devbox-26.11.20260804.e72e4f2";
    assert_eq!(version_label(path).as_deref(), Some("26.11.20260804.e72e4f2"));
}

/// The home profile's store paths are a bare `-profile` with no version in
/// them. `None` is the honest answer; inventing one would put a label on
/// the row that no part of the system agrees with.
#[test]
fn version_label_is_absent_where_the_path_carries_none() {
    assert_eq!(version_label("/nix/store/ym3jcfahrwr7nr8pkikcwmfnniwbnws7-profile"), None);
    assert_eq!(version_label(""), None);
}

/// The parser splits from the right specifically because a host name may
/// itself contain a hyphen; a left-to-right split would misread `server`
/// as the version instead of the actual date-stamped one.
#[test]
fn version_label_survives_a_hyphenated_host_name() {
    let path = "/nix/store/iqmqj06r37ma91cvp1xhw281vjv0kqln-nixos-system-web-server-26.11.20260804.e72e4f2";
    assert_eq!(version_label(path).as_deref(), Some("26.11.20260804.e72e4f2"));
}

/// A host name ending in digits, on a path that carries no real version
/// suffix, must not be mistaken for one: `Some("2")` would be a wrong
/// version stated as confidently as a right one.
#[test]
fn version_label_rejects_a_host_name_ending_in_digits() {
    assert_eq!(version_label("/nix/store/hash-nixos-system-devbox-2"), None);
}
