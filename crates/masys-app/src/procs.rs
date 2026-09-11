//! The Procs buffer: processes folded under the cgroup that owns them.
//!
//! Grouping by cgroup rather than by parent pid is the design's choice and
//! the reason this buffer is an outline rather than a sorted table: on a
//! systemd host a process's cgroup *is* its unit, so folding by cgroup
//! answers "what is this service costing me" directly, which a pid tree
//! does not.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use masys_domain::proc_detail::ProcDetail;
use masys_domain::rate::{self, ProcRate};
use masys_domain::sample::{Proc, Snapshot};
use masys_domain::service::SystemService;
use masys_view::Node;

use crate::keymap::Sort;

/// The Procs buffer: which rows are open, how they are ordered, and the
/// rates that ordering is against.
///
/// Three of these were fields of `App` and one lived in its `Facts`,
/// which is what made the row builder an eight-parameter call. `rows`
/// now names only what the session shares: the processes and the clock
/// tick, both from the one sample, and the buffer-wide fold set.
///
/// `SystemService` stays a port and is handed to the two methods that
/// read a process's detail, the way `IoBuffer` takes its `DirScanner`.
///
/// The fields are public for `NixBuffer`'s reason - they are readings and
/// cursor state rather than invariants this type alone can state - and
/// the methods below are the only writers in the program.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcsBuffer {
    /// The open process rows, and what the detail read returned for each.
    ///
    /// `Some(None)` is "open, and the read failed" - which for an
    /// unprivileged masys looking at another user's process is the common
    /// case, and is why the value is an `Option` rather than the row
    /// simply not opening.
    ///
    /// Keyed by pid rather than by name: several processes share a name,
    /// and the one you opened is a specific one.
    pub expanded: HashMap<u32, Option<ProcDetail>>,
    /// How groups and their members are ordered, and which way.
    pub sort: Sort,
    pub descending: bool,
    /// CPU%, memory and IO per process. A derivative, so it is empty
    /// until a second sample exists to derive against - which is what
    /// hides the Top CPU section until a rate can actually be computed.
    pub rates: Vec<ProcRate>,
}

impl Default for ProcsBuffer {
    /// Busiest first, which is the question this buffer is opened to
    /// answer. `Sort`'s own `default_descending` decides the direction so
    /// that the initial state and the one `Action::SortBy` produces come
    /// from the same rule.
    fn default() -> Self {
        ProcsBuffer {
            expanded: HashMap::new(),
            sort: Sort::Cpu,
            descending: Sort::Cpu.default_descending(),
            rates: Vec::new(),
        }
    }
}

impl ProcsBuffer {
    /// Per-process rates against the previous sample.
    ///
    /// Empty rather than zeroed where there is no previous sample: CPU%
    /// is a derivative and does not exist before the second tick.
    pub fn refresh(&mut self, previous: Option<&Snapshot>, current: &Snapshot) {
        self.rates = match previous {
            Some(previous) => rate::derive(previous, current, current.clock_ticks_per_sec),
            None => Vec::new(),
        };
    }

    /// Re-reads every open row's detail, and drops the rows whose process
    /// has gone.
    ///
    /// A process that exited while its row was open is dropped rather
    /// than re-read: `proc_detail` fails for a dead pid, and holding the
    /// row open on the last thing it said would let a stale command line
    /// outlive the process it described. A read that merely *failed*
    /// leaves the row alone, because that is the ordinary state of
    /// another user's process - the two are told apart by asking the
    /// sample whether the pid is still there.
    ///
    /// Called by `tick` while this is the open buffer, and asked of the
    /// sample that tick just took - the previous one would keep a dead
    /// pid's row a tick past its process.
    ///
    /// Ticks are rare while a row is open, because
    /// [`ProcsBuffer::paused`] stops the caller taking them: this is the
    /// most expensive read the buffer makes, and a tick underneath an
    /// open row costs the most and helps the least. `g` still comes
    /// through, and it is the reason this runs at all - a refresh that
    /// updated the row but not the block beneath it would draw one frame
    /// from two moments.
    pub fn refresh_open(&mut self, system: &dyn SystemService, current: Option<&Snapshot>) {
        let open: Vec<u32> = self.expanded.keys().copied().collect();
        for pid in open {
            match system.proc_detail(pid) {
                Ok(detail) => {
                    self.expanded.insert(pid, Some(detail));
                }
                Err(_)
                    if !current
                        .is_some_and(|sample| sample.procs.iter().any(|proc| proc.pid == pid)) =>
                {
                    self.expanded.remove(&pid);
                }
                // Still in the sample, just unreadable - which is the
                // ordinary state of another user's process.
                Err(_) => {}
            }
        }
    }

