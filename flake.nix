{
  description = "SmartCool — intelligent fan control daemon";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    ronix.url = "git+https://codeberg.org/caniko/ronix.git";

    rs-harbor = {
      url = "git+ssh://git@codeberg.org/caniko/rs-harbor.git?ref=trunk";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.crane.follows = "crane";
    };

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs @ {
    self,
    nixpkgs,
    ronix,
    rs-harbor,
    rust-overlay,
    ...
  }: let
    systems = [
      "x86_64-linux"
      "aarch64-linux"
    ];
    forSystems = nixpkgs.lib.genAttrs systems;

    pkgsFor = system:
      import nixpkgs {
        inherit system;
        overlays = [
          (import rust-overlay)
          self.overlays.default
        ];
      };

    rawPkgsFor = system:
      import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };

    mkPackages = system: let
      pkgs = rawPkgsFor system;
      harbor = rs-harbor.lib;
      toolchain = harbor.mkToolchain {inherit pkgs;};
      craneLib = toolchain.craneLib;
      nativeBuildInputs = harbor.mkRustNativeBuildInputs {
        inherit pkgs;
        extra = [pkgs.pkg-config pkgs.sccache];
      };
      sccacheEnv = harbor.mkSccacheCraneEnv {
        enable = true;
        package = "${pkgs.sccache}/bin/sccache";
      };
      commonArgs =
        {
          src = ./.;
          strictDeps = true;
          pname = "smartcool";
          version = "0.1.0";
          inherit nativeBuildInputs;
        }
        // sccacheEnv;
      cargoArtifacts = craneLib.buildDepsOnly commonArgs;
      buildPackage = package:
        craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            cargoExtraArgs = "--package ${package}";
          }
        );
    in rec {
      smartcool = buildPackage "smartcool";
      sc-nixgen = buildPackage "sc-nixgen";
      default = smartcool;
    };

    mkChecks = system: let
      pkgs = rawPkgsFor system;
      harbor = rs-harbor.lib;
      toolchain = harbor.mkToolchain {inherit pkgs;};
      craneLib = toolchain.craneLib;
      packages = mkPackages system;
      nativeBuildInputs = harbor.mkRustNativeBuildInputs {
        inherit pkgs;
        extra = [pkgs.pkg-config pkgs.sccache];
      };
      sccacheEnv = harbor.mkSccacheCraneEnv {
        enable = true;
        package = "${pkgs.sccache}/bin/sccache";
      };
      commonArgs =
        {
          src = ./.;
          strictDeps = true;
          pname = "smartcool";
          version = "0.1.0";
          inherit nativeBuildInputs;
        }
        // sccacheEnv;
      moduleChecks = import ./nix/module-tests.nix {
        inherit nixpkgs system;
        pkgs = pkgsFor system;
        smartcoolModule = self.nixosModules.default;
        package = packages.smartcool;
      };
    in
      {
        inherit (packages) smartcool sc-nixgen;
        cargo-test = craneLib.cargoTest (commonArgs // {cargoArtifacts = null;});
        cargo-clippy = craneLib.cargoClippy (
          commonArgs
          // {
            cargoArtifacts = null;
            cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
          }
        );
        cargo-fmt = craneLib.cargoFmt {src = ./.;};
      }
      // moduleChecks;
  in {
    overlays.default = final: _prev: mkPackages final.system;

    packages = forSystems (system: mkPackages system);

    devShells = forSystems (system: let
      pkgs = pkgsFor system;
      harbor = rs-harbor.lib;
      toolchain = harbor.mkToolchain {inherit pkgs;};
      cross = harbor.mkCross {
        inherit pkgs system;
        enableOsxcross = false;
      };
      cargoConfig = harbor.mkCargoConfig {
        inherit pkgs;
        channel = "nightly";
      };
    in
      harbor.mkDevShells {
        inherit pkgs cross cargoConfig;
        inherit (toolchain) craneLib;
        enableOsxcrossEnv = false;
        packages = [
          self.packages.${system}.sc-nixgen
        ];
      });

    lib = ronix.lib;

    nixosModules.default = import ./nix/module.nix {ronixLib = ronix.lib;};

    checks = forSystems mkChecks;
  };
}
