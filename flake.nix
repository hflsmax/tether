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
        rustToolchain = pkgs.rust-bin.stable.latest.default;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        commonArgs = {
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
          buildInputs = [ ];
          nativeBuildInputs = [ pkgs.pkg-config ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        tether = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
        });
      in
      {
        packages = {
          default = tether;
          tether = tether;
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
          ];
        };
      }
    ) // {
      nixosModules.default = import ./nix/module.nix self;
      homeManagerModules.default = import ./nix/home-module.nix self;
    };
}
