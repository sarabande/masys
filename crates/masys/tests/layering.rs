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
//! Manifests, and one file. A companion check that grepped source for
//! `masys_systemd::` in masys-app would never fire: naming a crate that
//! is not a dependency does not compile, so rustc has already refused it
//! long before a test could run. The half worth checking is the
//! manifest, because *that* edit compiles fine on its own and is what
//! quietly makes the other half legal.
//!
//! That reasoning has exactly one gap, and `FORBIDDEN_FILE_PATHS` below
//! covers it: inside the composition root, which legitimately depends on
//! every other crate, there is no manifest edit to catch. `masys_app::`
//! in `crates/masys/src/platform.rs` compiles perfectly well, so the
//! source is the only place that rule can live.
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

mod corpus;

use corpus::workspace_root;
use std::collections::BTreeSet;
use std::path::PathBuf;

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
    (
        "masys-app",
        "crossterm",
        "No layer above the composition root names a terminal library.",
    ),
    (
        "masys-app",
        "zbus",
        "D-Bus types never appear above masys-systemd.",
    ),
    (
        "masys-render",
        "masys-app",
        "The renderer draws a View. It never reaches back into the session.",
    ),
    (
        "masys-render",
        "masys-systemd",
        "The renderer draws a View, not a machine.",
    ),
    (
        "masys-systemd",
        "masys-app",
        "An adapter is below the session, not beside it.",
    ),
    (
        "masys-systemd",
        "masys-view",
        "An adapter produces domain types; the row model is above it.",
    ),
    // masys-scan was constrained by nothing until 2026-08-26 - the only
    // library crate absent from every table here, free to depend on
    // ratatui or the session and have no check notice. It is an adapter
    // like masys-systemd, implementing DirScanner, and gets the same
    // prohibitions for the same reason.
    (
        "masys-scan",
        "masys-app",
        "An adapter is below the session, not beside it.",
    ),
    (
        "masys-scan",
        "masys-view",
        "An adapter produces domain types; the row model is above it.",
    ),
    (
        "masys-scan",
        "masys-systemd",
        "Two adapters implementing two ports. Neither reaches through the other.",
    ),
    (
        "masys-scan",
        "ratatui",
        "masys-scan walks a filesystem and has no idea it is being drawn.",
    ),
    (
        "masys-scan",
        "crossterm",
        "masys-scan walks a filesystem and has no idea it is being drawn.",
    ),
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
/// Distro specifics are quarantined in that crate precisely so nothing
/// above it has to know which distro this is - the whole reason
/// `PlatformService` exists rather than an `if nixos` in the app.
///
/// One entry, not two: the adapter for a host nothing recognises reads
/// nothing and so quarantines nothing. It lives in the composition root
/// beside `NoScanner`, and `FORBIDDEN_FILE_PATHS` keeps the constraint it
/// had as a crate.
const PLATFORM_CRATES: &[&str] = &["masys-platform-nixos"];

/// Source files that may not name a crate, keyed by path from the
/// workspace root.
///
/// The one place a source check earns its keep - see the module doc. A
/// file listed here sits inside a crate that is allowed to name what the
/// rule forbids, so no manifest edit betrays the violation and rustc is
/// perfectly happy with it.
///
/// `platform.rs` holds `FallbackPlatform` and `NoScanner`, the two null
/// adapters. `FallbackPlatform` had a crate of its own until 2026-08-26,
/// and a crate boundary that forbade exactly this list; the rule follows
/// the code rather than being dropped with the crate.
const FORBIDDEN_FILE_PATHS: &[(&str, &[&str], &str)] = &[(
    "crates/masys/src/platform.rs",
    &["ratatui", "crossterm", "zbus", "masys_view", "masys_app"],
    "A null adapter implements a masys-domain port and names nothing above it. This file \
     keeps the constraint masys-platform-fallback had as a crate.",
)];

/// Whether `body` names `krate::`, ignoring longer identifiers that merely
/// end with it - `not_masys_app::` is a different crate and not a
/// violation.
fn names(body: &str, krate: &str) -> bool {
    let needle = format!("{krate}::");
    body.match_indices(&needle).any(|(at, _)| {
        at == 0
            || !body[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
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
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
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
                problems.push(format!(
                    "{crate_name}/Cargo.toml depends on `{forbidden}`. {why}"
                ));
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

    assert!(
        problems.is_empty(),
        "the dependency direction is wrong:\n{}",
        problems.join("\n")
    );
}

/// The gap the manifest check cannot see, because inside the composition
/// root there is no manifest edit to catch.
///
/// A missing file fails rather than passing quietly: a rule whose target
/// has been renamed out from under it enforces nothing, and would go on
/// reporting success forever.
#[test]
fn no_source_file_names_what_its_path_forbids() {
    let mut problems = Vec::new();

    for (rel, forbidden, why) in FORBIDDEN_FILE_PATHS {
        let path = workspace_root().join(rel);
        let Ok(source) = std::fs::read_to_string(&path) else {
            problems.push(format!(
                "{rel} is missing, and a rule names it. Update FORBIDDEN_FILE_PATHS."
            ));
            continue;
        };
        let body = corpus::checkable(&source);
        for krate in *forbidden {
            if names(&body, krate) {
                problems.push(format!("{rel} names `{krate}::`. {why}"));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "a file names a crate its path forbids:\n{}",
        problems.join("\n")
    );
}
