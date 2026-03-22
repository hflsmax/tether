{
  description = "Tether — persistent PTY session manager";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, crane, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Rust toolchain with macOS cross-compilation targets
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          targets = [ "aarch64-apple-darwin" "x86_64-apple-darwin" ];
        };
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        src = craneLib.cleanCargoSource ./.;

        commonArgs = {
          inherit src;
          strictDeps = true;
          buildInputs = [ ];
          nativeBuildInputs = [ pkgs.pkg-config ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Native build (all binaries — daemon, proxy, client)
        tether = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
        });

        # macOS client cross-compilation via cargo-zigbuild
        tether-client-darwin = target:
          pkgs.stdenv.mkDerivation {
            pname = "tether-client-${target}";
            version = "0.1.0";
            inherit src;
            nativeBuildInputs = [ rustToolchain pkgs.zig pkgs.cargo-zigbuild ];
            buildPhase = ''
              export HOME=$TMPDIR
              cargo zigbuild -p tether --target ${target} --release
            '';
            installPhase = ''
              mkdir -p $out/bin
              cp target/${target}/release/tether $out/bin/tether
            '';
          };
      in
      {
        packages = {
          default = tether;
          tether = tether;
          tether-client-aarch64-darwin = tether-client-darwin "aarch64-apple-darwin";
          tether-client-x86_64-darwin = tether-client-darwin "x86_64-apple-darwin";
        };

        checks = {
          inherit tether;
          tether-clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });
          tether-tests = craneLib.cargoNextest (commonArgs // {
            inherit cargoArtifacts;
          });
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          packages = with pkgs; [
            rust-analyzer
            cargo-watch
            cargo-nextest
            cargo-zigbuild
            zig
          ];
        };
      }
    ) // {
      nixosModules.default = import ./nix/module.nix self;
      homeManagerModules.default = import ./nix/home-module.nix self;
    };
}