    /// Every row this buffer shows.
    pub fn rows(
        &self,
        procs: &[Proc],
        clock_ticks: u64,
        folds: &HashSet<String>,
        now_ms: u64,
    ) -> Vec<Node> {
        layout(
            procs,
            &self.rates,
            folds,
            self.sort,
            self.descending,
            clock_ticks,
            now_ms,
            &self.expanded,
        )
    }

    /// `enter` on a process row: opens it, or shuts one already open.
    ///
    /// One read, on the keypress, for the same reason `App::open_unit`
    /// is: the sweep already touches every process on the host every
    /// tick, and these reads are per-descriptor on top of that. A failed
    /// read leaves the row open with what `Proc` alone carries - most of
    /// this is unreadable for another user's process, and refusing to
    /// open the row would make an unprivileged masys look broken rather
    /// than unprivileged.
    pub fn toggle(&mut self, system: &dyn SystemService, pid: u32) {
        if self.expanded.remove(&pid).is_none() {
            self.expanded.insert(pid, system.proc_detail(pid).ok());
        }
    }

    /// The same key again reverses; a different one switches column and
    /// takes that column's natural direction, so `n` always starts at a-z
    /// rather than inheriting whichever way the last column pointed.
    pub fn sort_by(&mut self, sort: Sort) {
        self.descending = if self.sort == sort {
            !self.descending
        } else {
            sort.default_descending()
        };
        self.sort = sort;
    }

    /// Puts the buffer in this column's natural order, whatever it was in
    /// before.
    ///
    /// Not [`ProcsBuffer::sort_by`], which is the *key's* behaviour and
    /// reverses when it is given the column already showing. A caller
    /// that wants "busiest first" and calls `sort_by(Cpu)` on a buffer
    /// already sorted by cpu gets least-busy-first, because the default
    /// is cpu-descending and the second press is a reversal. That is
    /// right for `c` and wrong for anything asking for an order by name.
    pub fn set_sort(&mut self, sort: Sort) {
        self.sort = sort;
        self.descending = sort.default_descending();
    }

    /// Whether an open row should stop the caller's auto-refresh.
    ///
    /// Advisory, not enforced inside `tick`: `g` must still refresh on
    /// demand, and the caller owns the clock. Folding the row resumes it,
    /// with no state to remember either way - the pause *is* the open
    /// row.
    pub fn paused(&self) -> bool {
        !self.expanded.is_empty()
    }
}

/// The process under the cursor, if the cursor is on one.
pub fn selected(node: Option<&Node>) -> Option<(u32, String, i32)> {
    match node {
        Some(Node::Proc { proc, .. }) => Some((proc.pid, proc.comm.clone(), proc.nice)),
        _ => None,
    }
}

/// Where kernel threads bucket. They have no unit to belong to, and
/// burying them among real cgroups would make the tree lie about who owns
/// them - and there are typically more of them than of everything else
/// combined, so they would bury the rows that matter.
pub const KERNEL_GROUP: &str = "kernel";

/// Whether a cgroup path means "not owned by any unit".
///
/// The design says kernel threads have *no* cgroup. On a live host that
/// is not true, and testing for an empty value silently fails: a kernel
/// thread reports the **root** cgroup, `0::/`, so `kthreadd` on this
/// machine reads `/`. Bucketing on emptiness alone left 145 kernel
/// threads in a group named `/` that sorted ahead of every real service.
/// What a set of processes is doing to the disk, or `None` where nothing
/// in it has been measured yet.
///
/// Read and write summed, because the column is one question - "what is
/// hitting the disk" - and splitting it would ask the operator to know
/// the direction before they can look.
///
/// `None` only when *no* member has a rate. A group where one process has
/// been measured and another has not is a group with a real, if partial,
/// figure: the alternative is a group that drops to the bottom of the
/// buffer because one short-lived child inside it is new.
fn io_total<'a>(
    procs: impl Iterator<Item = &'a Proc>,
    rate_of: impl Fn(u32) -> Option<ProcRate>,
) -> Option<f64> {
    let mut total = None;
    for proc in procs {
        if let Some(rate) = rate_of(proc.pid) {
            *total.get_or_insert(0.0) += rate.io_read_bytes_per_sec + rate.io_write_bytes_per_sec;
        }
    }
    total
}

