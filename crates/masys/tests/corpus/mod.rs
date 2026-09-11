#![allow(dead_code)]
// Each guard compiles this module into its own test binary and uses a
// different part of it, the same reason `masys-app/tests/fake` carries
// the allow.

//! One reader of the production corpus, for the guards that check it.
//!
//! Four guards ask four different questions of the same text: does a
//! crate name one it may not, does a producing crate build every variant
//! it declares, is a display string being used as a key, does a public
//! doc comment cite a document that does not ship. The first three each
//! brought its own copy of the reading, and the copies had diverged.
//!
//! They diverged unevenly, which is worth being exact about.
//! `workspace_root` was byte-identical in all three. The two that walk a
//! directory - `producers.rs` and `identity.rs` - differed in whether
//! they recursed, and only `identity.rs` did. Only `identity.rs`
//! flattened whitespace, which is the reader `masys-style.md` records a
//! failure against having learned the hard way: a call rustfmt had
//! wrapped walked straight past a line-matching search.  `layering.rs`
//! never walked a directory at all - it reads two named files - so
//! recursion was never a gap it had, and it shares only the exclusion
//! rule and the root.
//!
//! Deliberately free of dependencies. `producers.rs` explains why at
//! length and it applies to all three: a guard that needs a crate to
//! build is a guard that stops running the day that crate does not.

use std::path::{Path, PathBuf};

/// The workspace root, from this crate's manifest directory.
///
/// Byte-identical in all three guards before it was shared, which is the
/// least interesting of the duplications here and the most obviously one.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// A file's production code, with its runs of whitespace flattened to
/// single spaces.
///
/// Two exclusions, both load-bearing, and the same two all three guards
/// made separately. Everything from the first `#[cfg(test)]` onwards is
/// cut away, and line comments are dropped: the rules these guards
/// enforce get *discussed* in prose constantly - `layering.rs` names
/// `masys_app` a dozen times in order to forbid it - so a comment must
/// not trip a check that is about code. Block comments are not used in
/// this tree.
///
/// **Flattened, which is the half two of the three were missing.** A call
/// split over several lines has to read the same as one that fits on one,
/// or the guard walks past exactly the constructs rustfmt reflowed.
/// `masys-style.md` records the cost of learning that the other way: a
/// string replacement matched nothing because the target had been
/// wrapped, and `.entry(\n    buffer.title(),\n)` is that shape.
pub fn checkable(source: &str) -> String {
    source
        .lines()
        .take_while(|line| !line.trim_start().starts_with("#[cfg(test)]"))
        .filter(|line| !line.trim_start().starts_with("//"))
        .flat_map(|line| line.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The same corpus as one text, for a guard that only asks whether the
/// crate contains something anywhere.
///
/// `producers.rs` is that guard - a variant is built somewhere in the
/// producing crate or it is not - and has no use for which file it was
/// in. Here rather than there, so that the three shapes of the same read
/// sit together.
pub fn production_text(dir: &Path) -> String {
    production_source(dir)
        .into_iter()
        .map(|(_, source)| source)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every production source file under `dir` and below it, as
/// `(file name, checkable text)`, in a stable order.
///
/// Recursive, which is the other half two of the three were missing:
/// `masys-app/src/proc/` and its neighbours hold real code, and a reader
/// that only listed the top level would report a clean tree because it
/// never looked.
///
/// This description sat above `production_text` until the raw reader
/// below was split out, which is a thing worth naming: two doc blocks
/// with no blank line between them are one comment on whatever follows,
/// so it read as `production_text`'s second half and this function had
/// no doc at all.
pub fn production_source(dir: &Path) -> Vec<(String, String)> {
    unedited_source(dir)
        .into_iter()
        .map(|(name, source)| (name, checkable(&source)))
        .collect()
}

/// Every production source file under `dir` and below it, as
/// `(file name, the file)`, in a stable order and exactly as written.
///
/// Named for what separates it from its two neighbours, which is the
/// only thing a caller has to choose between: this one is the source,
/// and the other two have been through [`checkable`]. The same walk
/// without that pass over it,
/// for the one guard that is *about* the comments the others drop:
/// `citations.rs` asks whether a doc comment names a document that does
/// not ship, and a reader that strips `//` lines would find nothing to
/// ask about and pass forever.
///
/// An empty corpus fails rather than passing quietly. A guard whose
/// target has been moved out from under it enforces nothing and goes on
/// reporting success forever, which is worse than the violation it was
/// watching for.
pub fn unedited_source(dir: &Path) -> Vec<(String, String)> {
    let mut sources: Vec<(String, String)> = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&directory)
            .expect("the crate's source is readable")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().is_some_and(|ext| ext == "rs") {
                let name = path
                    .file_name()
                    .expect("a source file has a name")
                    .to_string_lossy()
                    .into_owned();
                let source = std::fs::read_to_string(&path).expect("a source file is readable");
                sources.push((name, source));
            }
        }
    }
    sources.sort();
    assert!(
        !sources.is_empty(),
        "{} holds no source, so this guard would pass vacuously",
        dir.display()
    );
    sources
}
