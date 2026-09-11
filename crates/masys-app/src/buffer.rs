//! Which buffers *this host* has.
//!
//! The catalogue - which buffers masys can build, their digit and their
//! name - is `masys_view::buffer`. This is the half that varies by host,
//! and it is the half that has to: the Nix buffer must be **absent** on a
//! host with no declarative service, not present and empty. A digit that
//! opens a buffer explaining that NixOS was not found is worse than a
//! digit that does nothing.
//!
//! **`Registry::new` and `Buffer::title` are the only readers of
//! `BUFFERS` outside tests** - `grep -rn BUFFERS crates/*/src` says so in
//! one line, and that grep is the invariant. Everything that answers
//! "which buffers does this host have" - the digit, the footer, the `?`
//! help - goes through a `Registry`. It is stated as a grep because the
//! version of this change that left `keymap.rs` reading `BUFFERS`
//! directly gave the question two owners, and they disagreed exactly
//! where it is worst: the footer of a Debian host printed `[5] nix` for a
//! digit that did nothing.

pub use masys_view::buffer::{BUFFERS, Buffer, BufferSpec};

/// Which buffers this host has.
///
/// `BUFFERS` was the whole answer while every host had the same buffers. The
/// Nix buffer is the first that some hosts do not: it must be *absent* on a
/// host with no declarative service, which is a stronger claim than empty
/// - no digit, no footer entry, no line in the `?` help.
#[derive(Debug, Clone)]
pub struct Registry {
    specs: Vec<BufferSpec>,
}

/// Which of the optional buffers a host has.
///
/// A struct rather than the two positional `bool`s this used to be. Those
/// read as `Registry::new(true, false)` at twenty-nine call sites, which
/// says nothing about which gate is which and silently grows a third
/// unlabelled `bool` the next time a buffer is gated. The fields put the
/// answer where the reader already is.
///
/// `bool`s rather than the services themselves: this crate's question is
/// "is there one", and taking the ports would make the registry depend on
/// two traits it never calls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BufferGates {
    /// Whether a `DeclarativeService` was constructed for this host, which
    /// is what the Nix buffer needs.
    pub declarative: bool,
    /// Whether a `PackageService` was, which is what Packages needs.
    pub packages: bool,
}

impl BufferGates {
    /// The host that has every buffer.
    ///
    /// What the table guards iterate. Shadowing and binding agreement are
    /// properties of the tables, which are the same wherever masys runs,
    /// so a guard that ran against a host missing a buffer would be a
    /// guard that stops checking it - which is exactly what happened to
    /// Packages between its arrival and 2026-08-30.
    pub const ALL: Self = Self {
        declarative: true,
        packages: true,
    };

    /// The host that has none of them: no declarative service, no package
    /// service. What the fallback adapter's host looks like.
    pub const NONE: Self = Self {
        declarative: false,
        packages: false,
    };
}

impl Registry {
    /// The buffers this host has.
    pub fn new(gates: BufferGates) -> Self {
        let specs = BUFFERS
            .iter()
            .copied()
            .filter(|spec| gates.declarative || spec.buffer != Buffer::Nix)
            .filter(|spec| gates.packages || spec.buffer != Buffer::Packages)
            .collect();
        Self { specs }
    }

    pub fn contains(&self, buffer: Buffer) -> bool {
        self.specs.iter().any(|spec| spec.buffer == buffer)
    }

    /// The buffer this host's catalogue puts on `key`.
    ///
    /// The catalogue's digit, *not* the binding: an override moves the
    /// binding and leaves this untouched, so this cannot answer "which key
    /// opens io" once the operator has had a say. `Keymap::resolve` is the
    /// only lookup that can, and it is the one `App::handle_key` calls.
    /// Used by the tests that state which buffers a host has at all;
    /// nothing in the binary calls it.
    pub fn by_key(&self, key: char) -> Option<Buffer> {
        self.specs
            .iter()
            .find(|spec| spec.key == Some(key))
            .map(|spec| spec.buffer)
    }

    pub fn specs(&self) -> &[BufferSpec] {
        &self.specs
    }
}

impl Default for Registry {
    /// Without a declarative service and without packages - the common
    /// case, and the safe one in both directions: a default that included
    /// the Nix buffer would put it on Debian, and one that included
    /// Packages would put it on a host whose platform cannot list any.
    fn default() -> Self {
        Self::new(BufferGates::NONE)
    }
}
