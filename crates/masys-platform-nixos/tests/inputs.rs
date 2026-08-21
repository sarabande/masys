use masys_platform_nixos::inputs::parse_lock;

const LOCK: &str = r#"{
  "nodes": {
    "root": { "inputs": { "nixpkgs": "nixpkgs", "sops-nix": "sops-nix" } },
    "nixpkgs": {
      "locked": { "lastModified": 1785828668, "owner": "NixOS", "repo": "nixpkgs",
                  "rev": "e72e4f299401a3689d4b3d5fc6496b11db7064eb", "type": "github" }
    },
    "sops-nix": {
      "inputs": { "flake-compat": "flake-compat" },
      "locked": { "lastModified": 1783174389, "owner": "Mic92", "repo": "sops-nix",
                  "rev": "aaaa", "type": "github" }
    },
    "flake-compat": {
      "locked": { "lastModified": 1767039857, "owner": "edolstra", "repo": "flake-compat",
                  "rev": "bbbb", "type": "github" }
    }
  },
  "root": "root", "version": 7
}"#;

#[test]
fn every_locked_node_becomes_an_input_and_root_itself_does_not() {
    let inputs = parse_lock(LOCK).unwrap();
    let mut names: Vec<&str> = inputs.iter().map(|i| i.name.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["flake-compat", "nixpkgs", "sops-nix"]);
}

/// A 235-day-old transitive flake-compat is not the same finding as a
/// stale nixpkgs, so the row has to know which it is.
#[test]
fn inputs_the_configuration_names_are_direct_and_the_rest_are_not() {
    let inputs = parse_lock(LOCK).unwrap();
    let direct = |name: &str| inputs.iter().find(|i| i.name == name).unwrap().direct;
    assert!(direct("nixpkgs"));
    assert!(direct("sops-nix"));
    assert!(!direct("flake-compat"));
}

#[test]
fn origin_and_revision_come_from_the_locked_block() {
    let inputs = parse_lock(LOCK).unwrap();
    let nixpkgs = inputs.iter().find(|i| i.name == "nixpkgs").unwrap();
    assert_eq!(nixpkgs.origin.as_deref(), Some("NixOS/nixpkgs"));
    assert_eq!(nixpkgs.rev.as_deref(), Some("e72e4f299401a3689d4b3d5fc6496b11db7064eb"));
    assert_eq!(nixpkgs.last_modified_secs, Some(1785828668));
}

#[test]
fn a_lock_that_is_not_json_is_an_error_rather_than_an_empty_list() {
    assert!(parse_lock("not json at all").is_err());
}

const LOCK_WITH_ARRAY_FOLLOWS: &str = r#"{
  "nodes": {
    "root": { "inputs": { "nixpkgs": ["sops-nix", "nixpkgs"], "sops-nix": "sops-nix" } },
    "nixpkgs": {
      "locked": { "lastModified": 1785828668, "owner": "NixOS", "repo": "nixpkgs",
                  "rev": "e72e4f299401a3689d4b3d5fc6496b11db7064eb", "type": "github" }
    },
    "sops-nix": {
      "inputs": { "nixpkgs": "nixpkgs" },
      "locked": { "lastModified": 1783174389, "owner": "Mic92", "repo": "sops-nix",
                  "rev": "aaaa", "type": "github" }
    }
  },
  "root": "root", "version": 7
}"#;

/// The real shape on this host's own `~/.dotfiles/flake.lock`: a root
/// input deduplicated onto another input's own pin is locked as a path,
/// not a plain node-key string. `.as_str()` on that array used to return
/// `None` and silently drop it from the direct set - the exact inversion
/// `direct` exists to prevent.
#[test]
fn an_array_valued_root_input_is_still_direct() {
    let inputs = parse_lock(LOCK_WITH_ARRAY_FOLLOWS).unwrap();
    let nixpkgs = inputs.iter().find(|i| i.name == "nixpkgs").unwrap();
    assert!(nixpkgs.direct, "a root input locked as a follows path is still one the configuration names directly");
}

const LOCK_WITH_ALIAS_NODE: &str = r#"{
  "nodes": {
    "root": { "inputs": { "nixpkgs": "nixpkgs" } },
    "nixpkgs": {
      "locked": { "lastModified": 1785828668, "owner": "NixOS", "repo": "nixpkgs",
                  "rev": "e72e4f299401a3689d4b3d5fc6496b11db7064eb", "type": "github" }
    },
    "an-alias": { "inputs": { "nixpkgs": "nixpkgs" } }
  },
  "root": "root", "version": 7
}"#;

#[test]
fn a_node_with_no_locked_block_produces_no_row() {
    let inputs = parse_lock(LOCK_WITH_ALIAS_NODE).unwrap();
    assert!(inputs.iter().all(|i| i.name != "an-alias"));
}

const LOCK_WITH_MALFORMED_FIELDS: &str = r#"{
  "nodes": {
    "root": { "inputs": { "weird": "weird" } },
    "weird": {
      "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": 12345 }
    }
  },
  "root": "root", "version": 7
}"#;

/// A `rev` recorded as the wrong JSON type, or a `lastModified` the lock
/// simply does not carry, both read as `None` rather than a fabricated
/// value - `.as_str()`/`.as_u64()` return `None` on a type mismatch or a
/// missing key alike, and `parse_lock` must not paper over either with a
/// guess.
#[test]
fn a_malformed_or_absent_field_reads_as_none_not_a_fabricated_value() {
    let inputs = parse_lock(LOCK_WITH_MALFORMED_FIELDS).unwrap();
    let weird = inputs.iter().find(|i| i.name == "weird").unwrap();
    assert_eq!(weird.rev, None);
    assert_eq!(weird.last_modified_secs, None);
    assert_eq!(weird.origin.as_deref(), Some("NixOS/nixpkgs"));
}
