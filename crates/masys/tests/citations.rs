//! A public doc comment never cites a document the reader cannot open.
//!
//! One did: a `ModalView` variant's rustdoc pointed at `masys-design.md`'s
//! Process row. That file is a working document kept outside the repo on
//! purpose - it is amended whenever a decision moves - so on docs.rs the
//! citation resolved to nothing, and a reader was sent somewhere they
//! could not go to justify a rule that fits in a sentence.
//!
//! **The third species of one defect.** Prose that was true when it was
//! written and is not now: a comment describing what the code used to do,
//! a test name promising more than the test checks, a doc citing a file
//! that no longer ships. None of them fails a build, and all three have
//! been found by reading rather than by running. This one is mechanical
//! enough to check, so it is checked.
//!
//! **Public, not every.** A private doc's citation is addressed to
//! whoever edits that code, who has the working document open beside
//! them; `SMART_INTERVAL_MS` cites the design's cadence rule for exactly
//! that reader and is right to. A `pub` item's doc has an audience that
//! got the crate from crates.io and has nothing else, so a dangling
//! reference there is a promise the published artefact cannot keep.
//! `pub(crate)` counts as private, because rustdoc does not publish it.
//!
//! **Not a link checker.** A URL, a crate, a man page, a `systemd`
//! directive - anything a reader can follow on their own is a reference
//! and stays. The defect is a dangling pointer: a document named the way
//! a file in the repository is named, which is not one.
//!
//! Lives in the `masys` crate for the reason `layering.rs`, `producers.rs`
//! and `identity.rs` do: the composition root is the one place allowed to
//! name every other crate, and so the natural home for a test that reads
//! someone else's source.

mod corpus;

use corpus::{unedited_source, workspace_root};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What makes a word a document rather than prose. Every file the tree
/// ships that a doc comment might name is Markdown; a citation of a
/// source file names a module, which rustdoc can link and this guard has
/// no business policing.
const DOCUMENT_SUFFIX: &str = ".md";

/// Every document that reaches a reader who has the crate.
///
/// **What ships, not what is on this disk**, which is a narrower set and
/// deliberately so. `cargo package` takes the files under a crate's own
/// directory, plus the one `readme` every manifest here points at -
/// `../../README.md`. Nothing else at the workspace root is published,
/// tracked or not.
///
/// Reading the filesystem for `*.md` anywhere under the root was the
/// first version of this and had the defect the guard is about: drop the
/// working document into the repository and every citation of it starts
/// passing, on a machine that has it and for nobody else. The set is
/// built from what a package contains so that being present locally
/// cannot make a citation look followable.
fn shipped_documents(root: &Path) -> BTreeSet<String> {
    let mut documents = BTreeSet::new();
    if root.join("README.md").is_file() {
        documents.insert("README.md".to_string());
    }
    let mut pending = vec![root.join("crates")];
    while let Some(directory) = pending.pop() {
        let entries = std::fs::read_dir(&directory).expect("crates/ is readable");
        for path in entries.filter_map(|entry| entry.ok().map(|entry| entry.path())) {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            match path.is_dir() {
                // `target` is build output wherever it appears, and a
                // dot-directory is local working state.
                true if name != "target" && !name.starts_with('.') => pending.push(path),
                true => {}
                false if name.ends_with(DOCUMENT_SUFFIX) => {
                    documents.insert(name);
                }
                false => {}
            }
        }
    }
    documents
}

/// One document named by one doc comment.
#[derive(Debug)]
struct Citation {
    file: String,
    line: usize,
    document: String,
    /// Whether the item this doc is attached to is published. See the
    /// module doc: it is the reader who cannot follow the pointer that
    /// makes it a defect.
    public: bool,
}

#[test]
fn no_public_doc_cites_a_document_that_does_not_ship() {
    let root = workspace_root();
    let shipped = shipped_documents(&root);
    assert!(
        shipped.contains("README.md"),
        "the tree's own README was not found, so this guard is reading the wrong \
         directory and would call every citation dangling: {shipped:?}"
    );

    let dangling: Vec<String> = crate_sources(&root)
        .iter()
        .flat_map(|(file, source)| citations(file, source))
        .filter(|citation| citation.public && !shipped.contains(&citation.document))
        .map(|citation| {
            format!(
                "{}:{} cites `{}`",
                citation.file, citation.line, citation.document
            )
        })
        .collect();

    assert!(
        dangling.is_empty(),
        "a published doc comment names a document this tree does not ship:\n  {}\n\
         The reader has the crate and nothing else, so the citation resolves to \
         nothing on docs.rs. State the rule where it is needed, or cite something \
         that ships. A working document kept outside the repo may still be cited \
         from a private doc, where the reader is whoever edits the code.",
        dangling.join("\n  ")
    );
}

