//! Which buffers exist, and how they are reached.
//!
//! The design calls for a registry rather than wagit's closed
//! `outline::Context` enum, because masys has five buffers in v1 and the
//! platform adapter contributes another. This is the registry in the sense
//! that matters today - `BUFFERS` is a data table, so adding a buffer is
//! adding a row rather than editing a match arm in several files. It stays
//! a static slice until something actually contributes a buffer at
//! runtime; making it dynamic before then would be structure with no
//! second case to justify it.

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
}

impl Buffer {
    /// Every buffer, for the keymap's shadowing check to iterate.
    pub const ALL: &'static [Buffer] = &[Buffer::Status, Buffer::Procs, Buffer::Systemd, Buffer::Log, Buffer::Io];

    /// The registry entry's title, which is also this buffer's identity
    /// for per-buffer state such as the cursor.
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

/// The registry, keyed by digit.
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
/// Order is the ring order, and the digit is the 1-based position.
pub const BUFFERS: &[BufferSpec] = &[
    BufferSpec { buffer: Buffer::Status, key: Some('1'), title: "Status" },
    BufferSpec { buffer: Buffer::Procs, key: Some('2'), title: "Procs" },
    BufferSpec { buffer: Buffer::Systemd, key: Some('3'), title: "systemd" },
    BufferSpec { buffer: Buffer::Io, key: Some('4'), title: "IO" },
    // Reached by `l` from a unit, left by `esc`. See `BufferSpec::key`.
    BufferSpec { buffer: Buffer::Log, key: None, title: "log" },
];
