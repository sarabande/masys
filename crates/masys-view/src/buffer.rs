//! Which buffers masys can build, and what each is called.
//!
//! The *catalogue*: a data table, so adding a buffer is adding a row
//! rather than editing a match arm in several files. Which buffers a
//! given **host** has is `masys_app::buffer::Registry`, and the two are
//! different questions - on a Debian box the Nix buffer is in the
//! catalogue and absent from the registry.
//!
//! Here rather than in masys-app because a `Buffer` is a destination in
//! the row model: `Jump` names one, and `Jump` is what a finding answers
//! when asked where it points. The session decides which of them this
//! host offers and what key reaches them; naming them is not a session
//! decision, and the layer that draws rows needs the name to draw a
//! header with.

impl BufferSpec {
    /// This jump's chord, as the footer and the help would print it.
    ///
    /// *Would*, not does: nothing in the binary calls this. Both render
    /// from `KeyBinding::chord`, which the keymap builds. The one caller
    /// is the test that asserts no buffer binds a key a jump already
    /// owns, and the method is what makes that test an independent
    /// check - comparing the keymap against itself would pass however
    /// wrong it was, so the assertion needs a second source for the same
    /// string, and this is it.
    pub fn chord(&self) -> Option<String> {
        Some(self.key?.to_string())
    }
}

/// One buffer's catalogue entry: which buffer, which digit reaches it,
/// and what it is called.
///
/// This doc used to read "keyed by the letter that follows the `b`
/// prefix", and sat above the `impl` block rather than the struct - so it
/// described a `b`-prefixed scheme that stopped existing when buffers
/// moved to digits, from a position where it documented the wrong item.
/// Both were true until 2026-08-30.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferSpec {
    pub buffer: Buffer,
    /// The digit that reaches this buffer, or `None` for one that is not in
    /// the ring at all.
    ///
    /// The log buffer has none. It is a *drill-down*: `l` names the unit it
    /// shows and `esc` comes back, so a digit reaching it would land on
    /// "whichever unit you opened last" - a stateful destination where
    /// every other digit is a fixed one. It stays in this table because
    /// the table is also what gives a buffer its title, and the header
    /// prints one for the log like any other buffer.
    pub key: Option<char>,
    pub title: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Buffer {
    Status,
    Procs,
    Systemd,
    Log,
    Io,
    Nix,
    Packages,
}

impl Buffer {
    /// Every buffer, for the keymap's shadowing check to iterate.
    ///
    /// The whole enum, not this host's registry: shadowing is a property
    /// of the tables, which are the same wherever masys runs, and a rule
    /// that only held on the hosts where a buffer happened to exist would
    /// be a rule that stops being checked.
    pub const ALL: &'static [Buffer] = &[
        Buffer::Status,
        Buffer::Procs,
        Buffer::Systemd,
        Buffer::Log,
        Buffer::Io,
        Buffer::Nix,
        Buffer::Packages,
    ];

    /// What the header prints for this buffer.
    ///
    /// A display name and nothing else. It used to be an identity as
    /// well - `App` keyed its cursors and its filters on it, and `Keymap`
    /// keyed its per-buffer overlays on it - which made rewording a
    /// heading a change that could move a cursor or hand one buffer
    /// another's key overrides. All three are keyed on the `Buffer` now,
    /// and `a_title_never_keys_anything` is what keeps the next one from
    /// quietly going back.
    ///
    /// Reads the catalogue rather than a host's `Registry` on purpose: a
    /// buffer has a name on a host that does not offer it.
    ///
    /// Panics if the buffer is not in `BUFFERS`. That is deliberate: the
    /// old fallback returned a shared placeholder, so an unregistered
    /// buffer was unreachable by key *and* silently shared one cursor
    /// slot with every other unregistered one. A missing registry entry
    /// is a programming error, and `every_buffer_is_registered` catches
    /// it before this ever runs.
    pub fn title(self) -> &'static str {
        BUFFERS
            .iter()
            .find(|spec| spec.buffer == self)
            .map(|spec| spec.title)
            .expect("every Buffer must have a BUFFERS entry")
    }
}

/// The catalogue, keyed by digit.
///
/// Buffers used to take bare letters - `s t u j d` - and this file used to
/// record that as a deliberate departure from the design, noting the cost:
/// "these letters are protected globally, so no buffer can ever use them
/// for anything else, and the contextual actions the transient engine adds
/// will need other keys."
///
/// That bill came due. A digit costs no letter at all, and switching buffers
/// is a menu rather than navigation *within* one - which is exactly the
/// distinction that decides what may be global: a key is global when it
/// means the same thing in every buffer. `n`, `p`, `q`, `g`, `tab` and `/`
/// pass that test and stay. Five letters that only ever opened a menu did
/// not, and go back to the buffers.
///
/// Order is the ring order, and the digit is the 1-based position. Every
/// buffer masys can build is here; which of them a given host *has* is
/// `Registry`.
pub const BUFFERS: &[BufferSpec] = &[
    BufferSpec {
        buffer: Buffer::Status,
        key: Some('1'),
        title: "Status",
    },
    BufferSpec {
        buffer: Buffer::Procs,
        key: Some('2'),
        title: "Procs",
    },
    BufferSpec {
        buffer: Buffer::Systemd,
        key: Some('3'),
        title: "systemd",
    },
    BufferSpec {
        buffer: Buffer::Io,
        key: Some('4'),
        title: "IO",
    },
    // Only on a host with a declarative service. See `Registry`.
    BufferSpec {
        buffer: Buffer::Nix,
        key: Some('5'),
        title: "Nix",
    },
    // Only on a host whose platform can list packages. See `Registry`.
    BufferSpec {
        buffer: Buffer::Packages,
        key: Some('6'),
        title: "Packages",
    },
    // Reached by `l` from a unit, left by `esc`. See `BufferSpec::key`.
    BufferSpec {
        buffer: Buffer::Log,
        key: None,
        title: "log",
    },
];
