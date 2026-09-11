# masys

A magit-style TUI for Linux system runtime ops.

`systemctl-tui` manages units well. `btop` and `htop` monitor resources
well. Nothing does both, and nothing applies magit's actual organising
idea — a status buffer that shows **what needs attention** rather than a
dashboard that shows everything.

A healthy machine produces a nearly empty screen:

```
 masys - devbox                                          2026-08-20 11:51

 System
   . NixOS 26.11 (Zokor)  .  linux 6.18.42 x86_64
   . Intel(R) Core(TM) i7-6700 CPU @ 3.40GHz  .  8 cores
   . running  .  366 units  .  load 3.85 4.61 2.86  .  up 2d 14h
   . mem 11G/31G  .  zram 97%  .  swap 105M free
   . clock synced

 --------------------------------------------------------------------------
 [/] filter
 [1] status  [2] procs  [3] systemd  [4] io  [5] nix  [g] refresh  [?] keys  [q] quit
```

An unhealthy one puts the problems under it, worst first:

```
 System
   . NixOS 26.11 (Zokor)  .  linux 6.18.42 x86_64
   . running  .  366 units  .  load 2.25 4.10 2.76  .  up 2d 14h

 Failed units (1)
   x restic-backup.service  failed (exit 6)  1h ago  curl: (6) Could not resolve ho

 Pressure (1)
   ^ io  some avg60  31.4%      full avg60  12.2%

 Disk (1)
   ^ /  91%   82G free   .  412 generations
```

## Buffers

| Key | Buffer | What it answers |
|-----|--------|-----------------|
| `1` | Status | What needs attention right now |
| `2` | Procs | What is running, grouped by the cgroup that owns it |
| `3` | systemd | What the units are doing, why, and what to do about it |
| `4` | IO | What the disks, filesystems and links are carrying |
| `5` | Nix | What the running system and its declared configuration disagree about |
| `l` | Log | What one unit said — reached from a unit, left with `esc` |

`5` exists only on a host with a declarative platform adapter — NixOS
today. On Debian or Arch it is unbound and `?` does not list it: there is
no declared configuration for the buffer to compare the machine against, so
there is nothing there for a digit to open.

It carries the same triage thesis as the top of this file: a host with no
pending reboot, a fresh lock and a healthy store reduces to four quiet
lists. This one has a reboot pending, and says whether it can wait:

```
 Nix

 ! reboot pending
     booted 427 . current 438 . kernel unchanged
     activation-only - no kernel or initrd change, this can wait

 Store
   /nix 83%  .  155G free
   37 system generations  .  5 home  .  77 gc roots
   . gc        keep 14d  last 4d 13h ago   next in 2d 2h
   . optimise            last 13h 30m ago  next in 6h 10m

 System generations (37)
   * 438  current       38m  26.11.20260804.e72e4f2  6.18.42
     427  booted      4d 1h  26.11.20260804.e72e4f2  6.18.42

 Inputs (6)  lock e72e4f2 . running e72e4f2 . in sync
     nixpkgs         17d  2026-08-04  NixOS/nixpkgs
   ^ flake-compat   235d  2025-12-29  edolstra/flake-compat
```

masys reads the profile symlinks and the flake lock directly, and shows
what they say. Seven keys reach the operations on what it found — the
eighteen `nixos-rebuild`, `nix`, `nix-env` and `nix-channel` invocations
worth having. `s` switches, and `h` switches a standalone home-manager —
each reached for often enough to earn a letter of its own. `b`, `a`, `i`,
`c` and `f` open a small popup per family — rebuild, generation, inputs,
store and search — with the generation under the cursor named in the
group that acts on it.

Rows that cannot act here are **marked and still listed**, each with the
reason: `no channels on this host` on a flake machine, `activated by
nixos-rebuild switch` where home-manager is a module, `profile is
read-only - run masys as root` where it is. Nothing is hidden, because a
row that vanishes teaches nothing and a row that says why teaches
something true about the machine.

Anything that changes the system asks first, and anything that takes the
terminal gets it: a rebuild streams its own output for as long as it
runs, rather than being captured and shown once it has finished.

Keys are global only when they mean the same thing everywhere: `n`/`p`
move between sections, `tab` folds, `/` filters, `g` refreshes, `q` quits.
Everything else belongs to the buffer under the cursor, so `r` can mean
restart in one buffer without being spent in the others.

