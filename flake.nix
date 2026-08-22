{
  description = "masys - a magit-style TUI for Linux system runtime ops";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      # Linux only, and not as a limitation: masys reads /proc, talks to
      # systemd and reports on a Linux machine's runtime. There is nothing
      # for it to do on another kernel.
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forEach = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # `CARGO_TARGET_<TRIPLE>_LINKER`, with the triple spelled the way
      # cargo wants it: upper case, hyphens as underscores.
      linkerVar = pkgs:
        "CARGO_TARGET_" + nixpkgs.lib.toUpper
          (builtins.replaceStrings [ "-" ] [ "_" ] pkgs.stdenv.hostPlatform.config) + "_LINKER";
    in
    {
      devShells = forEach (pkgs: {
        default = pkgs.mkShell ({
          # The workspace needs 1.88, a floor its dependencies set rather
          # than masys: ratatui 0.30.2 declares 1.88 and zbus 5.19 declares
          # 1.87. nixpkgs is well past that, so the version is not pinned
          # here - pinning would mean an extra input for a floor nothing
          # is close to.
          packages = [
            pkgs.cargo
            pkgs.rustc
            # Both are what CI runs, and neither ships with cargo. Their
            # absence is why a clippy job that had never run once found
            # eleven warnings the day it did.
            pkgs.clippy
            pkgs.rustfmt
            # For linking. The build needs no systemd headers and no
            # pkg-config: libsystemd is opened at runtime, never linked.
            pkgs.gcc
          ];

          # masys reads the journal by spawning `journalctl -o json`, which
          # works everywhere and cannot follow. Where libsystemd can be
          # dlopened it reads in-process instead and an open log updates as
          # lines are written - and `cargo test -p masys-systemd` only
          # exercises that path when it can find the library.
          #
          # nixpkgs' own systemd rather than /run/current-system, so this
          # resolves on any host the shell runs on rather than on NixOS
          # alone.
          MASYS_LIBSYSTEMD = "${pkgs.systemd}/lib/libsystemd.so.0";

          # `~/.cargo/config.toml` on a contributor's machine may name a
          # wrapper, a linker or flags this shell does not provide, and
          # cargo reads it wherever the build runs. Each env var below
          # wins over the config key it shadows, and empty means "none" -
          # which is what makes the shell reproducible rather than a
          # function of whatever is in the caller's home directory.
          #
          # All three are needed and one is not enough: `RUSTFLAGS` covers
          # `rustflags`, and leaves `rustc-wrapper` and `linker` in force.
          # A clone with no `.cargo/config.toml` of its own found that out
          # - the shell died on a global `sccache` path that does not
          # exist, having cleared only the flags.
          RUSTFLAGS = "";
          CARGO_BUILD_RUSTC_WRAPPER = "";
        } // {
          # Built separately because the name is computed: `//` binds to
          # `mkShell`'s *result* without the parentheses around the whole
          # argument, which silently drops it.
          ${linkerVar pkgs} = "cc";
        });
      });
    };
}
