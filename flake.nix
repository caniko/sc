{
  description = "SmartCool — intelligent fan control daemon";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    ronix.url = "git+https://codeberg.org/caniko/ronix.git";
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    ronix,
    ...
  }: let
    forSystems = nixpkgs.lib.genAttrs [
      "x86_64-linux"
      "aarch64-linux"
    ];
    pkgsFor = system: nixpkgs.legacyPackages.${system};
    craneLibFor = system: crane.mkLib (pkgsFor system);

    # Shared cargo artifacts for the whole workspace
    commonArgsFor = system: let
      craneLib = craneLibFor system;
    in {
      src = craneLib.cleanCargoSource ./.;
      strictDeps = true;
    };

    cargoArtifactsFor = system: let
      craneLib = craneLibFor system;
    in
      craneLib.buildDepsOnly (commonArgsFor system);

    # sc daemon binary
    scFor = system: let
      craneLib = craneLibFor system;
    in
      craneLib.buildPackage (
        (commonArgsFor system)
        // {
          cargoArtifacts = cargoArtifactsFor system;
          cargoExtraArgs = "--package smartcool";
        }
      );

    # sc-nixgen CLI binary
    nixgenFor = system: let
      craneLib = craneLibFor system;
    in
      craneLib.buildPackage (
        (commonArgsFor system)
        // {
          cargoArtifacts = cargoArtifactsFor system;
          cargoExtraArgs = "--package sc-nixgen";
        }
      );
  in {
    packages = forSystems (system: {
      default = scFor system;
      sc-nixgen = nixgenFor system;
    });

    devShells = forSystems (
      system: let
        pkgs = pkgsFor system;
      in {
        default = pkgs.mkShell {
          packages = [self.packages.${system}.sc-nixgen];
        };
      }
    );

    lib = ronix.lib;

    nixosModules.default = import ./nix/module.nix {ronixLib = ronix.lib;};
  };
}
