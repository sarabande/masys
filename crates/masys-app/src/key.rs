//! masys's own key type.
//!
//! The design's rule: no layer above the composition root names a terminal
//! library. The binary converts crossterm's events into these, so every
//! crate from here up is testable without a terminal and could be driven
//! by a different backend without touching a keymap.

/// The keys masys's keymap actually distinguishes. Deliberately not
/// exhaustive over a terminal library's full set: an unlisted key simply
/// resolves to no action, which is what makes adding a binding a keymap
/// change rather than a type change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    /// Shift-Tab. Its own variant rather than `Tab` plus a modifier
    /// because that is how a terminal delivers it - crossterm reports
    /// `BackTab`, and there is no shift bit to read.
    BackTab,
    Backspace,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    /// Any key this model doesn't carry. Never matches a binding.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub alt: bool,
    /// Held Control.
    ///
    /// Carried because without it `convert` in the binary had nowhere to
    /// put the modifier and dropped it, so `ctrl-k` arrived as a bare
    /// `k` - which in the Procs view opens the SIGTERM confirmation. A
    /// modifier a key model cannot represent is a modifier that silently
    /// becomes a different keystroke.
    pub ctrl: bool,
}

impl Key {
    pub fn new(code: KeyCode) -> Self {
        Key { code, alt: false, ctrl: false }
    }

    pub fn alt(code: KeyCode) -> Self {
        Key { code, alt: true, ctrl: false }
    }

    pub fn ctrl(code: KeyCode) -> Self {
        Key { code, alt: false, ctrl: true }
    }

    /// A bare character keypress - by far the most common case.
    pub fn char(c: char) -> Self {
        Key::new(KeyCode::Char(c))
    }
}

impl Key {
    /// Parses a key from its config-file spelling: `n`, `alt+n`, `tab`,
    /// `pgdn`.
    ///
    /// Case matters for a bare letter - `K` and `k` are different keys and
    /// masys binds both - but not for a named one, so `Tab` and `tab` are
    /// the same. Returns `None` for anything unrecognised rather than
    /// falling back to a guess: a typo in a config file should leave the
    /// default binding alone and say so, not silently bind something else.
    pub fn parse(text: &str) -> Option<Key> {
        let text = text.trim();
        // `shift+tab` is a key in its own right, not `tab` with a
        // modifier, so it must survive the alt-prefix strip below.
        if matches!(text.to_ascii_lowercase().as_str(), "backtab" | "shift+tab" | "s-tab") {
            return Some(Key::new(KeyCode::BackTab));
        }
        // Modifiers in either order, and either case, because a config
        // file is written by a person rather than generated.
        let mut alt = false;
        let mut ctrl = false;
        let mut rest = text;
        loop {
            let lower = rest.to_ascii_lowercase();
            if let Some(stripped) = lower.strip_prefix("alt+") {
                alt = true;
                rest = &rest[rest.len() - stripped.len()..];
            } else if let Some(stripped) = lower.strip_prefix("ctrl+").or_else(|| lower.strip_prefix("c-")) {
                ctrl = true;
                rest = &rest[rest.len() - stripped.len()..];
            } else {
                break;
            }
        }
        let code = match rest.to_ascii_lowercase().as_str() {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" | "shift+tab" | "s-tab" => KeyCode::BackTab,
            "bksp" | "backspace" => KeyCode::Backspace,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "pgup" | "pageup" => KeyCode::PageUp,
            "pgdn" | "pagedown" => KeyCode::PageDown,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "space" => KeyCode::Char(' '),
            // A single character, taken as typed so `K` stays distinct
            // from `k`.
            _ => {
                let mut chars = rest.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => KeyCode::Char(c),
                    _ => return None,
                }
            }
        };
        Some(Key { code, alt, ctrl })
    }

    /// How this key is written in a config file and shown in the help.
    pub fn spelling(&self) -> String {
        let base = match self.code {
            KeyCode::Char(' ') => "space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "enter".to_string(),
            KeyCode::Esc => "esc".to_string(),
            KeyCode::Tab => "tab".to_string(),
            KeyCode::BackTab => "shift+tab".to_string(),
            KeyCode::Backspace => "bksp".to_string(),
            KeyCode::Up => "up".to_string(),
            KeyCode::Down => "down".to_string(),
            KeyCode::PageUp => "pgup".to_string(),
            KeyCode::PageDown => "pgdn".to_string(),
            KeyCode::Home => "home".to_string(),
            KeyCode::End => "end".to_string(),
            KeyCode::Other => "?".to_string(),
        };
        // Ctrl outermost, so `ctrl+alt+x` reads the way it is typed.
        let base = if self.alt { format!("alt+{base}") } else { base };
        if self.ctrl { format!("ctrl+{base}") } else { base }
    }
}
