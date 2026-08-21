//! Which buffers exist, and how they are reached.
//!
//! The design calls for a registry rather than wagit's closed
//! `outline::Context` enum, because masys has five buffers in v1 and the
//! platform adapter contributes another. `BUFFERS` is the *catalogue* - a
//! data table, so adding a buffer is adding a row rather than editing a
//! match arm in several files - and `Registry` is what one *host* has.
//!
//! This file used to say the registry would stay a static slice "until
//! something actually contributes a buffer at runtime; making it dynamic
//! before then would be structure with no second case to justify it". The
//! Nix view is that second case, and it is the reason `Registry` exists:
//! on a host with no declarative service the view must be **absent**, not
//! present and empty. A digit that opens a screen explaining that NixOS
//! was not found is worse than a digit that does nothing.
//!
//! **`Registry::new` and `Buffer::title` are the only readers of `BUFFERS`
//! outside tests** - `grep -rn BUFFERS crates/*/src` says so in one line,
//! and that grep is the invariant. Everything that answers "which views
//! does this host have" - the digit, the footer, the `?` help - goes
//! through a `Registry`. It is stated as a grep because the version of
//! this change that left `keymap.rs` reading `BUFFERS` directly gave the
//! question two owners, and they disagreed exactly where it is worst:
//! the footer of a Debian host printed `[5] nix` for a digit that did
//! nothing.

/// One buffer, keyed by the letter that follows the `b` prefix.
impl BufferSpec {
    /// How this jump prints in the footer and the help.
    pub fn chord(&self) -> Option<String> {
        Some(self.key?.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferSpec {
    pub buffer: Buffer,
    /// The digit that reaches this view, or `None` for one that is not in
    /// the ring at all.
    ///
    /// The log view has none. It is a *drill-down*: `l` names the unit it
    /// shows and `esc` comes back, so a digit reaching it would land on
    /// "whichever unit you opened last" - a stateful destination where
    /// every other digit is a fixed one. It stays in this table because
    /// the table is also what gives a view its title, and a title is what
    /// keys its cursor and its keymap overlay.
    pub key: Option<char>,
    pub title: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Buffer {
    Status,
    Procs,
    Systemd,
    Log,
    Io,
    Nix,
}

impl Buffer {
    /// Every buffer, for the keymap's shadowing check to iterate.
    ///
    /// The whole enum, not this host's registry: shadowing is a property
    /// of the tables, which are the same wherever masys runs, and a rule
    /// that only held on the hosts where a view happened to exist would
    /// be a rule that stops being checked.
    pub const ALL: &'static [Buffer] = &[Buffer::Status, Buffer::Procs, Buffer::Systemd, Buffer::Log, Buffer::Io, Buffer::Nix];

    /// The registry entry's title, which is also this buffer's identity
    /// for per-buffer state such as the cursor.
    ///
    /// Reads the catalogue rather than a host's `Registry` on purpose: a
    /// buffer keeps its identity on a host that does not offer it, so its
    /// cursor and its keymap overlay do not collide with another's.
    ///
    /// Panics if the buffer is not in `BUFFERS`. That is deliberate: the
    /// old fallback returned a shared placeholder, so an unregistered
    /// buffer was unreachable by key *and* silently shared one cursor
    /// slot with every other unregistered one. A missing registry entry
    /// is a programming error, and `every_buffer_is_registered` catches
    /// it before this ever runs.
    pub fn title(self) -> &'static str {
        BUFFERS.iter().find(|spec| spec.buffer == self).map(|spec| spec.title).expect("every Buffer must have a BUFFERS entry")
    }
}

/// The catalogue, keyed by digit.
///
/// Views used to take bare letters - `s t u j d` - and this file used to
/// record that as a deliberate departure from the design, noting the cost:
/// "these letters are protected globally, so no buffer can ever use them
/// for anything else, and the contextual actions the transient engine adds
/// will need other keys."
///
/// That bill came due. A digit costs no letter at all, and switching views
/// is a menu rather than navigation *within* one - which is exactly the
/// distinction that decides what may be global: a key is global when it
/// means the same thing in every view. `n`, `p`, `q`, `g`, `tab` and `/`
/// pass that test and stay. Five letters that only ever opened a menu did
/// not, and go back to the views.
///
/// Order is the ring order, and the digit is the 1-based position. Every
/// buffer masys can build is here; which of them a given host *has* is
/// `Registry`.
pub const BUFFERS: &[BufferSpec] = &[
    BufferSpec { buffer: Buffer::Status, key: Some('1'), title: "Status" },
    BufferSpec { buffer: Buffer::Procs, key: Some('2'), title: "Procs" },
    BufferSpec { buffer: Buffer::Systemd, key: Some('3'), title: "systemd" },
    BufferSpec { buffer: Buffer::Io, key: Some('4'), title: "IO" },
    // Only on a host with a declarative service. See `Registry`.
    BufferSpec { buffer: Buffer::Nix, key: Some('5'), title: "Nix" },
    // Reached by `l` from a unit, left by `esc`. See `BufferSpec::key`.
    BufferSpec { buffer: Buffer::Log, key: None, title: "log" },
];

/// Which buffers this host has.
///
/// `BUFFERS` was the whole answer while every host had the same views. The
/// Nix view is the first that some hosts do not: it must be *absent* on a
/// host with no declarative service, which is a stronger claim than empty
/// - no digit, no footer entry, no line in the `?` help.
#[derive(Debug, Clone)]
pub struct Registry {
    specs: Vec<BufferSpec>,
}

impl Registry {
    /// The views this host has, given whether a `DeclarativeService` was
    /// constructed for it.
    ///
    /// A `bool` rather than the service itself: this crate's question is
    /// "is there one", and taking the port would make the registry depend
    /// on a trait it never calls.
    pub fn new(declarative: bool) -> Self {
        let specs = BUFFERS.iter().copied().filter(|spec| declarative || spec.buffer != Buffer::Nix).collect();
        Self { specs }
    }

    pub fn contains(&self, buffer: Buffer) -> bool {
        self.specs.iter().any(|spec| spec.buffer == buffer)
    }

    pub fn by_key(&self, key: char) -> Option<Buffer> {
        self.specs.iter().find(|spec| spec.key == Some(key)).map(|spec| spec.buffer)
    }

    pub fn specs(&self) -> &[BufferSpec] {
        &self.specs
    }
}

impl Default for Registry {
    /// Without a declarative service - the common case, and the safe one:
    /// a default that included the Nix view would put it on Debian.
    fn default() -> Self {
        Self::new(false)
    }
}
