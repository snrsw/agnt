{
  description = "agnt — TUI for managing local Claude Code and Codex sessions";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1"; # unstable Nixpkgs
    fenix = {
      url = "https://flakehub.com/f/nix-community/fenix/0.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, ... }@inputs:

    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forEachSupportedSystem =
        f:
        inputs.nixpkgs.lib.genAttrs supportedSystems (
          system:
          f {
            pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [
                inputs.self.overlays.default
              ];
            };
          }
        );
    in
    {
      overlays.default = final: prev: {
        rustToolchain =
          with inputs.fenix.packages.${prev.stdenv.hostPlatform.system};
          combine (
            with stable;
            [
              clippy
              rustc
              cargo
              rustfmt
              rust-src
            ]
          );
      };

      packages = forEachSupportedSystem (
        { pkgs }:
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "agnt";
            version = (pkgs.lib.importTOML ./Cargo.toml).package.version;
            src = pkgs.lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;

            # These drive a real shell on a PTY and watch it `cd`, which the
            # Nix build sandbox does not allow; the rest of the suite still runs.
            checkFlags = [
              "--skip=app::tests::exiting_the_shell_gives_the_text_area_back"
              "--skip=app::tests::keys_reach_the_embedded_shell_and_the_directory_follows_it"
              "--skip=shell::tests::a_real_shell_runs_on_the_pty_and_reports_its_directory"
            ];

            meta = {
              description = "TUI for managing local Claude Code and Codex sessions";
              homepage = "https://github.com/snrsw/agnt";
              mainProgram = "agnt";
            };
          };
        }
      );

      devShells = forEachSupportedSystem (
        { pkgs }:
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustToolchain
              cargo-edit
              cargo-watch
              rust-analyzer
            ];

            env = {
              # Required by rust-analyzer
              RUST_SRC_PATH = "${pkgs.rustToolchain}/lib/rustlib/src/rust/library";
            };
          };
        }
      );
    };
}
