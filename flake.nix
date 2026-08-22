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
    in
    {
      devShells = forEach (pkgs: {
        default = pkgs.mkShell {
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

          # ~/.cargo/config.toml on a contributor's machine may name a
          # wrapper or a linker this shell does not provide. Cargo's env
          # var wins over the config file, and empty means "no flags",
          # which is what makes the shell reproducible rather than a
          # function of whatever is in the caller's home directory.
          RUSTFLAGS = "";
        };
      });
    };
}
