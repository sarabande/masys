//! Every `SectionKind` is built by somebody.
//!
//! A variant with no producer is dead weight, and this tree has grown one
//! four times: a sparkline nothing filled, an `Input` picker nothing
//! constructed, and two `SectionKind` variants - `Kernel` and
//! `RecentErrors` - that nothing ever built. `SectionKind::NixUnits` was
//! deleted for the same reason earlier still, came back with a producer,
//! and the commit that removed it did not re-check its siblings. That is
//! the gap this test closes.
//!
//! **Nothing warns about it.** A `pub` item in a library crate is
//! reachable by definition, so dead-code analysis has no opinion, and a
//! test that constructs the variant makes it genuinely reachable - the
//! picker had three passing tests over a value no caller ever built.
//! Coverage of a shape is not coverage of a path to it.
//!
//! **Construction sites, not uses.** Counting uses reads as healthy for a
//! dead variant, because `folds`, `fold_key`, match arms, imports and doc
//! comments all name a variant without producing one. That is precisely
//! why `Kernel` looked alive: it was named in a test comment claiming the
//! Logs buffer keyed on it, which it never did.
//!
//! Lives in the `masys` crate for the reason `layering.rs` does: the
//! composition root is the one place allowed to name every other crate,
//! and so the natural home for a test that reads someone else's source.
//!
//! # Why this checks `SectionKind` and not `Node`
//!
//! The check works here because of a property that happens to hold:
//! inside `masys-app`, every occurrence of `SectionKind::` is a
//! construction. The two methods that merely *name* variants - `folds`
//! and `fold_key` - live in `masys-view`, outside the corpus, so there is
//! no pattern position to tell apart from an expression one and no
//! heuristic to get wrong.
//!
//! `Node` has no such property: `app.rs` matches on `Node::Spacer`,
//! `Node::SectionHeader` and others constantly, so the same test would
//! have to distinguish a pattern from a constructor textually and would
//! be wrong in both directions. Issue #3 is about that surface and wants
//! exhaustive methods on the type rather than a test reading source.
//!
//! No dependencies, and the source is read line by line - enough for the
//! question being asked, and it keeps a test about the tree from adding
//! to it.

mod corpus;

use corpus::workspace_root;
use std::collections::BTreeSet;

/// The crate whose buffers build sections. Every `SectionKind` in masys
/// is constructed under here; `masys-view` declares the enum and
/// `masys-render` only ever receives one already built.
const PRODUCERS: &str = "crates/masys-app/src";

/// Where the enum is declared.
const DECLARATION: &str = "crates/masys-view/src/node.rs";

#[test]
fn every_section_kind_has_a_producer() {
    let root = workspace_root();
    let declared = variants_of(
        &std::fs::read_to_string(root.join(DECLARATION)).expect("node.rs is readable"),
        "SectionKind",
    );
    assert!(
        declared.len() > 5,
        "the enum parsed as {declared:?}, which is too few to be right - \
         the parser has drifted from node.rs rather than the enum shrinking"
    );

    let corpus = corpus::production_text(&root.join(PRODUCERS));
    let orphans: Vec<&String> = declared
        .iter()
        .filter(|variant| !corpus.contains(&format!("SectionKind::{variant}")))
        .collect();

    assert!(
        orphans.is_empty(),
        "declared in {DECLARATION} and built nowhere in {PRODUCERS}: {orphans:?}\n\
         A variant with no producer is dead weight. Either build it where the \
         section is assembled, or delete it - and if it is a deliberate \
         placeholder, that is the one case this test is meant to argue with."
    );
}

/// Every variant name of one `pub enum`, by its declaration order.
///
/// Payload variants come back by name alone: `Generations(ProfileKind)`
/// is `Generations`, which is what a construction site spells before its
/// argument.
fn variants_of(source: &str, name: &str) -> BTreeSet<String> {
    source
        .lines()
        .skip_while(|line| !line.starts_with(&format!("pub enum {name}")))
        .skip(1)
        .take_while(|line| !line.starts_with('}'))
        .filter_map(|line| {
            let token = line.trim();
            let head: String = token
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            // Doc comments, attributes and blank lines all fail this: a
            // variant is the only thing in an enum body that starts with
            // an uppercase letter and runs to a `(`, `{` or `,`.
            match head.chars().next().is_some_and(char::is_uppercase) {
                true => Some(head),
                false => None,
            }
        })
        .collect()
}
