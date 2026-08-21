//! `/proc/[pid]/stat`, the only per-process file whose format actively
//! resists naive parsing.

use masys_domain::error::MasysError;
use masys_domain::sample::ProcState;

/// The five fields of `/proc/[pid]/stat` that `masys_domain::sample::Proc`
/// needs. Named `Raw` because every count is in the kernel's own units -
/// clock ticks and pages - not bytes or seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawStat {
    pub comm: String,
    pub state: ProcState,
    pub cpu_ticks: u64,
    pub nice: i32,
    pub rss_pages: u64,
    /// Clock ticks since boot, field 22. Becomes `started_at_ms` only once
    /// combined with `/proc/stat`'s `btime` and `CLK_TCK`.
    pub starttime_ticks: u64,
}

fn state_of(token: &str) -> ProcState {
    match token {
        "R" => ProcState::Running,
        "D" => ProcState::UninterruptibleWait,
        "Z" => ProcState::Zombie,
        "T" | "t" => ProcState::Stopped,
        // S, I (idle kernel thread), and anything a future kernel adds:
        // sleeping is the honest default, and an unknown state must not
        // fail a whole sample.
        _ => ProcState::Sleeping,
    }
}

/// `comm` is arbitrary bytes chosen by the process, so it can contain
/// spaces *and* parentheses - `1 ((evil) x) S 0 ...` is a legal line.
/// Splitting on whitespace, or on the first `)`, mis-parses every field
/// after it. The kernel's own guarantee is that `comm` is wrapped in
/// parentheses and no field after it contains one, so the *last* `)` is
/// unambiguous.
pub fn parse_stat(text: &str) -> Result<RawStat, MasysError> {
    let open = text.find('(').ok_or_else(|| MasysError::System("stat: no comm".into()))?;
    let close = text.rfind(')').ok_or_else(|| MasysError::System("stat: unterminated comm".into()))?;
    if close < open {
        return Err(MasysError::System("stat: malformed comm".into()));
    }
    let comm = text[open + 1..close].to_string();

    // Field 3 (state) is the first token after `)`, so this slice's index
    // 0 is field 3 and index N is field N+3.
    let rest: Vec<&str> = text[close + 1..].split_whitespace().collect();
    let field = |n: usize| -> Result<&str, MasysError> {
        rest.get(n - 3).copied().ok_or_else(|| MasysError::System(format!("stat: missing field {n}")))
    };
    let num = |n: usize| -> Result<u64, MasysError> {
        field(n)?.parse().map_err(|_| MasysError::System(format!("stat: field {n} is not a number")))
    };

    Ok(RawStat {
        comm,
        state: state_of(field(3)?),
        // utime + stime. Children's time (fields 16/17) is deliberately
        // excluded: masys shows per-process cost, and a shell that has
        // reaped a heavy child would otherwise read as busy forever.
        cpu_ticks: num(14)? + num(15)?,
        nice: field(19)?.parse().map_err(|_| MasysError::System("stat: nice is not a number".into()))?,
        rss_pages: num(24)?,
        starttime_ticks: num(22)?,
    })
}
