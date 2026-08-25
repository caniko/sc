{
  description = "SmartCool — intelligent fan control daemon";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    nix-pklx.url = "git+https://github.com/caniko/nix-pklx.git";
    plinth = {
      url = "git+https://github.com/caniko/plinth";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.crane.follows = "crane";
      inputs.rust-overlay.follows = "rust-overlay";
    };

    rs-harbor = {
      url = "git+https://github.com/caniko/harbor-rs.git?ref=trunk&rev=05cc4f162b55fa904b687db1821e2463fa813e50";
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
    plinth,
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
        extra = [pkgs.pkg-config];
      };
      sccacheEnv = harbor.mkSccacheCraneEnv {
        enable = false;
        package = "${pkgs.sccache}/bin/sccache";
      };
      commonArgs =
        {
          src = ./.;
          strictDeps = true;
          pname = "smartcool";
          version = "0.1.0";
          SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
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
      docs = pkgs.stdenvNoCC.mkDerivation {
        pname = "smartcool-docs";
        version = "0.1.0";
        src = nixpkgs.lib.fileset.toSource {
          root = ./.;
          fileset = nixpkgs.lib.fileset.maybeMissing ./docs;
        };
        nativeBuildInputs = [pkgs.mdbook];
        phases = ["buildPhase" "installPhase"];
        buildPhase = ''
          cp -r --no-preserve=mode $src/docs docs
          mdbook build docs
        '';
        installPhase = ''
          cp -r docs/book $out
        '';
      };
      plinthProject = plinth.packages.${system}.plinth-project;
      projectSite = pkgs.stdenvNoCC.mkDerivation {
        pname = "smartcool-site";
        version = "0.1.0";
        src = nixpkgs.lib.fileset.toSource {
          root = ./.;
          fileset = nixpkgs.lib.fileset.unions [
            (nixpkgs.lib.fileset.maybeMissing ./website)
          ];
        };
        nativeBuildInputs = [plinthProject];
        phases = ["buildPhase" "installPhase"];
        buildPhase = ''
          cp -r --no-preserve=mode $src/website website
          plinth-project build --config website/plinth-project.toml --out public
        '';
        installPhase = ''
          mkdir -p $out
          cp -r public/. $out/
          mkdir -p $out/docs
          cp -r ${docs}/. $out/docs/
        '';
      };
    in rec {
      smartcool = buildPackage "smartcool";
      sc-nixgen = buildPackage "sc-nixgen";
      inherit docs;
      site = projectSite;
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
        extra = [pkgs.pkg-config];
      };
      sccacheEnv = harbor.mkSccacheCraneEnv {
        enable = false;
        package = "${pkgs.sccache}/bin/sccache";
      };
      commonArgs =
        {
          src = ./.;
          strictDeps = true;
          pname = "smartcool";
          version = "0.1.0";
          SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          inherit nativeBuildInputs;
        }
        // sccacheEnv;
      moduleChecks = import ./nix/module-tests.nix {
        inherit nixpkgs system;
        pkgs = pkgsFor system;
        smartcoolModule = self.nixosModules.default;
        package = packages.smartcool;
        pklx = inputs.nix-pklx.packages.${system}.pklx;
      };
      pklValidate = pkgs.runCommand "pkl-validate" {
        src = ./.;
        buildInputs = [
          inputs.nix-pklx.packages.${system}.pklx
          pkgs.cacert
          pkgs.coreutils
          pkgs.findutils
        ];
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
      } ''
        cd "$src"
        failures=0
        while IFS= read -r file; do
          echo "validating: $file"
          tmp="$(mktemp)"
          if ! pklx eval "$file" -o "$tmp" >/dev/null 2>&1; then
            echo "FAIL: $file" >&2
            failures=$((failures + 1))
          fi
          rm -f "$tmp"
        done < <(find . \
          -path ./target -prune -o \
          -path ./.git -prune -o \
          -name '*.pkl' -type f -print | sort)

        if [ "$failures" -gt 0 ]; then
          echo "ERROR: $failures Pkl file(s) failed validation" >&2
          exit 1
        fi

        touch "$out"
      '';
    in
      {
        inherit (packages) smartcool sc-nixgen;
        pkl-validate = pklValidate;
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
    overlays.default = final: _prev: mkPackages final.stdenv.hostPlatform.system;

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
      plinthProject = plinth.packages.${system}.plinth-project;
    in
      harbor.mkDevShells {
        inherit pkgs cross cargoConfig;
        inherit (toolchain) craneLib;
        enableOsxcrossEnv = false;
        packages = [
          inputs.nix-pklx.packages.${system}.pklx
          pkgs.mdbook
          plinthProject
          self.packages.${system}.sc-nixgen
        ];
      });

    nixosModules.default = import ./nix/module.nix;

    checks = forSystems mkChecks;
  };
}
