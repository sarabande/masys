//! Colors for every themed element. Hardcoded for now - real config-file
//! loading (`~/.config/masys/config.toml`) is unassigned future work, same
//! as it was when `masys_domain::finding::Thresholds` introduced the same
//! seam: every function below takes a `&Theme` rather than hardcoding a
//! color, so loading one from config later is additive.

use ratatui::style::Color;

/// One color per semantic slot, not per `Finding` variant - `x`/`~`/`^`/`!`
/// map onto three severity tiers (dead, unstable-or-resource-pressure,
/// urgent), which is coarser than the nine `Finding` variants but matches
/// how the status buffer mockup actually reads: severity, not category, is
/// what the color communicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub section_header: Color,
    /// `x` - a unit that has stopped doing its job.
    pub severity_dead: Color,
    /// `~`/`^` - flapping, PSI, and disk/inode capacity: not down, but
    /// heading somewhere bad.
    pub severity_warning: Color,
    /// `!` - clock, OOM, degraded rollup, kernel, journal errors: needs
    /// attention now.
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