Press `?` for the full map, which is generated from the live keymap rather
than written out — a key that has been rebound shows its new chord.

## What it does that a dashboard does not

**Triage, not gauges.** The status buffer is a list of findings — a failed
unit, a flapping one, a filesystem filling, sustained IO pressure, an OOM
kill, a clock that is not synchronised. Nothing appears because it exists;
things appear because they are wrong.

**A failed unit explains itself.** The row carries the last thing the unit
actually said, from its own journal, so the common case needs no second
step.

**Actions where the fault is.** `r` restarts, `S`/`X` start and stop, `R`
reloads, `alt+r` try-restarts, `D` disables, `M`/`U` mask and unmask, `F`
clears a failed state, `E` opens the drop-in in `$EDITOR`. `e` opens a
transient on the row under the cursor with the rest, grouped and
annotated.

**It tells you when an action will not stick.** On a declaratively managed
host — NixOS today — the verbs that write under `/etc/systemd/system` say
so before you press them, because "enabled" that a rebuild silently
reverts is worse than no button at all. `D`, `E`, `M` and `U` carry the
mark in the footer; `enable` carries it on its own row in the transient,
with the reason beside it. Marked, never hidden: the key still works, and
what it costs you is stated rather than taken away.

**Logs read like logs.** One unit's journal, sectioned by local day, `o` to
flip oldest/newest first, and a sender's own timestamp stripped so the
time column is not printed twice. An open log follows within ~250 ms where
`libsystemd` can be loaded.

**Disk answers "why", not just "how full".** Opening a filesystem walks it
on a thread pool and fills in sizes as it counts. It agrees with `du`
byte for byte, stays on one filesystem, and finished `/home` (622 GB) in
1.9 s against `du`'s 8.6 s on the development host.

## Installing

From a clone:

```
cargo build --release
./target/release/masys
```

`cargo install masys` once the crates are published.

Runs unprivileged. Everything it can read without privileges, it reads;
anything it cannot is reported as absent rather than as an error.

Acting is a different question, and masys answers it per action. Unit
verbs shell out to `systemctl`, which raises polkit, so an ordinary user
can be granted them.

Rebuilds raise privilege themselves. Every verb that reaches
`switch-to-configuration` - `switch`, `boot`, `test`, `dry-activate` and
`rollback` - is handed `--elevate` when masys is not root, so the build
runs as you and only the activation half asks for anything.

It asks through `sudo`, on the terminal masys has just given up for the
rebuild's own output - so the prompt appears where you are already
looking. That is the whole reason for the choice. `run0` would authorise
through polkit, which sounds tidier and asks wherever the session's
polkit agent happens to be: on a desktop that is a window somewhere
behind the terminal, and over SSH or on a bare TTY there is no agent to
ask at all. masys falls back to `run0` only where there is no `sudo`,
since a prompt you have to go and find still beats none. `build` is the
exception and needs nothing: it stops at a store path.

The flag belongs to `nixos-rebuild-ng`, so masys asks your
`nixos-rebuild` whether it takes one before passing it, and looks on
`PATH` for the method before naming it. Where either answer is no,
nothing is passed and the old behaviour stands.

The operations that write a root-owned profile - activating a
generation, deleting some, collecting the store - ask for root too, and
by the same preference. `nix-env` and `nix-collect-garbage` take no
elevation flag, so masys runs them *through* `sudo` rather than passing
one. Those rows say `asks for root` before you press them.

Asked per profile, not per user: `~/.local/state/nix/profiles` is yours,
so deleting a home-manager generation needs nothing, and masys does not
prompt for a privilege the act will not use. `clean` is the exception and
asks regardless, because `nix-collect-garbage` acts on every profile it
finds and the system one is never yours.

On a host with neither `sudo` nor `run0` those rows stay marked *run
masys as root*, which is what they all used to say.

### Optional: following logs live

masys reads the journal by spawning `journalctl -o json`, which works
everywhere and cannot follow. Where `libsystemd` can be `dlopen`ed it
reads in-process instead and an open log updates as lines are written.

It is loaded at runtime, never linked: the build needs no systemd headers
and no `pkg-config`, and `ldd` on the binary shows no `libsystemd`. The
soname resolves on Debian, Ubuntu, Fedora, RHEL, Arch and SUSE. On NixOS,
which keeps no global library path, point masys at it:

