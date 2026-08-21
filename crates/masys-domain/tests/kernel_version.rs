use masys_domain::declarative::kernel_version;

#[test]
fn kernel_version_reads_the_store_path_of_a_bzimage() {
    let path = "/nix/store/0zqfi9rsqj16kk42pvmlcjp7ibv5lck3-linux-6.18.42/bzImage";
    assert_eq!(kernel_version(path).as_deref(), Some("6.18.42"));
    assert_eq!(kernel_version("/nix/store/deadbeef-something-else/bzImage"), None);
}
