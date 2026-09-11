//! Colors for every themed element, defaulted here and settable from
//! `~/.config/masys/config.toml`'s `[theme]` table.
//!
//! The seam this was built with is what made that additive: every
//! function in this crate takes a `&Theme` rather than reaching for a
//! color, so the composition root reads six names and hands over a
//! different struct, and nothing between here and a terminal cell had to
//! learn that a config file exists. `masys_domain::finding::Thresholds`
//! introduced the same shape for the same reason.
//!
//! Defaults, not a preset. There is no named palette to choose from, for
//! the reason the README gives for there being no colemak keymap: the
//! mechanism is what earns its keep, and a curated set of themes is a
//! second decision nobody has asked for.

use ratatui::style::Color;

/// One color per semantic slot, not per `FindingKind` variant - `x`/`~`/`^`/`!`
/// map onto three severity tiers (dead, unstable-or-resource-pressure,
/// urgent), which is coarser than the fourteen `FindingKind` variants but matches
/// how the status buffer mockup actually reads: severity, not category, is
/// what the color communicates.
///
/// Which is also why these are worth being able to change. The glyph
/// carries the shape and the color carries the judgement, so on a
/// terminal where `LightRed` or `DarkGray` is illegible - the two most
/// likely of the sixteen to have been redefined - the judgement is what
/// is lost. Each field name below is the key that sets it.
///
/// **Which findings draw in which colour is not written here.** These
/// three name a `Severity`, and `masys_domain::FindingKind::severity` is
/// the one statement of which findings carry which. Each doc below used
/// to list them - "flapping, PSI, and disk/inode capacity" - which was a
/// fourth copy of that mapping, in prose, that nothing could check and
/// that went stale the first time a finding was added without anyone
/// thinking to edit a colour's doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub section_header: Color,
    /// `x` - `Severity::Dead`: something has stopped doing its job.
    pub severity_dead: Color,
    /// `~`/`^` - `Severity::Warning`: not down, but heading somewhere bad.
    pub severity_warning: Color,
    /// `!` - `Severity::Urgent`: needs attention now.
    pub severity_urgent: Color,
    /// `.` - the System section's plain facts.
    pub info: Color,
    pub status_error: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            section_header: Color::White,
            severity_dead: Color::Red,
            severity_warning: Color::Yellow,
            severity_urgent: Color::LightRed,
            info: Color::DarkGray,
            status_error: Color::Red,
        }
    }
}
