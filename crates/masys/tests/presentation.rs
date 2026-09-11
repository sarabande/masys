//! `Presentation` stays a record that forces every answer.
//!
//! Fourteen finding kinds used to be described by five exhaustive matches
//! in three crates. They are one match now, and that collapse is only
//! defensible because of a property the collapse itself does not
//! guarantee: **every arm must still be forced to answer everything.**
//!
//! The tree deliberately keeps a five-way cascade elsewhere - `NixOp` is
//! matched exhaustively in five places and stays that way - on the
//! grounds that a table of defaults would let a new operation inherit an
//! answer nobody chose. `Presentation::of` is allowed to be one match
//! instead of five *because it is a record, not a table*: a fifteenth
//! kind cannot compile until every field is filled in, so the compiler
//! still extracts the same six decisions, once rather than five times.
//!
//! Four things would quietly end that, and none of them fails a build:
//!
//! * a `Default` impl, or a `..` in a construction, so a field can be
//!   left out and take a value nobody chose;
//! * a `pub` field, so `Presentation::of` stops being the only door and
//!   a caller can assemble a presentation that disagrees with its
//!   finding;
//! * a third `Option` field, because `Option` is where a default hides
//!   in plain sight - `None` reads as an answer and is usually an
//!   omission;
//! * a wildcard arm, which is the original defect wearing a new hat.
//!
//! Each is one plausible-looking line, added by somebody solving a real
//! problem, and each turns this back into the arrangement the `NixOp`
//! entry rejects. So they are checked.
//!
//! **Why a test rather than a type.** All four are absences, and Rust
//! has no way to say "this struct may not grow a `Default`", or "this
//! field may not become public". What the compiler *does* enforce is
//! the consequence of the third: while the fields are private, no
//! module outside this one can write a `Presentation` literal at all.
//! Keeping them private is the part that needs a test, because turning
//! one `pub` is a one-word change that reads like a convenience and
//! ends the guarantee for the whole workspace.
//!
//! Lives in the `masys` crate for the reason `layering.rs`,
//! `producers.rs`, `identity.rs` and `citations.rs` do: the composition
//! root is the one place allowed to name every other crate, and so the
//! natural home for a test that reads someone else's source.

mod corpus;

use corpus::{checkable, workspace_root};

/// The module the rules below are about.
const DECLARATION: &str = "crates/masys-view/src/presentation.rs";

/// The two fields where absent is a real answer about a finding, rather
/// than a value nobody supplied.
///
/// `label` is `None` where the row has no label column at all, which is
/// different from `Some("")` - the column exists and this row has
/// nothing to put in it - and both are drawn differently. `jump` is
/// `None` where the finding names nothing masys has a row for, which
/// five kinds permanently do.
///
/// A third would need the same argument made in public before it was
/// added, which is what this list is for. Raising the count without one
/// is the change this test exists to make somebody notice.
const OPTIONAL_FIELDS: &[&str] = &["label", "jump"];

fn declaration() -> String {
    let path = workspace_root().join(DECLARATION);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{DECLARATION} is readable; this guard is about that file"));
    // Without this the test passes loudest when it has stopped working:
    // move or rename the module and every assertion below holds over an
    // empty string.
    assert!(
        source.contains("pub struct Presentation"),
        "{DECLARATION} no longer declares `Presentation`, so this guard is \
         checking nothing. Either it moved - in which case move DECLARATION \
         with it - or the collapse was undone, which is a bigger change than \
         this test knows about."
    );
    checkable(&source)
}

/// The text between `open`'s brace and the one that closes it.
///
/// Brace-matched rather than "everything after the header", which is
/// what the wildcard check did first and was wrong: `repeat_count` sits
/// below `Presentation::of` in the same file, so a `match` in *it*
/// would have failed a test about the one above. A guard that fires on
/// innocent code is worse than no guard, because the fix people reach
/// for is to stop running it.
fn braced<'a>(source: &'a str, open: &str) -> &'a str {
    let start = source
        .find(open)
        .unwrap_or_else(|| panic!("{DECLARATION} no longer contains `{open}`"))
        + open.len();
    let mut depth = 1;
    for (at, c) in source[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[start..start + at];
                }
            }
            _ => {}
        }
    }
    panic!("`{open}` is not closed in {DECLARATION}")
}

