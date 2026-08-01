{
  description = "SmartCool — intelligent fan control daemon";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    nix-pklx.url = "git+https://codeberg.org/caniko/nix-pklx.git";
    plinth = {
      url = "git+https://codeberg.org/caniko/plinth";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.crane.follows = "crane";
      inputs.rust-overlay.follows = "rust-overlay";
    };

    rs-harbor = {
      url = "git+https://codeberg.org/caniko/rs-harbor.git?ref=trunk&rev=9bfa8bdb0ecb22d7bc11448665f7fbaebae7a759";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.crane.follows = "crane";
      inputs.rust-overlay.follows = "rust-overlay";
    };

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs @ {
    self,
    nixpkgs,
    plinth,
    rs-harbor,
    rust-overlay,
    treefmt-nix,
    git-hooks,
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
      buildCache = harbor.mkBuildCachePolicy {
        inherit pkgs;
        buildPackageSet = pkgs.buildPackages;
        sccachePackage = rs-harbor.packages.${system}.sccache;
        cacheRoot = "/tmp/sccache";
        namespaceScope = "canix-rust";
        namespaceGeneration = 5;
      };
      cacheRust = package: buildCache.withRustCache {inherit package;};
      commonArgs = {
        src = craneLib.cleanCargoSource ./.;
        rsHarborCargoTomlContents = builtins.readFile ./Cargo.toml;
        strictDeps = true;
        pname = "smartcool";
        version = "0.1.0";
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        inherit nativeBuildInputs;
      };
      cargoArtifacts = cacheRust (craneLib.buildDepsOnly commonArgs);
      buildPackage = package:
        cacheRust (craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            pname = package;
            cargoExtraArgs = "--package ${package}";
            meta.mainProgram =
              if package == "smartcool"
              then "sc"
              else package;
          }
        ));
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
      buildCache = harbor.mkBuildCachePolicy {
        inherit pkgs;
        buildPackageSet = pkgs.buildPackages;
        sccachePackage = rs-harbor.packages.${system}.sccache;
        cacheRoot = "/tmp/sccache";
        namespaceScope = "canix-rust";
        namespaceGeneration = 5;
      };
      cacheRust = package: buildCache.withRustCache {inherit package;};
      commonArgs = {
        src = craneLib.cleanCargoSource ./.;
        rsHarborCargoTomlContents = builtins.readFile ./Cargo.toml;
        strictDeps = true;
        pname = "smartcool";
        version = "0.1.0";
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        inherit nativeBuildInputs;
      };
      cargoArtifacts = cacheRust (craneLib.buildDepsOnly commonArgs);
      treefmtEval = treefmt-nix.lib.evalModule pkgs (import ./nix/treefmt.nix);
      pre-commit-check = git-hooks.lib.${system}.run {
        src = ./.;
        hooks = import ./nix/pre-commit.nix {
          inherit pkgs;
          treefmtWrapper = treefmtEval.config.build.wrapper;
        };
      };
      moduleChecks = import ./nix/module-tests.nix {
        inherit nixpkgs system;
        pkgs = pkgsFor system;
        smartcoolModule = self.nixosModules.default;
        package = packages.smartcool;
        pklx = inputs.nix-pklx.packages.${system}.pklx;
      };
      pklValidate =
        pkgs.runCommand "pkl-validate" {
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
    in {
      inherit (packages) smartcool sc-nixgen;
      pkl-validate = pklValidate;
      cargo-test = cacheRust (craneLib.cargoTest (
        commonArgs
        // {
          inherit cargoArtifacts;
          cargoTestExtraArgs = "--workspace --all-features";
        }
      ));
      cargo-clippy = cacheRust (craneLib.cargoClippy (
        commonArgs
        // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "--workspace --all-targets --all-features -- -D warnings";
        }
      ));
      cargo-fmt = craneLib.cargoFmt {src = ./.;};
      formatting = treefmtEval.config.build.check self;
      module-daemon = moduleChecks.module-daemon;
      module-asusd = moduleChecks.module-asusd;
      module-asusd-assertions = moduleChecks.module-asusd-assertions;
      cli-runnable = pkgs.runCommand "smartcool-cli-runnable" {} ''
        ${pkgs.lib.getExe packages.smartcool} --help >/dev/null
        ${pkgs.lib.getExe packages.sc-nixgen} --help >/dev/null
        touch $out
      '';
      pre-commit = pre-commit-check;
    };
  in {
    overlays.default = final: _prev: mkPackages final.stdenv.hostPlatform.system;

    packages = forSystems (system: mkPackages system);

    formatter = forSystems (system: let
      pkgs = rawPkgsFor system;
    in
      (treefmt-nix.lib.evalModule pkgs (import ./nix/treefmt.nix)).config.build.wrapper);

    devShells = forSystems (system: let
      pkgs = rawPkgsFor system;
      harbor = rs-harbor.lib;
      toolchain = harbor.mkToolchain {inherit pkgs;};
      treefmtEval = treefmt-nix.lib.evalModule pkgs (import ./nix/treefmt.nix);
      pre-commit-check = git-hooks.lib.${system}.run {
        src = ./.;
        hooks = import ./nix/pre-commit.nix {
          inherit pkgs;
          treefmtWrapper = treefmtEval.config.build.wrapper;
        };
      };
      plinthProject = plinth.packages.${system}.plinth-project;
      defaultShell = toolchain.craneLib.devShell {
        packages =
          [
            inputs.nix-pklx.packages.${system}.pklx
            pkgs.cargo-audit
            pkgs.cargo-deny
            pkgs.mdbook
            plinthProject
            self.packages.${system}.sc-nixgen
          ]
          ++ pre-commit-check.enabledPackages;
        shellHook = ''
          if ! git config --get core.hooksPath >/dev/null; then
            ${pre-commit-check.shellHook}
          fi
        '';
      };
    in {
      default = defaultShell;
      docs = pkgs.mkShell {packages = [pkgs.mdbook];};
    });

    nixosModules.default = import ./nix/module.nix;

    checks = forSystems mkChecks;
  };
}
