//! The Packages buffer.

mod fake;

use fake::{FakePlatformService, FakeSystemService, NoScanner, bare_snapshot};
use masys_app::buffer::{Buffer, BufferGates, Registry};
use masys_app::keymap::Keymap;
use masys_app::packages_buffer::PackagesBuffer;
use masys_app::{App, Key};
use masys_domain::error::MasysError;
use masys_domain::platform::Package;
use masys_domain::service::PackageService;
use masys_view::{Node, SectionKind};

struct Listing(Vec<Package>);

impl PackageService for Listing {
    fn packages(&self) -> Result<Vec<Package>, MasysError> {
        Ok(self.0.clone())
    }
}

struct Refuses;

impl PackageService for Refuses {
    fn packages(&self) -> Result<Vec<Package>, MasysError> {
        Err(MasysError::Platform("no".to_string()))
    }
}

fn package(name: &str, version: &str) -> Package {
    Package {
        name: name.to_string(),
        version: version.to_string(),
    }
}

/// A header carrying the count, then one row per package.
#[test]
fn the_buffer_is_a_section_and_a_row_for_each_package() {
    let mut buffer = PackagesBuffer::default();
    buffer.refresh(&Listing(vec![
        package("git", "2.55.0"),
        package("curl", "8.9"),
    ]));

    let rows = buffer.rows();
    assert!(matches!(
        &rows[0],
        Node::SectionHeader {
            kind: SectionKind::Packages,
            count: Some(2),
            ..
        }
    ));
    assert_eq!(rows.len(), 3, "a header and two packages: {rows:#?}");
    assert!(matches!(&rows[1], Node::Package(p) if p.name == "git"));
}

/// **No read yet is not an empty host.** Before the first refresh the
/// buffer contributes no rows at all, rather than a header reading
/// `Packages 0` - which would be a count nobody measured, and the claim
/// the whole port was reshaped to avoid.
#[test]
fn an_unread_buffer_shows_nothing_rather_than_zero() {
    assert!(PackagesBuffer::default().rows().is_empty());
}

/// A host that genuinely has none says so, which is a different row from
/// saying nothing.
#[test]
fn a_host_with_no_packages_shows_a_header_saying_zero() {
    let mut buffer = PackagesBuffer::default();
    buffer.refresh(&Listing(Vec::new()));
    assert!(matches!(
        buffer.rows().as_slice(),
        [Node::SectionHeader { count: Some(0), .. }]
    ));
}

/// Keep-last-good: a read that fails leaves the previous list standing.
///
/// The rule `NixBuffer::refresh` applies to all nine of its readings. The
/// alternative makes a transient failure look like every package being
/// uninstalled at once.
#[test]
fn a_failed_read_keeps_the_last_good_list() {
    let mut buffer = PackagesBuffer::default();
    buffer.refresh(&Listing(vec![package("git", "2.55.0")]));
    buffer.refresh(&Refuses);

    assert!(
        matches!(buffer.rows().as_slice(), [_, Node::Package(p)] if p.name == "git"),
        "the list from before the failure survives: {:#?}",
        buffer.rows()
    );
}

/// The *first* read reports its failure, where the tick underneath does
/// not.
///
/// `refresh` returning a `Result` that was structurally always `Ok` was
/// the defect this splits: the `?` at its call site was dead, so a host
/// whose package database could not be read showed an empty buffer and
/// said nothing. Opening the buffer is a question the operator asked, so
/// its answer - including "I could not find out" - belongs in the echo
/// line.
#[test]
fn opening_the_buffer_reports_a_read_that_failed() {
    let mut buffer = PackagesBuffer::default();
    assert!(
        buffer.open(&Refuses).is_err(),
        "a refusal on the first read has to reach the operator"
    );
    assert!(
        buffer.rows().is_empty(),
        "and nothing is claimed about the host: {:#?}",
        buffer.rows()
    );
}

/// A failed re-open keeps the list it already had.
///
/// Unlike `LogBuffer::open`, which clears: that one changes scope, so a
/// failed fetch would show the previous unit's lines under the new
/// unit's name. A package list is the same host either way, so the last
/// good answer is still the best one available - and the error is
/// reported alongside it.
#[test]
fn a_failed_reopen_keeps_the_last_good_list() {
    let mut buffer = PackagesBuffer::default();
    buffer
        .open(&Listing(vec![package("git", "2.55.0")]))
        .expect("the first read");
    assert!(buffer.open(&Refuses).is_err(), "reported");
    assert!(
        matches!(buffer.rows().as_slice(), [_, Node::Package(p)] if p.name == "git"),
        "and the list survives: {:#?}",
        buffer.rows()
    );
}

/// A session on a host whose platform can list packages.
fn host_with(packages: Vec<Package>) -> App {
    let system = FakeSystemService::returning(vec![bare_snapshot()]);
    let mut app = App::with_keymap(
        Box::new(system),
        Box::new(FakePlatformService::default()),
        Box::new(NoScanner),
        "debian".to_string(),
        Keymap::for_registry(Registry::new(BufferGates {
            declarative: false,
            packages: true,
        })),
    );
    app.set_package_service(Some(Box::new(Listing(packages))));
    app.tick(1_000, "t".to_string());
    app
}

/// **`6` opens Packages, and the buffer has rows in it.**
///
/// The test the whole feature was missing. Every other test here reaches
/// `PackagesBuffer` directly, and the registry tests ask `by_key`, which
/// nothing in the binary calls - so the buffer was built, gated, rendered
/// and covered while no key in the default bindings opened it. Coverage
/// of a shape is not coverage of a path to it, and this is the path.
#[test]
fn six_opens_the_packages_buffer_and_it_has_rows() {
    let mut app = host_with(vec![package("git", "2.55.0"), package("curl", "8.9")]);

    app.handle_key(Key::char('6'));
    assert_eq!(app.buffer(), Buffer::Packages, "6 must open Packages");

    let rows = app.view().rows;
    assert!(
        rows.iter().any(|row| matches!(
            row,
            Node::SectionHeader {
                kind: SectionKind::Packages,
                count: Some(2),
                ..
            }
        )),
        "the section header reaches the view: {rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| matches!(row, Node::Package(p) if p.name == "git")),
        "and so do the packages: {rows:#?}"
    );
}

/// Opening it is what reads it, so the rows are there on the first frame.
///
/// The buffer is read only while it is open (the tick skips it
/// otherwise), so without the read in `open` the operator would press
/// `6`, see an empty screen, and get rows only when the next tick
/// happened to land.
#[test]
fn the_buffer_is_populated_by_the_keypress_rather_than_the_next_tick() {
    let mut app = host_with(vec![package("git", "2.55.0")]);

    app.handle_key(Key::char('6'));
    assert!(
        app.view()
            .rows
            .iter()
            .any(|row| matches!(row, Node::Package(_))),
        "no tick has run since the keypress: {:#?}",
        app.view().rows
    );
}
