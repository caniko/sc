{
  nixpkgs,
  system,
  pkgs,
  smartcoolModule,
  package,
  pklx,
}: let
  eval = extraModule:
    nixpkgs.lib.nixosSystem {
      inherit system;
      modules = [
        smartcoolModule
        ({lib, ...}: {
          nixpkgs.hostPlatform = system;
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = lib.mkDefault "/dev/disk/by-label/nixos";
            fsType = "ext4";
          };
          system.stateVersion = "25.11";
          services.smartcool.package = package;
        })
        extraModule
      ];
    };

  basicSystem = eval {
    services.smartcool.enable = true;
    services.smartcool.settings = {
      poll_interval_ms = 2000;
      derivative = {
        window_size = 10;
        boost_threshold = 2.0;
        decay_rate = 0.5;
      };
      sensors = [
        {
          name = "cpu";
          hwmon = "k10temp";
          index = 1;
        }
      ];
      fans = [
        {
          name = "cpu-fan";
          hwmon = "nct6799";
          pwm_index = 2;
          topology = {
            position = "cpu_cooler";
            direction = "exhaust";
          };
          sensors = ["cpu"];
          curve = [
            {
              temp = 40;
              pwm = 100;
            }
            {
              temp = 90;
              pwm = 255;
            }
          ];
        }
      ];
    };
  };

  conflictingSystem = eval {
    services.smartcool.enable = true;
    services.thinkfan.enable = true;
    services.smartcool.settings = basicSystem.config.services.smartcool.settings;
  };

  curve = {
    fan = "CPU";
    temp = [30 40 50 60 70 80 90 100];
    pwm = [0 10 30 60 100 150 210 255];
  };

  asusdSystem = eval {
    services.asusd.enable = true;
    services.smartcool.asusd = {
      enable = true;
      profiles = {
        balanced = [curve];
        quiet = [
          curve
          (curve // {fan = "GPU";})
        ];
      };
    };
  };

  missingAsusdSystem = eval {
    services.smartcool.asusd = {
      enable = true;
      profiles.balanced = [curve];
    };
  };

  emptyProfilesSystem = eval {
    services.asusd.enable = true;
    services.smartcool.asusd.enable = true;
  };

  unknownProfileSystem = eval {
    services.asusd.enable = true;
    services.smartcool.asusd = {
      enable = true;
      profiles.turbo = [curve];
    };
  };

  invalidCurveSystem = eval {
    services.asusd.enable = true;
    services.smartcool.asusd = {
      enable = true;
      profiles.balanced = [
        curve
        (curve
          // {
            temp = [30 40 50 60 70 80 90 90];
          })
      ];
    };
  };

  decreasingCurveSystem = eval {
    services.asusd.enable = true;
    services.smartcool.asusd = {
      enable = true;
      profiles.balanced = [
        (curve
          // {
            pwm = [0 10 30 20 100 150 210 255];
          })
      ];
    };
  };

  hasConflictAssertion =
    nixpkgs.lib.any (
      assertion:
        !assertion.assertion
        && assertion.message == "services.smartcool and services.thinkfan cannot be enabled together"
    )
    conflictingSystem.config.assertions;

  hasFailedAssertion = systemConfig: message:
    nixpkgs.lib.any (
      assertion: !assertion.assertion && assertion.message == message
    )
    systemConfig.config.assertions;

  basicCfg = basicSystem.config;
  asusdCfg = asusdSystem.config;
in {
  module-daemon =
    pkgs.runCommand "smartcool-module-daemon" {
      nativeBuildInputs = [
        pklx
        pkgs.cacert
      ];
      SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    } ''
      test "${basicCfg.systemd.services.smartcool.serviceConfig.Type}" = notify
      test "${basicCfg.systemd.services.smartcool.serviceConfig.NotifyAccess}" = main
      test "${toString (builtins.elem "thinkfan.service" basicCfg.systemd.services.smartcool.conflicts)}" = 1
      test "${toString (builtins.elem "sleep.target" basicCfg.systemd.services.smartcool-sleep.wantedBy)}" = 1
      test "${toString (builtins.elem "sleep.target" basicCfg.systemd.services.smartcool-wakeup.wantedBy)}" = 1
      test "${toString hasConflictAssertion}" = 1
      case ${nixpkgs.lib.escapeShellArg basicCfg.systemd.services.smartcool.serviceConfig.ExecStart} in
        *"/bin/sc daemon -c /etc/smartcool/config.pkl"*) ;;
        *) exit 1 ;;
      esac
      pklx eval ${basicCfg.environment.etc."smartcool/config.pkl".source} > "$TMPDIR/config.nix"
      grep -F 'poll_interval_ms = 2000' "$TMPDIR/config.nix"
      ${package}/bin/sc config --validate ${basicCfg.environment.etc."smartcool/config.pkl".source}
      touch $out
    '';

  module-asusd =
    pkgs.runCommand "smartcool-module-asusd" {
      nativeBuildInputs = [
        pklx
        pkgs.cacert
      ];
      SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    } ''
      test "${toString (builtins.elem package asusdCfg.environment.systemPackages)}" = 1
      test "${toString (builtins.elem package basicCfg.environment.systemPackages)}" = 1
      test "${
        if asusdCfg.systemd.services ? smartcool
        then "1"
        else "0"
      }" = 0
      test "${asusdCfg.systemd.services.smartcool-asusd.serviceConfig.Type}" = oneshot
      test "${toString asusdCfg.systemd.services.smartcool-asusd.serviceConfig.RemainAfterExit}" = 1
      test "${toString (builtins.elem "asusd.service" asusdCfg.systemd.services.smartcool-asusd.after)}" = 1
      test "${toString (builtins.elem "asusd.service" asusdCfg.systemd.services.smartcool-asusd.requires)}" = 1
      test "${toString (builtins.elem "asusd.service" asusdCfg.systemd.services.smartcool-asusd.partOf)}" = 1
      test "${toString (builtins.elem asusdCfg.environment.etc."smartcool/asusd.pkl".source asusdCfg.systemd.services.smartcool-asusd.restartTriggers)}" = 1
      test ${nixpkgs.lib.escapeShellArg asusdCfg.systemd.services.smartcool-asusd.serviceConfig.ExecStart} = ${nixpkgs.lib.escapeShellArg "${package}/bin/sc asusd apply --config /etc/smartcool/asusd.pkl --apply"}
      pklx eval ${asusdCfg.environment.etc."smartcool/asusd.pkl".source} > "$TMPDIR/asusd.nix"
      grep -F 'name = "balanced"' "$TMPDIR/asusd.nix"
      grep -F 'fan = "GPU"' "$TMPDIR/asusd.nix"
      grep -F 'pwm = [' "$TMPDIR/asusd.nix"
      touch $out
    '';

  module-asusd-assertions = assert hasFailedAssertion missingAsusdSystem "services.smartcool.asusd requires services.asusd.enable";
  assert hasFailedAssertion emptyProfilesSystem "services.smartcool.asusd.profiles must not be empty";
  assert hasFailedAssertion unknownProfileSystem "services.smartcool.asusd.profiles contains an unknown profile name";
  assert hasFailedAssertion invalidCurveSystem "services.smartcool.asusd profiles require at least one unique fan and exactly 8 non-decreasing temp/PWM values";
  assert hasFailedAssertion decreasingCurveSystem "services.smartcool.asusd profiles require at least one unique fan and exactly 8 non-decreasing temp/PWM values";
    pkgs.runCommand "smartcool-module-asusd-assertions" {} ''
      touch $out
    '';
}
