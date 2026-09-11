//! The Packages buffer: what this host has installed.
//!
//! The thinnest buffer masys has, and deliberately. Every other buffer
//! reconciles several readings - the Nix buffer holds nine - where this
//! one holds a list and a section header over it. There is no second
//! reading to disagree with, no per-row state to fold, and nothing here
//! that acts: the design's own Packages row is "platform-owned", and every
//! operation that changes what is installed belongs to the package manager
//! rather than to a system monitor.
//!
//! **A different thing from what the design's buffer table meant.** That
//! table describes Packages as "platform-owned; NixOS generations, diff,
//! GC", which is the Nix buffer, built later and not in that table at all.
//! Building the table's version would be a second *buffer* onto the
//! generations - two buffers answering one question, which is the defect
//! this tree has spent its recent history removing. So this is the reading
//! that was left over once the Nix buffer took the rest: the installed
//! list, which nothing else in masys shows.
//!
//! (A `View` is a picture and a buffer is a place. This paragraph had the
//! two the wrong way round until 2026-08-30.)

use crate::nix_buffer::keep_last_good;
use masys_domain::error::MasysError;
use masys_domain::platform::Package;
use masys_domain::service::PackageService;
use masys_view::{Node, SectionKind};

/// The installed list, kept across ticks.
///
/// `Option` for the reason every reading in `NixBuffer` is one: `None` is
/// **no read has succeeded yet**, and an empty `Vec` is a host that
/// genuinely has no packages. Rendering the first as the second would put
/// "0 packages" on screen on the strength of a read that failed, which is
/// the claim this whole port was reshaped to avoid.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PackagesBuffer {
    pub packages: Option<Vec<Package>>,
}

impl PackagesBuffer {
    /// The first read, whose failure the operator is told about.
    ///
    /// Split from `refresh` the way `LogBuffer::open` is split from
    /// `LogBuffer::refresh`, and for its reason: the operator pressed a
    /// key to get here, so a read that fails now is an answer to
    /// something they just asked and belongs in the echo line. A failure
    /// on the *tick* underneath them is not, and is kept quiet below.
    ///
    /// Unlike the log buffer this keeps its last good list rather than
    /// clearing: there is no scope change here to mislead anyone. The
    /// log clears because a failed fetch would otherwise show the
    /// previous unit's lines under the new unit's name, and a package
    /// list has no equivalent - it is the same host either way.
    pub fn open(&mut self, packages: &dyn PackageService) -> Result<(), MasysError> {
        let read = packages.packages()?;
        self.packages = Some(read);
        Ok(())
    }

    /// Re-reads the installed list, keeping the last good one on failure.
    ///
    /// Keep-last-good rather than clearing, the rule `NixBuffer::refresh`
    /// applies to all nine of its readings: a list that was true two
    /// seconds ago is a better answer than an empty screen, and the
    /// alternative makes a transient read error look like an uninstall of
    /// everything.
    ///
    /// Returns nothing, like every other `refresh` in masys. It used to
    /// return a `Result` that was structurally always `Ok`, which made
    /// the `?` at its one call site dead and read, to anyone skimming,
    /// as though a failed tick reached the operator. It did not.
    pub fn refresh(&mut self, packages: &dyn PackageService) {
        keep_last_good(&mut self.packages, packages.packages().map(Some));
    }

    /// One section, one row per package.
    ///
    /// The count goes in the header rather than on a line of its own,
    /// which is where every other section in masys puts it.
    pub fn rows(&self) -> Vec<Node> {
        let Some(packages) = self.packages.as_ref() else {
            return Vec::new();
        };
        let mut rows = vec![Node::SectionHeader {
            title: "Packages".to_string(),
            kind: SectionKind::Packages,
            count: Some(packages.len() as u32),
        }];
        rows.extend(packages.iter().cloned().map(Node::Package));
        rows
    }
}
