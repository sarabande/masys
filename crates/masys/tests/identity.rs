//! A title is what the header prints, and never what state is filed under.
//!
//! `Buffer::title()` used to be an identity. `App` filed its cursors and
//! its filters under it and `Keymap` filed its per-buffer overlays under
//! it, so a display name decided which state belonged to which buffer -
//! and rewording a heading was a change that could move a cursor or hand
//! one buffer another's key overrides. Both were rekeyed onto `Buffer`
//! itself, which is the thing that actually has an identity.
//!
//! Nothing in the compiler prevents the next one. `title()` returns a
//! `&'static str`, which is a perfectly good `HashMap` key, so the third
//! map keyed on a heading would compile and pass every other test in the
//! tree - exactly as the first two did. It is caught here or not at all.
//!
//! Two assertions, because one of them alone was not enough. The first
//! is the rule: a title may not reach a map operation. The second is a
//! head count, and it is the cruder of the two on purpose - the rule
//! only inspects call sites it can recognise, so the count is what
//! notices a caller written in a shape the rule has no pattern for. A
//! second *legitimate* display use would trip it, and that is the
//! intended outcome: come here, say which one it is, and raise the
//! number with the reason attached.
//!
//! Lives in the `masys` crate for the reason `layering.rs` and
//! `producers.rs` do: the composition root is the one place allowed to
//! name every other crate, which makes it the natural home for a test
//! that reads someone else's source.

mod corpus;

use corpus::workspace_root;
use std::path::Path;

/// Where a `Buffer` is held and its keys resolved. Both maps that had to
/// be rekeyed were under here, and `Buffer` is not constructed outside
/// it.
const CORPUS: &str = "crates/masys-app/src";

/// How many calls to `title()` the production tree is expected to hold,
/// and where the one is.
const EXPECTED_CALLERS: usize = 1;
const THE_CALLER: &str = "the \"<name> only\" heading in the key help (keymap.rs)";

/// What a map operation looks like at a call site. A title reaching one
/// of these is a title being used as a key.
const LOOKUPS: &[&str] = &[
    ".entry(",
    ".get(",
    ".get_mut(",
    ".insert(",
    ".remove(",
    ".contains_key(",
];

#[test]
fn a_title_never_keys_anything() {
    let callers = callers_of_title(&workspace_root().join(CORPUS));

    // Without this the test passes loudest when it has stopped working:
    // rename the method and every assertion below holds vacuously.
    assert!(
        !callers.is_empty(),
        "no call to .title() anywhere in {CORPUS}, so this test is checking \
         nothing. Either the method was renamed - in which case rename it \
         here too - or the header stopped printing a buffer's name, which \
         is a bigger change than this test knows about."
    );

    let keyed: Vec<&Caller> = callers.iter().filter(|caller| caller.keys).collect();
    assert!(
        keyed.is_empty(),
        "a buffer's title is being used as a key:\n{}\n\n\
         A title is a display name, so filing state under it means a \
         reworded heading moves state that has nothing to do with the \
         wording. Key on `Buffer`, which derives `Hash` for this reason.",
        listing(&keyed)
    );

    assert_eq!(
        callers.len(),
        EXPECTED_CALLERS,
        "expected {EXPECTED_CALLERS} call to .title() - {THE_CALLER} - and found \
         {}:\n{}\n\n\
         The count is here to catch a title-keyed lookup written in a shape \
         the rule above has no pattern for. If the new caller really is \
         printing a name, raise {EXPECTED_CALLERS} and say which one it is.",
        callers.len(),
        listing(&callers.iter().collect::<Vec<_>>())
    );
}

/// One call to `title()`: where it is, what it reads like, and whether
/// it is being used to look something up.
struct Caller {
    file: String,
    context: String,
    keys: bool,
}

fn listing(callers: &[&Caller]) -> String {
    callers
        .iter()
        .map(|caller| format!("  {}: {}", caller.file, caller.context))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every call to `title()` in the production source under `dir`.
///
/// Each file is read with its runs of whitespace flattened to single
/// spaces before anything is looked for, so that a call split over
/// several lines reads the same as one that fits on one. Matching line
/// by line was the first version of this, and `masys-style.md` already
/// records what that costs: a string replacement matched nothing because
/// rustfmt had reflowed the target. `.entry(\n    buffer.title(),\n)` is
/// exactly that shape, and it would have walked past.
///
/// Whether a call is a lookup is decided by what precedes it back to the
/// nearest `;`, `{` or `}` - one statement's worth of context, which is
/// as far as an argument can be from the call it is an argument to.
fn callers_of_title(dir: &Path) -> Vec<Caller> {
    corpus::production_source(dir)
        .iter()
        .flat_map(|(file, source)| {
            source.match_indices(".title()").map(move |(at, _)| {
                let statement = &source[..at];
                let statement = match statement.rfind([';', '{', '}']) {
                    Some(end) => &statement[end + 1..],
                    None => statement,
                };
                Caller {
                    file: file.clone(),
                    context: trimmed_tail(statement),
                    keys: LOOKUPS.iter().any(|op| statement.contains(op)),
                }
            })
        })
        .collect()
}

/// The last of a statement, for a failure message to quote. Enough to
/// recognise the line by, and not so much that a long chain fills the
/// screen.
fn trimmed_tail(statement: &str) -> String {
    const WIDTH: usize = 90;
    let trimmed = statement.trim();
    match trimmed.char_indices().nth_back(WIDTH) {
        Some((at, _)) => format!("...{}.title()", &trimmed[at..]),
        None => format!("{trimmed}.title()"),
    }
}