/// The body of the `Presentation` struct, between its braces.
fn fields() -> String {
    let source = declaration();
    // Past the opening brace, not at it: the declaration itself reads
    // `pub struct`, and a check for `pub ` over a span that includes it
    // fires on every tree there has ever been.
    braced(&source, "pub struct Presentation {").to_string()
}

/// Nothing may be left out of a `Presentation`.
///
/// A `Default` impl or a `..` in a construction is the whole difference
/// between a record and a table: with either, a fifteenth finding kind
/// compiles the moment somebody writes `..Default::default()`, and the
/// six decisions this type exists to force become five, or four, with no
/// error and nothing on screen to notice.
#[test]
fn a_presentation_has_no_default_to_fall_back_on() {
    let source = declaration();
    assert!(
        !source.contains("impl Default for Presentation"),
        "`Presentation` has grown a `Default`. Every field is a decision \
         about how a finding reads, and a default is a decision nobody \
         made being applied to a row an operator will act on."
    );
    assert!(
        !source.contains("Default::default()"),
        "`Presentation` is being built from a default somewhere in \
         {DECLARATION}. Fill every field in the arm, or the arm is not \
         answering the question the collapse exists to force."
    );
    assert!(
        !source.contains("Presentation { .."),
        "a `Presentation` is being built with `..`, which leaves fields to \
         be supplied by something other than the finding they describe."
    );
}

/// `Presentation::of` is the only door.
///
/// Private fields are what make that true, and they are the reason a
/// `Node::Finding` cannot carry a presentation that disagrees with its
/// finding: the variant is writable by hand, but the only value that can
/// go in its second slot is one this module produced.
///
/// **The compiler does the rest of this job, which is why there is only
/// one assertion here.** A private field is reachable from the defining
/// module and its descendants and from nowhere else, so `node.rs` - the
/// one file with a reason to assemble a presentation by hand - already
/// cannot, and neither can any other crate. A test that walked
/// masys-view looking for stray `Presentation {` literals was written
/// and deleted on that ground: it was checking something rustc had
/// already refused, and it would have fired on the day somebody
/// legitimately split this module into a directory.
///
/// What rustc cannot refuse is the field turning `pub`, or `pub(crate)`,
/// which ends the guarantee everywhere at once and reads like a
/// one-word convenience. That is the whole of what is left to check.
#[test]
fn a_presentation_cannot_be_assembled_by_hand() {
    let fields = fields();
    assert!(
        !fields.contains("pub "),
        "a field of `Presentation` is public, so any caller can now build \
         one field by field. `Node::finding` stops being the only way a \
         presentation is produced, and a row can be drawn saying something \
         its finding does not: {fields}"
    );
}

/// `Option` is where a default hides in plain sight.
///
/// A `None` reads like an answer, so a field that should have been
/// required arrives absent and nothing complains - which is the failure
/// this whole collapse was made to remove, in the one shape the type
/// system still permits. Two fields have earned it and the doc on each
/// says why.
#[test]
fn only_the_two_fields_that_earned_it_are_optional() {
    let fields = fields();
    let optional: Vec<&str> = fields
        .split(',')
        .filter(|field| field.contains("Option<"))
        .map(str::trim)
        .collect();
    assert_eq!(
        optional.len(),
        OPTIONAL_FIELDS.len(),
        "`Presentation` has {} optional fields and {} have been argued \
         for. `None` is a real answer for {OPTIONAL_FIELDS:?} and an \
         omission everywhere else: {optional:?}",
        optional.len(),
        OPTIONAL_FIELDS.len()
    );
    for name in OPTIONAL_FIELDS {
        assert!(
            optional.iter().any(|field| field.starts_with(name)),
            "`{name}` is no longer the optional field it was; the list this \
             test checks against is stale: {optional:?}"
        );
    }
}

/// No arm may inherit an answer.
///
/// The wildcard is the original defect in its plainest form: it hands a
/// new kind whatever the catch-all says, which is indistinguishable from
/// a decision and is the one nobody notices being wrong. `section_of`
/// and `jump_target` both carried this argument in their own docs before
/// they were folded in here.
#[test]
fn the_one_match_has_no_wildcard_arm() {
    let source = declaration();
    let body = braced(&source, "pub fn of(kind: &FindingKind) -> Presentation {");
    assert!(
        !body.contains("_ =>"),
        "`Presentation::of` has a wildcard arm. A fifteenth finding kind \
         now compiles without anybody deciding what it looks like, where \
         it files, or where `.` takes you from it."
    );
}
