//! `/proc/pressure/{cpu,io,memory}` - PSI, the signal the design picks
//! over load average because it measures stall, not queue depth.

use masys_domain::error::MasysError;
use masys_domain::sample::PsiLine;

/// One `some`/`full` line: `some avg10=0.04 avg60=0.23 avg300=0.12 total=586715292`.
fn parse_line(line: &str) -> PsiLine {
    let mut psi = PsiLine::default();
    for token in line.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else { continue };
        let Ok(number) = value.parse::<f32>() else { continue };
        match key {
            "avg10" => psi.avg10 = number,
            "avg60" => psi.avg60 = number,
            "avg300" => psi.avg300 = number,
            _ => {}
        }
    }
    psi
}

/// `(some, full)` for one resource file. `/proc/pressure/cpu` carries a
/// `full` line whose values the kernel always reports as zero - a task
/// stalled on cpu is by definition not the only runnable one - so callers
/// discard it rather than surfacing a permanent 0.0%.
pub fn parse_pressure(text: &str) -> Result<(PsiLine, PsiLine), MasysError> {
    let mut some = None;
    let mut full = None;
    for line in text.lines() {
        match line.split_whitespace().next() {
            Some("some") => some = Some(parse_line(line)),
            Some("full") => full = Some(parse_line(line)),
            _ => {}
        }
    }
    Ok((some.ok_or_else(|| MasysError::System("pressure: no some line".into()))?, full.unwrap_or_default()))
}
