//! What pins this configuration: a flake lock, a channel profile, or
//! neither.
//!
//! Neither is a real state and reads as `None`. A host with an empty
//! `/etc/nixos`, no channels and a flake three directories away in
//! `$NH_FLAKE` is not exotic - it is the development host - so nothing
//! here classifies a host into a paradigm. It reports what it finds.

use masys_domain::declarative::Input;
use masys_domain::error::MasysError;

/// Every locked node in a `flake.lock`, with the ones the configuration
/// names directly marked - by the name `root` declares them under, not by
/// resolving what a renamed or nested `follows` ultimately targets.
///
/// The lock's `root` node lists the direct inputs and is not itself an
/// input, so it contributes the `direct` flag and no row of its own.
pub fn parse_lock(text: &str) -> Result<Vec<Input>, MasysError> {
    let lock: serde_json::Value =
        serde_json::from_str(text).map_err(|error| MasysError::Platform(format!("flake.lock is not readable: {error}")))?;

    let root_key = lock.get("root").and_then(|value| value.as_str()).unwrap_or("root");
    let nodes = lock
        .get("nodes")
        .and_then(|value| value.as_object())
        .ok_or_else(|| MasysError::Platform("flake.lock has no nodes object".to_string()))?;

    // Direct-ness only needs the root input's own declared name, not what
    // it resolves to. The lock records that resolution as a plain
    // node-key string for an input pinned or aliased to a sibling, but as
    // a path array - e.g. `["sops-nix", "nixpkgs"]` for an input that
    // follows into another input's own input - when reaching it means
    // walking the graph rather than reading one key. Keying off the
    // map's own keys instead of its values reads right under both shapes
    // without resolving that path: this only needs to know the
    // configuration names an input at this name, not which physical node
    // a renamed or nested follows ultimately reaches.
    let direct: Vec<&str> = nodes
        .get(root_key)
        .and_then(|node| node.get("inputs"))
        .and_then(|inputs| inputs.as_object())
        .map(|inputs| inputs.keys().map(String::as_str).collect())
        .unwrap_or_default();

    let mut out = Vec::new();
    for (name, node) in nodes {
        if name == root_key {
            continue;
        }
        // A node with no `locked` block does not occur in a v7 lock in
        // practice - a `follows` is always an inline path within the
        // referencing node's own `inputs`, never a separate node lacking
        // `locked` - but skipping it costs nothing and guards against a
        // lock shape this crate has not seen rather than assuming the
        // shape it has.
        let Some(locked) = node.get("locked") else { continue };
        let text_at = |key: &str| locked.get(key).and_then(|value| value.as_str()).map(str::to_string);
        let origin = match (text_at("owner"), text_at("repo")) {
            (Some(owner), Some(repo)) => Some(format!("{owner}/{repo}")),
            _ => text_at("url").or_else(|| text_at("path")),
        };
        out.push(Input {
            name: name.clone(),
            origin,
            rev: text_at("rev"),
            last_modified_secs: locked.get("lastModified").and_then(serde_json::Value::as_u64),
            direct: direct.contains(&name.as_str()),
        });
    }
    out.sort_by(|a, b| b.direct.cmp(&a.direct).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// Where this host's `flake.lock` is, in the order the design settles on:
/// an explicit setting, then `$NH_FLAKE`, then `$FLAKE`, then
/// `/etc/nixos/flake.nix`.
///
/// `$NH_FLAKE` is read because it is set on real NixOS hosts - it is the
/// only way the development host's configuration is reachable at all - and
/// costs one `env::var`. Reading a variable another tool defines is not a
/// dependency on that tool.
pub fn locate_lock(configured: Option<&str>) -> Option<std::path::PathBuf> {
    let candidates =
        [configured.map(str::to_string), std::env::var("NH_FLAKE").ok(), std::env::var("FLAKE").ok(), Some("/etc/nixos".to_string())];
    candidates
        .into_iter()
        .flatten()
        .map(|dir| {
            // A ref may carry a `#attr`; the lock is beside the flake,
            // not inside the attribute. `split_once` returns `None` when
            // there is no `#` at all, which is the ordinary case, so the
            // fallback here is real - unlike `split('#').next()`, which
            // always yields something and made the equivalent fallback
            // dead code.
            let path = dir.split_once('#').map_or(dir.as_str(), |(head, _)| head);
            std::path::PathBuf::from(path).join("flake.lock")
        })
        .find(|path| path.is_file())
}
