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
what they say. `e` opens the operations on what it found — the eighteen
`nixos-rebuild`, `nix`, `nix-env` and `nix-channel` invocations worth
having, grouped, with the generation under the cursor named in the group
that acts on it.

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
can be granted them. The NixOS operations that write a root-owned profile
- activating a generation, collecting the store - have no such mechanism:
`nix-env` and `nix-collect-garbage` take no elevation flag and raise no
polkit action. masys checks whether the profile directories can be
written and marks those rows where they cannot, rather than starting a
command that would delete some generations and then fail partway. Run
masys as root to use them.

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

## Developing

```
cargo test --workspace          # 677 tests, no D-Bus or /proc required
cargo fmt --all                 # rustfmt.toml widens the line budget past 100
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
host is 99 ms against a 2 s tick. One source is not: summing a directory
tree is seconds, so it lives behind a thread pool and a poll, which is the
exception the architecture names rather than one it stumbled into.

`masys-domain` and `masys-app` have no dev-dependencies. Their suites run
against fakes, so neither needs D-Bus, `systemctl` or `/proc` to be
tested.

## Status

Early. It runs, it is useful, and the shape is settled; the version says
0.1.0 and means it.

What is not done, in the order you are likely to meet it. A rebuild run
unprivileged builds for minutes and is then refused at activation:
`nixos-rebuild` can deploy as a non-root user through `--elevate`, which
masys does not yet pass, so run it as root for anything that activates -
`build` is the exception and works either way. Four operations the design
names are deliberately absent, each waiting on a question only the code
that reaches them can settle: `upgrade`, `repl`, `build-vm` and
`build-image`. And NixOS is the only platform adapter that answers
anything; every other host gets the fallback, which is honest about
knowing nothing rather than guessing, but it means one adapter is proving
a seam meant for several.

## License

MIT or Apache-2.0, at your option.