fn is_kernel(cgroup: Option<&str>) -> bool {
    matches!(cgroup.map(str::trim), None | Some("") | Some("/"))
}

/// One group's rows: the group itself, then its processes unless it is
/// collapsed.
///
/// `collapsed` holds group names rather than expanded ones, so a cgroup
/// that appears between two ticks shows up expanded without anything
/// having to notice it is new.
///
/// Eight arguments, and clippy is right that this is too many for an
/// interface. It is not one: this is private, and four of the eight are
/// [`ProcsBuffer`]'s own fields. Callers reach it through
/// [`ProcsBuffer::rows`], which names the other four.
#[allow(clippy::too_many_arguments)]
fn layout(
    procs: &[Proc],
    rates: &[ProcRate],
    collapsed: &HashSet<String>,
    sort: Sort,
    descending: bool,
    clock_ticks: u64,
    now_ms: u64,
    open: &HashMap<u32, Option<ProcDetail>>,
) -> Vec<Node> {
    if procs.is_empty() {
        return Vec::new();
    }
    let rate_of = |pid: u32| rates.iter().find(|r| r.pid == pid).copied();

    // BTreeMap so the tree is in a stable, name-sorted order every tick.
    // Anything else and rows would shuffle under the cursor between
    // samples, which is the fastest way to make a live buffer unusable.
    let mut groups: BTreeMap<String, Vec<&Proc>> = BTreeMap::new();
    for proc in procs {
        let name = if is_kernel(proc.cgroup.as_deref()) {
            KERNEL_GROUP.to_string()
        } else {
            proc.cgroup.clone().unwrap_or_default()
        };
        groups.entry(name).or_default().push(proc);
    }

    // Groups are ordered by the same key their members are, so `o` moves
    // the whole buffer rather than only its leaves. Name order comes free
    // from the BTreeMap; the other two need an explicit pass.
    let mut ordered: Vec<(String, Vec<&Proc>)> = groups.into_iter().collect();
    match sort {
        Sort::Name => ordered.sort_by(|a, b| flip(a.0.cmp(&b.0), descending)),
        Sort::Cpu => ordered.sort_by(|a, b| {
            let total = |ps: &Vec<&Proc>| {
                ps.iter()
                    .filter_map(|p| rate_of(p.pid))
                    .map(|r| r.cpu_percent)
                    .sum::<f32>()
            };
            // The tie-break stays ascending by name whichever way the
            // column points: it is there to keep rows from shuffling
            // between ticks, not to be part of the ordering the user
            // asked for.
            flip(total(&a.1).total_cmp(&total(&b.1)), descending).then_with(|| a.0.cmp(&b.0))
        }),
        Sort::Memory => ordered.sort_by(|a, b| {
            let total = |ps: &Vec<&Proc>| ps.iter().map(|p| p.rss_bytes).sum::<u64>();
            flip(total(&a.1).cmp(&total(&b.1)), descending).then_with(|| a.0.cmp(&b.0))
        }),
        Sort::Io => ordered.sort_by(|a, b| {
            let total = |ps: &Vec<&Proc>| io_total(ps.iter().copied(), rate_of);
            unmeasured_last(total(&a.1), total(&b.1), descending, || a.0.cmp(&b.0))
        }),
    }

    let mut rows = Vec::new();
    for (name, mut members) in ordered {
        // Heaviest first within a group, so an expanded unit shows what is
        // costing it before anything else.
        match sort {
            Sort::Cpu => members.sort_by(|a, b| {
                let cpu = |p: &Proc| rate_of(p.pid).map(|r| r.cpu_percent).unwrap_or(0.0);
                flip(cpu(a).total_cmp(&cpu(b)), descending).then_with(|| a.pid.cmp(&b.pid))
            }),
            Sort::Memory => members.sort_by(|a, b| {
                flip(a.rss_bytes.cmp(&b.rss_bytes), descending).then_with(|| a.pid.cmp(&b.pid))
            }),
            Sort::Name => members.sort_by(|a, b| {
                flip(a.comm.cmp(&b.comm), descending).then_with(|| a.pid.cmp(&b.pid))
            }),
            Sort::Io => members.sort_by(|a, b| {
                let io = |p: &Proc| io_total(std::iter::once(p), rate_of);
                unmeasured_last(io(a), io(b), descending, || a.pid.cmp(&b.pid))
            }),
        }

        let expanded = !collapsed.contains(&name);
        rows.push(Node::ProcGroup {
            // Aggregates are sums over the group's own processes. A
            // cgroup path is hierarchical, but these are not rolled up
            // through the hierarchy: nesting parents inside parents is
            // the full tree the design's mockup shows, and it needs a
            // path-aware builder rather than this flat one.
            cpu_percent: members
                .iter()
                .filter_map(|p| rate_of(p.pid))
                .map(|r| r.cpu_percent)
                .sum(),
            mem_bytes: members.iter().map(|p| p.rss_bytes).sum(),
            read_bytes_per_sec: members
                .iter()
                .filter_map(|p| rate_of(p.pid))
                .map(|r| r.io_read_bytes_per_sec)
                .sum(),
            write_bytes_per_sec: members
                .iter()
                .filter_map(|p| rate_of(p.pid))
                .map(|r| r.io_write_bytes_per_sec)
                .sum(),
            proc_count: members.len() as u32,
            name,
            depth: 0,
            expanded,
        });

        if expanded {
            // `flat_map` rather than `map`: an open process is two rows,
            // its own and its detail block, and the detail has to sit
            // directly beneath the row it belongs to.
            rows.extend(members.into_iter().flat_map(|proc| {
                let detail = open.get(&proc.pid);
                let mut rows = vec![Node::Proc {
                    rate: rate_of(proc.pid),
                    cpu_seconds: proc.cpu_ticks / clock_ticks.max(1),
                    // `None` rather than zero when the start time is
                    // missing or in the future: a clock that disagrees
                    // with the sampler would otherwise render every
                    // process as freshly started, which is the one thing
                    // this column is there to detect.
                    age_ms: (proc.started_at_ms > 0 && now_ms >= proc.started_at_ms)
                        .then(|| now_ms - proc.started_at_ms),
                    proc: proc.clone(),
                    depth: 1,
                    expanded: detail.is_some(),
                }];
                if let Some(detail) = detail {
                    rows.push(Node::ProcDetail {
                        proc: proc.clone(),
                        detail: detail.clone().map(Box::new),
                    });
                }
                rows
            }));
        }
    }
    rows
}

/// **A row nothing has measured sorts last, either direction.**
///
/// `rate_of` answers `None` until two samples exist for a pid, and
/// treating that as a rate of zero would put a row masys knows nothing
/// about among the ones it knows are idle - a claim about a reading it
/// has not taken. Last is not a rank here; it is where the rows with no
/// answer wait, which is why `descending` flips the measured rows against
/// each other and never lifts the unmeasured ones off the bottom.
///
/// The tiebreak belongs to the caller because what makes two rows equal
/// differs by what they are: groups settle by name, processes by pid.
fn unmeasured_last(
    a: Option<f64>,
    b: Option<f64>,
    descending: bool,
    tie: impl FnOnce() -> Ordering,
) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => flip(x.total_cmp(&y), descending).then_with(tie),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => tie(),
    }
}

/// Reverses an ordering when the column is pointing the other way.
///
/// Applied to the comparison rather than by reversing the sorted list,
/// so the stable tie-break underneath keeps pointing the same way: a
/// reversed list would flip that too, and rows with equal values would
/// swap places every time the direction changed.
fn flip(ordering: std::cmp::Ordering, descending: bool) -> std::cmp::Ordering {
    if descending {
        ordering.reverse()
    } else {
        ordering
    }
}
