//! Turning a reading into the words a row shows.
//!
//! Not a decision, which is the only thing this crate refuses: a byte
//! count rendered as `11G` is the same fact in fewer characters, and
//! nothing here compares anything against a threshold or picks a colour.
//! Those stay in masys-domain and masys-render respectively.
//!
//! Here rather than in the renderer because the text of a row is part of
//! what the row *is*: `Presentation` answers what a finding says, and it
//! cannot answer that from a layer the renderer sits above. The renderer
//! re-exports these, so `masys_render::view::human_bytes` still resolves.

use masys_domain::finding::PressureResource;

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: &[(u64, &str)] = &[
        (1u64 << 40, "T"),
        (1u64 << 30, "G"),
        (1u64 << 20, "M"),
        (1u64 << 10, "K"),
    ];
    for &(scale, suffix) in UNITS {
        if bytes >= scale {
            let value = bytes as f64 / scale as f64;
            return if value >= 10.0 {
                format!("{value:.0}{suffix}")
            } else {
                format!("{value:.1}{suffix}")
            };
        }
    }
    format!("{bytes}B")
}

/// `10_800_000 -> "3h"`, `187_200_000 -> "2d 4h"` - the biggest one or two
/// non-zero units, matching the mockup's `3h ago`/`2d 4h`.
pub fn human_duration(ms: u64) -> String {
    let total_mins = ms / 60_000;
    let days = total_mins / (60 * 24);
    let hours = (total_mins / 60) % 24;
    let minutes = total_mins % 60;
    if days > 0 {
        return if hours > 0 {
            format!("{days}d {hours}h")
        } else {
            format!("{days}d")
        };
    }
    if hours > 0 {
        return if minutes > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{hours}h")
        };
    }
    format!("{minutes}m")
}

pub fn pressure_word(resource: PressureResource) -> &'static str {
    match resource {
        PressureResource::Cpu => "cpu",
        PressureResource::Io => "io",
        PressureResource::Memory => "memory",
    }
}