/// The reader itself, over a source that holds one of each case.
///
/// Without this the test above passes loudest when it has stopped
/// working: a parser that recognises nothing reports a clean tree, and
/// the tree is clean, so nothing would ever say otherwise. Here the
/// answer is known in advance.
#[test]
fn the_reader_tells_a_published_citation_from_a_private_one() {
    const SAMPLE: &str = r#"
//! The crate's own front page, which cites masys-style.md.

/// Cites masys-design.md's Process row.
pub enum Public {}

/// Cites masys-design.md, for whoever edits this.
const PRIVATE: u64 = 1;

/// Cites masys-design.md, and does not publish either.
pub(crate) fn crate_only() {}

/// Links to https://example.invalid/masys-design.md, which a reader can follow.
pub fn linked() {}

/// A struct whose fields carry the citation.
pub struct Fields {
    /// A published field's doc cites masys-design.md too.
    pub named: u64,
    /// A private one cites masys-design.md as well.
    hidden: u64,
}

/// A variant carries no `pub` of its own.
pub enum Variants {
    /// Cites masys-design.md, and publishes with its enum.
    Public,
}

/// An unpublished enum's variants are unpublished too.
pub(crate) enum Hidden {
    /// Cites masys-design.md, for whoever has the source.
    Private,
}

pub mod nested {
    /// An enum inside a module is indented, and still publishes.
    pub enum Inner {
        /// Cites masys-design.md, and publishes with its enum.
        Public,
    }

    /// Past the enum's closing brace, an item is on its own again.
    fn after() {}
}
"#;
    let found = citations("sample.rs", SAMPLE);
    let public: Vec<&str> = found
        .iter()
        .filter(|citation| citation.public)
        .map(|citation| citation.document.as_str())
        .collect();
    let private: Vec<&str> = found
        .iter()
        .filter(|citation| !citation.public)
        .map(|citation| citation.document.as_str())
        .collect();

    // The module doc, the `pub enum`, the published field, the variant
    // and the variant of the enum nested in a module - which is the case
    // a reader keyed on column zero got wrong. A possessive and a
    // trailing stop are stripped, since that is how a citation is
    // actually written in a sentence.
    assert_eq!(
        public,
        vec![
            "masys-style.md",
            "masys-design.md",
            "masys-design.md",
            "masys-design.md",
            "masys-design.md"
        ],
        "{found:#?}"
    );
    // The private constant, the `pub(crate)` function, the private field
    // and the `pub(crate)` enum's variant. The URL is not a citation at
    // all - its reader can follow it.
    assert_eq!(
        private,
        vec![
            "masys-design.md",
            "masys-design.md",
            "masys-design.md",
            "masys-design.md"
        ],
        "{found:#?}"
    );
}

/// Every crate's production source, as `(crate/src/file, the file)`.
///
/// Every member of the workspace rather than the one crate the other
/// guards read: this rule is about what a published doc comment says,
/// and every crate here is published.
fn crate_sources(root: &Path) -> Vec<(String, String)> {
    let mut crates: Vec<PathBuf> = std::fs::read_dir(root.join("crates"))
        .expect("crates/ is readable")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.join("src").is_dir())
        .collect();
    crates.sort();
    assert!(
        crates.len() > 5,
        "{} crates found, which is too few to be right - this guard is reading the \
         wrong directory",
        crates.len()
    );
    crates
        .iter()
        .flat_map(|path| {
            let crate_name = path
                .file_name()
                .expect("a crate directory has a name")
                .to_string_lossy()
                .into_owned();
            unedited_source(&path.join("src"))
                .into_iter()
                .map(move |(file, source)| (format!("{crate_name}/src/{file}"), source))
        })
        .collect()
}

