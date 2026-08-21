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
 [1] status  [2] procs  [3] systemd  [4] io  [g] refresh  [?] keys  [q] quit
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

## Views

| Key | View | What it answers |
|-----|------|-----------------|
| `1` | Status | What needs attention right now |
| `2` | Procs | What is running, grouped by the cgroup that owns it |
| `3` | systemd | What the units are doing, why, and what to do about it |
| `4` | IO | What the disks, filesystems and links are carrying |
| `l` | Log | What one unit said — reached from a unit, left with `esc` |

Keys are global only when they mean the same thing everywhere: `n`/`p`
move between sections, `tab` folds, `/` filters, `g` refreshes, `q` quits.
Everything else belongs to the view under the cursor, so `r` can mean
restart in one view without being spent in the others.

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
reloads, `alt+r` try-restarts, `e`/`D` enable and disable, `M`/`U` mask and
unmask, `F` clears a failed state, `E` opens the drop-in in `$EDITOR`.

**It tells you when an action will not stick.** On a declaratively managed
host — NixOS today — the five verbs that write under `/etc/systemd/system`
are dimmed and say so before you press them, because "enabled" that a
rebuild silently reverts is worse than no button at all.

**Logs read like logs.** One unit's journal, sectioned by local day, `o` to
flip oldest/newest first, and a sender's own timestamp stripped so the
time column is not printed twice. An open log follows within ~250 ms where
`libsystemd` can be loaded.

**Disk answers "why", not just "how full".** Opening a filesystem walks it
on a thread pool and fills in sizes as it counts. It agrees with `du`
byte for byte, stays on one filesystem, and finished `/home` (622 GB) in
1.9 s against `du`'s 8.6 s on the development host.

## Installing

```
cargo install masys
```

Or from a clone:

```
cargo build --release
./target/release/masys
```

Runs unprivileged. Everything it can read without privileges, it reads;
anything it cannot is reported as absent rather than as an error, and the
actions that need root will say so when they are refused.

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

## Configuration

`~/.config/masys/config.toml`, and only for keys:

```toml
[keys]
unit_restart = "r"
view_procs = "2"
filter = "/"
```

A binding that cannot be parsed leaves the default in place and says so on
startup: a typo should cost you the customisation, not the key.

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
host is 99 ms against a 2 s tick. One source is not: summing a directory
tree is seconds, so it lives behind a thread pool and a poll, which is the
exception the architecture names rather than one it stumbled into.

`masys-domain` and `masys-app` have no dev-dependencies. Their suites run
against fakes, so neither needs D-Bus, `systemctl` or `/proc` to be
tested.

## Status

Early. It runs, it is useful, and the shape is settled; the version says
0.1.0 and means it.

## License

MIT or Apache-2.0, at your option.