```
MASYS_LIBSYSTEMD=/run/current-system/sw/lib/libsystemd.so.0 masys
```

Without it, everything works and logs refresh on the ordinary tick.

### Optional: SMART disk health

The overview shows `smart ok` or `smart failing` where masys can ask the
disks and every one of them answered. Asking means running `smartctl` from
smartmontools, which most hosts do not have installed and which usually
needs root.

Where it cannot ask, the segment is **absent rather than reassuring**. No
tool, no permission, a virtual disk, a disk in standby, one disk of three
unreadable: each of those draws nothing, because a disk nobody asked must
not read the same as a disk that answered and is fine. So an absent
segment means "not asked", never "healthy" — and if you expect the reading
and do not see it, install smartmontools and check masys can run it.

## Configuration

`~/.config/masys/config.toml`, for keys, for what counts as a problem,
and for what it is drawn in.

```toml
[keys]
unit_restart = "r"
buffer_procs = "2"
filter = "/"

[thresholds]
disk_used_percent = 90          # default 85
inode_used_percent = 90
psi_some_avg60_percent = 20     # the fraction of wall-clock time tasks
psi_full_avg60_percent = 5      # spent blocked, over the last minute
flapping_restart_count = 3      # restarts within the window below
flapping_window_ms = 3_600_000
thermal_throttled_percent = 10  # share of a tick a core spent held
                                # below the clock it asked for

[theme]
section_header = "white"        # section headings and the footer's keys
severity_dead = "red"           # x - a unit that has stopped
severity_warning = "yellow"     # ~ ^ - flapping, pressure, capacity
severity_urgent = "light-red"   # ! - clock, OOM, degraded, kernel
info = "dark-gray"              # . - the System section's plain facts
status_error = "red"            # the status line when something failed
```

All three tables are optional, and every value shown is its default
except `disk_used_percent`, so the file above changes exactly one thing.
The threshold and colour keys are the field names triage and the renderer
read, on purpose: a friendlier spelling would be a mapping between what
you write and what the code uses, and a mapping is a thing that can
drift.

**The palette is not a preference.** Severity *is* colour here: the glyph
carries the shape and the colour carries the judgement, and two of the
defaults are `light-red` and `dark-gray` - the two most likely to be
unreadable on a terminal you configured yourself. A colour is one of the
sixteen terminal names - `black`, `white`, `gray`, `dark-gray`, and
`red`, `green`, `yellow`, `blue`, `magenta`, `cyan` each with a `light-`
form - or a 0-255 palette index like `"208"`, or `"#ff8800"`. The names
are the ones that respect a terminal's own configuration; the other two
spellings are for saying which entry you actually mean. There are no
named palettes to choose from, for the reason there is no colemak preset
for `[keys]`: the mechanism is what earns its keep, and a curated set of
themes is a second decision nobody has asked for.

A `[theme]` table naming three colours moves three; the rest of the
palette stays exactly as it is.

Anything masys will not take leaves that one setting at its default and
says so on startup - a typo should cost you the customisation, not the
key or the check. That includes a misspelled threshold, which is reported
rather than ignored: a `disk_used_percentage` that silently did nothing
would leave you believing you had relaxed a check you had not. A
percentage outside 0 to 100 is refused rather than clamped, because 150
clamped to 100 is a check that never fires, reported as one that was
accepted.

## Developing

```
cargo test --workspace          # 963 tests, no D-Bus or /proc required
cargo fmt --all                 # stock rustfmt; the tree carries no config
cargo clippy --workspace --all-targets
```

There is a Nix dev shell if you want one - `nix develop` - carrying the
toolchain, `clippy` and `rustfmt`, and pointing `MASYS_LIBSYSTEMD` at a
`libsystemd` so the `dlopen` path is exercised rather than skipped. It
also clears `RUSTFLAGS`, so a `~/.cargo/config.toml` naming a wrapper or
a linker the shell does not provide cannot make the build a function of
your home directory. Nothing here requires it; the three commands above
work on any toolchain at or past the floor.

CI runs those three with `RUSTFLAGS: -D warnings`, so a warning fails the
build. The workspace needs **Rust 1.88**, and cargo says so itself before
it builds anything - the floor is set by dependencies rather than by
masys: ratatui 0.30.2 declares 1.88 and zbus 5.19 declares 1.87.

