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

  nvidiaSystem = eval {
    hardware.nvidia.package = pkgs.hello; # stand-in: only its store path is checked
    services.smartcool.enable = true;
    services.smartcool.settings = {
      poll_interval_ms = 2000;
      derivative = {
        window_size = 10;
        boost_threshold = 2.0;
        decay_rate = 0.5;
      };
    };
    services.smartcool.nvidia = [
      {
        device = "0000:01:00.0";
        hysteresis = 2;
        fans = [
          {
            index = 0;
            curve = [
              {
                temp = 40;
                percent = 30;
              }
              {
                temp = 80;
                percent = 100;
              }
            ];
          }
          {
            index = 1;
            disabled = true;
          }
        ];
      }
    ];
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
  nvidiaCfg = nvidiaSystem.config;
  asusdCfg = asusdSystem.config;
  quietSystem = eval {
    services.asusd.enable = true;
    services.smartcool.asusd = {
      enable = true;
      profiles = nixpkgs.lib.genAttrs ["quiet" "balanced" "performance"] (_: [
        {
          fan = "CPU";
          temp = [40 50 55 60 65 75 85 90];
          pwm = [0 0 0 40 70 100 150 255];
        }
        {
          fan = "GPU";
          temp = [35 45 50 55 60 65 78 80];
          pwm = [0 0 0 0 45 90 150 255];
        }
      ]);
      quietControl = {
        enable = true;
        settings = {
          qualification_id = "module-fixture";
          hardware = {
            product = "fixture";
            board = "fixture";
            bios = "fixture";
            kernel = "fixture";
            nvidia_driver = "fixture";
            gpu_devices = ["fixture"];
          };
          gpu_pci_id = "0000:01:00.0";
          cpu_start_pwm = 40;
          gpu_start_pwm = 40;
          profiles = map (name: {
            inherit name;
            cpu_pwm_ceiling = 150;
            gpu_pwm_ceiling = 150;
            cpu_ppt = [15 25 35];
            gpu_clock_mhz = 1200;
          }) ["quiet" "balanced" "performance"];
        };
      };
    };
  };
  quietCfg = quietSystem.config;
in {
  module-asusd-quiet = pkgs.runCommand "smartcool-module-asusd-quiet" {} ''
    test "${quietCfg.systemd.services.smartcool-asusd-quiet.serviceConfig.Type}" = notify
    test "${quietCfg.systemd.services.smartcool-asusd-quiet.serviceConfig.WatchdogSec}" = 30s
    test "${toString (builtins.elem "smartcool-asusd.service" quietCfg.systemd.services.smartcool-asusd-quiet.partOf)}" = 1
    case ${nixpkgs.lib.escapeShellArg quietCfg.systemd.services.smartcool-asusd-quiet-sleep.serviceConfig.ExecStop} in
      *"/bin/systemctl try-restart smartcool-asusd-quiet.service") ;;
      *) exit 1 ;;
    esac
    ${package}/bin/sc acoustic validate-policy --config ${quietCfg.environment.etc."smartcool/quiet.pkl".source}
    touch "$out"
  '';
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

  module-nvidia =
    pkgs.runCommand "smartcool-module-nvidia" {
      nativeBuildInputs = [
        pklx
        pkgs.cacert
      ];
      SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    } ''
      test ${nixpkgs.lib.escapeShellArg nvidiaCfg.systemd.services.smartcool.environment.LD_LIBRARY_PATH} = ${nixpkgs.lib.escapeShellArg "${nvidiaCfg.hardware.nvidia.package}/lib"}
      test "${toString (builtins.elem "nvidia-persistenced.service" nvidiaCfg.systemd.services.smartcool.after)}" = 1
      test "${toString (builtins.elem "nvidia-persistenced.service" nvidiaCfg.systemd.services.smartcool.wants)}" = 1
      test -z "${toString (builtins.elem "nvidia-persistenced.service" (nvidiaCfg.systemd.services.smartcool.requires or []))}"
      ${package}/bin/sc config --validate ${nvidiaCfg.environment.etc."smartcool/config.pkl".source} | grep -F '1 nvidia devices'
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
