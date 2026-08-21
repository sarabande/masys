/// The unit types systemd defines, all eleven of them.
///
/// The whole set rather than a useful subset: this is what the systemd
/// view groups by, so a type missing here does not become an unlabelled
/// section, it disappears into `Other` along with every other type that is
/// missing - which is how 89 devices, 13 slices, 4 swaps, 4 automounts, 2
/// scopes and 2 paths on one host all ended up in the same bucket.
///
/// `Other` remains for a type a future systemd adds. It is a real answer -
/// "systemd has a kind masys does not know" - and not a dumping ground.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitKind {
    Service,
    Socket,
    Target,
    Device,
    Mount,
    Automount,
    Swap,
    Path,
    Timer,
    Slice,
    Scope,
    Other,
}

impl UnitKind {
    /// The plural noun this type is listed under.
    pub fn plural(self) -> &'static str {
        match self {
            UnitKind::Service => "Services",
            UnitKind::Socket => "Sockets",
            UnitKind::Target => "Targets",
            UnitKind::Device => "Devices",
            UnitKind::Mount => "Mounts",
            UnitKind::Automount => "Automounts",
            UnitKind::Swap => "Swap",
            UnitKind::Path => "Paths",
            UnitKind::Timer => "Timers",
            UnitKind::Slice => "Slices",
            UnitKind::Scope => "Scopes",
            UnitKind::Other => "Other",
        }
    }

    /// The order the systemd view lists them in: what an operator acts on
    /// most, first. Services lead because they are what starts, stops and
    /// fails; devices trail because there are more of them than anything
    /// else and almost nothing can be done to one.
    pub const ORDER: &'static [UnitKind] = &[
        UnitKind::Service,
        UnitKind::Timer,
        UnitKind::Socket,
        UnitKind::Target,
        UnitKind::Mount,
        UnitKind::Automount,
        UnitKind::Swap,
        UnitKind::Path,
        UnitKind::Slice,
        UnitKind::Scope,
        UnitKind::Device,
        UnitKind::Other,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveState {
    Active,
    Reloading,
    Inactive,
    Failed,
    Activating,
    Deactivating,
}

/// A `.timer` unit's schedule.
///
/// Timers are the classic silent failure: a job that has been failing
/// every night looks like one failed service in a list of two hundred,
/// with nothing to say it is scheduled or that it has failed before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timer {
    /// When it next fires. `None` for a timer with no future elapse -
    /// systemd reports 0 for a monotonic-only timer whose reference point
    /// has passed, and for one that will not fire again.
    pub next_ms: Option<u64>,
    /// When it last fired. `None` if it never has, which for a timer
    /// installed long ago is itself worth noticing.
    pub last_ms: Option<u64>,
    /// The unit it activates - usually the same name with `.service`, but
    /// `Unit=` can point anywhere.
    pub activates: String,
}

/// One systemd unit, as `SystemService::units` reports it - raw state, no
/// triage judgement. `crate::triage` turns a slice of these into
/// `Finding`s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub name: String,
    pub kind: UnitKind,
    pub active_state: ActiveState,
    /// systemd's sub-state, e.g. `running`, `dead`, `exited` - shown
    /// alongside `active_state` since two failed units can fail
    /// differently.
    pub sub_state: String,
    /// Process exit code, when `active_state` is `Failed` and the unit ran
    /// at least one process. `None` for units that failed before exec (a
    /// bad unit file) or that were never a process at all (targets,
    /// mounts).
    pub exit_code: Option<i32>,
    pub enabled: bool,
    /// Every start systemd recorded within its own accounting window - raw
    /// material for flapping detection, not a derived count. Empty for a
    /// unit that hasn't restarted.
    pub restart_timestamps_ms: Vec<u64>,
    /// Unix milliseconds the unit entered its current `active_state`.
    pub since_ms: u64,
    pub cgroup: Option<String>,
    /// The slice this unit is in - systemd's own containment hierarchy,
    /// and the one the systemd view nests by. `None` for a unit type that
    /// cannot be in one (`.target`, `.timer`, `.device`, `.path`) and for
    /// the root slice itself.
    pub slice: Option<String>,
    /// `Some` only for `.timer` units.
    pub timer: Option<Timer>,
    /// The units this one starts - systemd's `Triggers`. A timer triggers
    /// its service, a path the unit watching it, an activation socket the
    /// service behind it. Empty for the units that start nothing, which is
    /// most of them.
    ///
    /// This is the second hierarchy in the view, and unlike the slice tree
    /// it is a *causal* one: it answers "what does pressing this do", not
    /// "what is inside this".
    pub triggers: Vec<String>,
}

/// What a unit shows when its row is opened - the facts `systemctl status`
/// leads with, which is the set `systemctl-tui` surfaces too.
///
/// Fetched on demand rather than during the poll. Every field here costs a
/// targeted D-Bus `Get`, and at 366 units that would be the poll's whole
/// budget several times over; for the one unit under the cursor it is
/// nothing. That is the trade the type exists to make explicit - it is
/// deliberately *not* on `Unit`.
///
/// Every field is optional because a unit type may simply not have it: a
/// `.target` has no main pid, a `.device` no memory accounting, and a unit
/// with no fragment on disk no path. `None` means "this kind of unit does
/// not have one", which the renderer omits rather than printing as zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitDetail {
    /// The one-line human description - `SSH Daemon`.
    pub description: Option<String>,
    /// The unit file this was loaded from.
    pub fragment_path: Option<String>,
    pub main_pid: Option<u32>,
    pub tasks: Option<u64>,
    pub memory_bytes: Option<u64>,
    /// Total CPU consumed, in nanoseconds, as systemd accounts it.
    pub cpu_nsec: Option<u64>,
    pub cgroup: Option<String>,
    /// Why the unit last stopped - `success`, `exit-code`, `timeout`,
    /// `signal`, `oom-kill`, `core-dump`. The single most useful word on a
    /// failed unit, and the one `systemctl status` puts in its headline.
    pub result: Option<String>,
    /// Man pages and URLs the unit declares.
    pub documentation: Vec<String>,
    /// The high-water mark, beside the current figure - a unit sitting at
    /// 4M having peaked at 900M is a different unit from one that never
    /// moved.
    pub memory_peak_bytes: Option<u64>,
    /// The task limit this unit runs under, for the `n of limit` reading
    /// `systemctl status` gives.
    pub tasks_max: Option<u64>,
    /// Bytes this unit has actually read from and written to storage, as
    /// systemd accounts them.
    pub io_read_bytes: Option<u64>,
    pub io_write_bytes: Option<u64>,
    /// Bytes in and out, when IP accounting is on for the unit. `None`
    /// when it is off, which is a different fact from zero traffic.
    pub ip_ingress_bytes: Option<u64>,
    pub ip_egress_bytes: Option<u64>,
}
