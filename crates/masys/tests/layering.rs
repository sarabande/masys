//! The dependency direction, which the design calls load-bearing.
//!
//! Ports and adapters is a claim about which way the arrows point, and a
//! claim nothing checks is a convention. This checks it, and it checks
//! the two halves separately, because a layer violation usually arrives
//! as an innocent one-liner - "just read the unit here" - plus a
//! one-line manifest edit, and neither half looks wrong on its own.
//!
//! Lives in the `masys` crate because that crate is the composition
//! root: the one place allowed to name every other, and so the natural
//! home for the test that polices everyone else.
//!
//! Manifests only, deliberately. A companion check that grepped source
//! for `masys_systemd::` in masys-app would never fire: naming a crate
//! that is not a dependency does not compile, so rustc has already
//! refused it long before a test could run. The half worth checking is
//! the manifest, because *that* edit compiles fine on its own and is
//! what quietly makes the other half legal.
//!
//! `masys-app` appearing under masys-render's `[dev-dependencies]` is
//! not a violation and is why only `[dependencies]` is read: the
//! renderer's end-to-end tests drive a real session, which is the point
//! of having them.
//!
//! No dependencies. The manifests are read line by line rather than
//! parsed, which is enough for the question being asked - "does this
//! name appear as a dependency" - and keeps a test about the workspace's
//! dependencies from adding one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What a crate may **not** depend on, and why. Prohibitions rather than
/// an allowlist: adding `unicode-width` to the renderer breaks nothing,
/// but adding `masys-systemd` to `masys-app` inverts the architecture.
const FORBIDDEN_DEPS: &[(&str, &str, &str)] = &[
    (
        "masys-app",
        "masys-systemd",
        "masys-app talks to the SystemService port, never to the infrastructure behind it. \
         The `masys` binary wires the two together and selects the platform adapter.",
    ),
    (
        "masys-app",
        "ratatui",
        "No layer above the composition root names a terminal library. masys-app defines \
         its own Key; the binary converts crossterm's.",
    ),
    ("masys-app", "crossterm", "No layer above the composition root names a terminal library."),
    ("masys-app", "zbus", "D-Bus types never appear above masys-systemd."),
    ("masys-render", "masys-app", "The renderer draws a View. It never reaches back into the session."),
    ("masys-render", "masys-systemd", "The renderer draws a View, not a machine."),
    ("masys-systemd", "masys-app", "An adapter is below the session, not beside it."),
    ("masys-systemd", "masys-view", "An adapter produces domain types; the row model is above it."),
];

/// Crates whose dependency list is *exactly* this, because the design
/// states it for them rather than merely implying it.
const EXACT_DEPS: &[(&str, &[&str], &str)] = &[
    (
        "masys-domain",
        &["thiserror"],
        "masys-domain is the bottom layer: entities, use cases, and the SystemService/\
         PlatformService ports. Its only dependency is thiserror.",
    ),
    (
        "masys-view",
        &["masys-domain"],
        "masys-view is the app<->render contract and depends on masys-domain alone - \
         no ratatui, no transient engine, no decisions.",
    ),
];

/// Naming a platform adapter is the composition root's exclusive right.
/// Distro specifics are quarantined in those crates precisely so nothing
/// above them has to know which distro this is - the whole reason
/// `PlatformService` exists rather than an `if nixos` in the app.
const PLATFORM_CRATES: &[&str] = &["masys-platform-nixos", "masys-platform-fallback"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The names in one `[table]` of a manifest.
fn section(manifest: &str, want: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut current = "";
    for line in manifest.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = match header == want {
                true => want,
                false => "",
            };
            continue;
        }
        if current != want || line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `name = ...` or `name.workspace = true`.
        if let Some(name) = line.split(['=', '.']).next() {
            let name = name.trim();
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
    }
    names
}

fn crates() -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(workspace_root().join("crates"))
        .expect("crates/")
        .flatten()
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry.path()))
        .filter(|(_, path)| path.join("Cargo.toml").is_file())
        .collect();
    found.sort();
    found
}

#[test]
fn no_manifest_depends_the_wrong_way() {
    let mut problems = Vec::new();

    for (crate_name, dir) in crates() {
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).expect("a manifest");
        let deps = section(&manifest, "dependencies");

        for (owner, forbidden, why) in FORBIDDEN_DEPS {
            if *owner == crate_name && deps.contains(*forbidden) {
                problems.push(format!("{crate_name}/Cargo.toml depends on `{forbidden}`. {why}"));
            }
        }

        for (owner, exact, why) in EXACT_DEPS {
            if *owner != crate_name {
                continue;
            }
            let allowed: BTreeSet<String> = exact.iter().map(|d| d.to_string()).collect();
            let extra: Vec<&String> = deps.difference(&allowed).collect();
            if !extra.is_empty() {
                problems.push(format!("{crate_name}/Cargo.toml gained {extra:?}. {why}"));
            }
        }

        // Only the binary may name a platform adapter.
        if crate_name != "masys" {
            for platform in PLATFORM_CRATES {
                if deps.contains(*platform) && crate_name != *platform {
                    problems.push(format!(
                        "{crate_name}/Cargo.toml depends on `{platform}`. Only the `masys` binary may \
                         name a platform adapter; everything else talks to PlatformService so it never \
                         has to know which distro this is."
                    ));
                }
            }
        }

        // The domain's suite runs against fakes, which is what keeps the
        // bottom layer independent of the infrastructure beneath it.
        if crate_name == "masys-domain" && !section(&manifest, "dev-dependencies").is_empty() {
            problems.push(
                "masys-domain has dev-dependencies. Its tests run against the fake SystemService/\
                 PlatformService in tests/fake/mod.rs precisely so the domain never depends on the \
                 infrastructure beneath it - anything needing real systemd belongs in masys-systemd/tests/."
                    .to_string(),
            );
        }
    }

    assert!(problems.is_empty(), "the dependency direction is wrong:\n{}", problems.join("\n"));
}