`masys-domain` and `masys-app` have **no dev-dependencies**. Their suites
run against fake `SystemService`/`PlatformService` doubles, so neither
needs D-Bus, `systemctl` or `/proc` to be tested, and neither can quietly
grow a dependency on the infrastructure beneath it. Anything that needs a
real machine belongs in `masys-systemd/tests` or `masys-platform-*/tests`,
where the suites skip rather than fail when the bus is absent.

The dependency direction in the table below is an invariant, not a
description, and `crates/masys/tests/layering.rs` enforces it: the domain
depends on `thiserror` alone, the view on the domain alone, nothing above
the composition root names a terminal library, and only the `masys` binary
may name a platform adapter. Break one and the suite says which rule and
why.

## Architecture

Ports and adapters, one concern per crate.

| Crate | Depends on | Holds |
|-------|-----------|-------|
| `masys-domain` | `thiserror` | Entities, the port traits, triage rules. No I/O. |
| `masys-view` | domain | The row model the renderer draws. No decisions. |
| `masys-app` | domain, view | Session state, keymap, outline building. No I/O. |
| `masys-render` | domain, view, ratatui | Turns a view into a frame. |
| `masys-systemd` | domain, zbus, libc | systemd, `/proc`, the journal. |
| `masys-scan` | domain, rayon, libc | Directory sizing. |
| `masys-platform-*` | domain | Per-distribution facts. |
| `masys` | everything | The composition root and the event loop. |

The event loop is single-threaded with no channels. That is affordable
because every source is cheap against its cadence — a full sample of this
host is 99 ms against a 2 s tick. Two sources are not, and both are named
exceptions rather than ones the architecture stumbled into. Summing a
directory tree is seconds, so it lives behind a thread pool and a poll.
A SMART check spawns one `smartctl` per disk, so it is held for five
minutes rather than taken per tick. Measured: 19 ms for this host's one
**SATA SSD**, against 45 ms for the readings every tick takes anyway — so
the tick that carries it costs half again as much, and still sits far
inside the 2 s cadence. The disks are asked in turn, so four cost four
times one on hardware like this. A disk with **platters** was not
measured and this host has none, though `-n standby` means a spun-down
one answers without being woken, which is the case that would have cost
seconds rather than milliseconds. Without root, or without smartmontools,
the read fails in 14 ms or 0.3 ms and answers *unknown* — which is why
this cost is invisible on a development machine, and why it had to be
measured deliberately.

`masys-domain` and `masys-app` have no dev-dependencies. Their suites run
against fakes, so neither needs D-Bus, `systemctl` or `/proc` to be
tested.

## Status

Early. It runs, it is useful, and the shape is settled; the version says
0.1.0 and means it.

What is not done, in the order you are likely to meet it.

**masys reads deeply and acts narrowly.** Every buffer's readings are
built out — the Nix buffer alone takes nine — while the action tables are
sparse outside units and Nix. The sharpest form of that is the Status
buffer: it names what is wrong and gives you nothing to press. A finding
is a row you read, not a row you act on or follow to where the trouble
is.

**The Network buffer is two thirds of itself.** Interfaces and throughput
are there, as a section of the IO buffer. Listening ports are not: you
can see what a process has open by opening that process, and there is no
way to ask the question from the port end.

**Some designed per-row actions have no key**: a signal picker for
processes beyond `k`/`K`, `ionice`, resource limits, jumping from a unit
to its processes, and filtering the journal to the unit under the cursor.

**Eight mutating Nix operations have never been run against a live
host** — `switch`, `boot`, `test`, `rollback`, both `nix-env` generation
verbs, `nix flake update` and `nix-collect-garbage`. Their commands are
checked against their tools' synopses and their behaviour is not. The two
defects this project has found that way — an operation refused after
building for minutes, and one that cannot work on a flake host — were
both invisible to every other kind of check.

Two platform adapters answer: NixOS, and Debian and its derivatives. Every
other host gets the fallback, which is honest about knowing nothing rather
than guessing. The Debian adapter is deliberately small — a pending
reboot, unit ownership, and what is sitting in `/boot` — and it earned its
place by disagreeing with the port: `BootPressure::generations` was a
`u32` until a distro with no generations had to answer it, and would have
reported `0` where the renderer prints a count.

The NixOS adapter is still much the deeper of the two, and the port has
now been argued with by exactly one distro that answers differently.

## License

MIT or Apache-2.0, at your option.
