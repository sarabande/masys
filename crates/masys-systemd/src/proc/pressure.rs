//! `/proc/pressure/{cpu,io,memory}` - PSI, the signal the design picks
//! over load average because it measures stall, not queue depth.

use masys_domain::error::MasysError;
use masys_domain::sample::PsiLine;

/// One `some`/`full` line: `some avg10=0.04 avg60=0.23 avg300=0.12 total=586715292`.
///
/// `None` unless all three averages are present and readable. It started
/// from `PsiLine::default()` and filled in what it found, so a line
/// missing `avg60` - or carrying one that would not parse - yielded a
/// zero, and a zero here is the claim that nothing is stalling. These
/// are the figures the Status header's colours are decided from, so that
/// zero was masys reporting a machine healthy on the strength of a
/// number it never read.
///
/// All three rather than only the `avg60` everything thresholds on:
/// `PsiLine` is public, the other two are carried for whatever reads
/// them next, and a struct half-filled with silent zeros is a trap laid
/// for that caller. A kernel writes all three or masys has not
/// understood the line.
fn parse_line(line: &str) -> Option<PsiLine> {
    let mut psi = PsiLine::default();
    let (mut avg10, mut avg60, mut avg300) = (false, false, false);
    for token in line.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        let Ok(number) = value.parse::<f32>() else {
            continue;
        };
        match key {
            "avg10" => (psi.avg10, avg10) = (number, true),
            "avg60" => (psi.avg60, avg60) = (number, true),
            "avg300" => (psi.avg300, avg300) = (number, true),
            _ => {}
        }
    }
    (avg10 && avg60 && avg300).then_some(psi)
}

/// `(some, full)` for one resource file.
///
/// `full` is an `Option` because older kernels emit no `full` line for
/// cpu at all, and the caller is the only layer that knows which file
/// this was. It returned `PsiLine::default()` for an absent line until
/// 2026-08-31, which handed every caller a reading of zero for a line
/// that was never there - harmless for cpu, whose `full` is discarded,
/// and a claim that nothing is stalling for io and memory, whose is not.
pub fn parse_pressure(text: &str) -> Result<(PsiLine, Option<PsiLine>), MasysError> {
    let mut some = None;
    let mut full = None;
    for line in text.lines() {
        match line.split_whitespace().next() {
            Some("some") => some = parse_line(line),
            Some("full") => full = parse_line(line),
            _ => {}
        }
    }
    Ok((
        some.ok_or_else(|| MasysError::System("pressure: no readable some line".into()))?,
        full,
    ))
}