/// Every document one file's doc comments name, with whether that doc
/// publishes.
///
/// Line by line over the unedited source, which is why this reader is not
/// `corpus::checkable`: that one drops every `//` line, and every line
/// this one is about starts with three slashes.
fn citations(file: &str, source: &str) -> Vec<Citation> {
    let lines: Vec<&str> = source.lines().collect();
    let mut citations = Vec::new();
    // The enum whose body we are inside, as `(its indent, whether it
    // publishes)`. Tracked because a variant carries no `pub` of its own
    // and takes its enum's visibility - which this guard learned the
    // expensive way: written without it, it read `ModalView`'s variants
    // as private and passed straight over the one citation it was
    // written for.
    //
    // By indent rather than by column zero, which was the second version
    // of the same mistake: an enum inside a `mod` block is indented, and
    // a reader that only recognised top-level declarations would read
    // its variants as private too. The body ends at the first line no
    // more indented than the declaration, which is its closing brace.
    let mut enclosing_enum: Option<(usize, bool)> = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed_start = line.trim_start();
        let indent = line.len() - trimmed_start.len();
        let is_prose = trimmed_start.is_empty()
            || trimmed_start.starts_with("//")
            || trimmed_start.starts_with("#[");
        if let Some((declared_at, _)) = enclosing_enum
            && !is_prose
            && indent <= declared_at
        {
            enclosing_enum = None;
        }
        if !is_prose && let Some(publishes) = declares_enum(trimmed_start) {
            enclosing_enum = Some((indent, publishes));
        }
        let inside_public_enum = enclosing_enum.is_some_and(|(_, publishes)| publishes);
        // `//!` documents the module, which publishes with the crate.
        // `///` documents whatever follows it.
        let trimmed = line.trim_start();
        let (prose, public) = match trimmed.strip_prefix("//!") {
            Some(prose) => (prose, true),
            None => match trimmed.strip_prefix("///") {
                Some(prose) => (prose, publishes(&lines[index + 1..], inside_public_enum)),
                None => continue,
            },
        };
        citations.extend(
            prose
                .split_whitespace()
                .filter_map(document)
                .map(|document| Citation {
                    file: file.to_string(),
                    line: index + 1,
                    document,
                    public,
                }),
        );
    }
    citations
}

/// Whether this line declares an enum, and whether that enum publishes.
///
/// `pub(crate) enum` declares one that does not, which matters as much as
/// the `pub` case: its variants are as unpublished as it is, and their
/// docs are addressed to whoever has the source.
fn declares_enum(trimmed: &str) -> Option<bool> {
    match trimmed.split_once("enum ")? {
        ("", _) => Some(false),
        ("pub ", _) => Some(true),
        (before, _) if before.starts_with("pub(") && before.ends_with(") ") => Some(false),
        _ => None,
    }
}

/// The document a word names, if it names one.
///
/// Punctuation is stripped from both ends because a citation is written
/// inside a sentence: `(masys-design.md's`, `` `masys-style.md`, `` and a
/// name ending a sentence are all how one actually appears. A trailing
/// stop is safe to take because no file ends in one.
///
/// A URL is not a citation - the reader can follow it - and neither is a
/// bare `.md`.
fn document(word: &str) -> Option<String> {
    const EDGE: &str = "`\"'()[]{}<>,;:!?";
    if word.contains("://") {
        return None;
    }
    let word = word.trim_matches(|c: char| EDGE.contains(c));
    let word = word.trim_end_matches('.');
    let word = word.strip_suffix("'s").unwrap_or(word);
    let name = word.strip_suffix(DOCUMENT_SUFFIX)?;
    (!name.is_empty()).then(|| word.to_string())
}

/// Whether the item a doc comment is attached to publishes.
///
/// The lines after the comment, skipping the rest of the block, its
/// attributes and blank lines - the item is the first thing that is none
/// of those.
///
/// `pub(crate)`, `pub(super)` and `pub(in ...)` are private: rustdoc does
/// not publish them, so their reader is somebody with the source open.
/// An indented item with no `pub` of its own is a variant or a private
/// field, and only the first of those publishes - which is what
/// `inside_public_enum` answers.
fn publishes(rest: &[&str], inside_public_enum: bool) -> bool {
    let Some(item) = rest.iter().find(|line| {
        let trimmed = line.trim_start();
        !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with("#[")
    }) else {
        return false;
    };
    let trimmed = item.trim_start();
    match trimmed.starts_with("pub") {
        true => !trimmed.starts_with("pub("),
        false => item.starts_with(char::is_whitespace) && inside_public_enum,
    }
}
