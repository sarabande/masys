//! The Procs buffer: processes folded under the cgroup that owns them.
//!
//! Grouping by cgroup rather than by parent pid is the design's choice and
//! the reason this buffer is an outline rather than a sorted table: on a
//! systemd host a process's cgroup *is* its unit, so folding by cgroup
//! answers "what is this service costing me" directly, which a pid tree
//! does not.

use std::collections::{BTreeMap, HashMap, HashSet};

use masys_domain::proc_detail::ProcDetail;
use masys_domain::rate::ProcRate;
use masys_domain::sample::Proc;
use masys_view::Node;

use crate::keymap::Sort;

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
fn is_kernel(cgroup: Option<&str>) -> bool {
    matches!(cgroup.map(str::trim), None | Some("") | Some("/"))
}

/// One group's rows: the group itself, then its processes unless it is
/// collapsed.
///
/// `collapsed` holds group names rather than expanded ones, so a cgroup
/// that appears between two ticks shows up expanded without anything
/// having to notice it is new.
pub fn build_proc_rows(
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
    // samples, which is the fastest way to make a live view unusable.
    let mut groups: BTreeMap<String, Vec<&Proc>> = BTreeMap::new();
    for proc in procs {
        let name = if is_kernel(proc.cgroup.as_deref()) { KERNEL_GROUP.to_string() } else { proc.cgroup.clone().unwrap_or_default() };
        groups.entry(name).or_default().push(proc);
    }

    // Groups are ordered by the same key their members are, so `o` moves
    // the whole view rather than only its leaves. Name order comes free
    // from the BTreeMap; the other two need an explicit pass.
    let mut ordered: Vec<(String, Vec<&Proc>)> = groups.into_iter().collect();
    match sort {
        Sort::Name => ordered.sort_by(|a, b| flip(a.0.cmp(&b.0), descending)),
        Sort::Cpu => ordered.sort_by(|a, b| {
            let total = |ps: &Vec<&Proc>| ps.iter().filter_map(|p| rate_of(p.pid)).map(|r| r.cpu_percent).sum::<f32>();
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
            Sort::Memory => members.sort_by(|a, b| flip(a.rss_bytes.cmp(&b.rss_bytes), descending).then_with(|| a.pid.cmp(&b.pid))),
            Sort::Name => members.sort_by(|a, b| flip(a.comm.cmp(&b.comm), descending).then_with(|| a.pid.cmp(&b.pid))),
        }

        let expanded = !collapsed.contains(&name);
        rows.push(Node::ProcGroup {
            // Aggregates are sums over the group's own processes. A
            // cgroup path is hierarchical, but these are not rolled up
            // through the hierarchy: nesting parents inside parents is
            // the full tree the design's mockup shows, and it needs a
            // path-aware builder rather than this flat one.
            cpu_percent: members.iter().filter_map(|p| rate_of(p.pid)).map(|r| r.cpu_percent).sum(),
            mem_bytes: members.iter().map(|p| p.rss_bytes).sum(),
            read_bytes_per_sec: members.iter().filter_map(|p| rate_of(p.pid)).map(|r| r.io_read_bytes_per_sec).sum(),
            write_bytes_per_sec: members.iter().filter_map(|p| rate_of(p.pid)).map(|r| r.io_write_bytes_per_sec).sum(),
            proc_count: members.len() as u32,
            // Empty until a sample ring exists to draw from - the design
            // lists its depth as an open question.
            sparkline: Vec::new(),
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
                    age_ms: (proc.started_at_ms > 0 && now_ms >= proc.started_at_ms).then(|| now_ms - proc.started_at_ms),
                    proc: proc.clone(),
                    depth: 1,
                    expanded: detail.is_some(),
                }];
                if let Some(detail) = detail {
                    rows.push(Node::ProcDetail { proc: proc.clone(), detail: detail.clone().map(Box::new) });
                }
                rows
            }));
        }
    }
    rows
}

/// Reverses an ordering when the column is pointing the other way.
///
/// Applied to the comparison rather than by reversing the sorted list,
/// so the stable tie-break underneath keeps pointing the same way: a
/// reversed list would flip that too, and rows with equal values would
/// swap places every time the direction changed.
fn flip(ordering: std::cmp::Ordering, descending: bool) -> std::cmp::Ordering {
    if descending { ordering.reverse() } else { ordering }
}
