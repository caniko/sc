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
          services.smartcool.enable = true;
          services.smartcool.package = package;
        })
        extraModule
      ];
    };

  basicSystem = eval {
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
    services.thinkfan.enable = true;
    services.smartcool.settings = basicSystem.config.services.smartcool.settings;
  };

  hasConflictAssertion =
    nixpkgs.lib.any (
      assertion:
        !assertion.assertion
        && assertion.message == "services.smartcool and services.thinkfan cannot be enabled together"
    )
    conflictingSystem.config.assertions;

  basicCfg = basicSystem.config;
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
}
