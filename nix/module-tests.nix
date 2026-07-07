{
  nixpkgs,
  system,
  pkgs,
  smartcoolModule,
  package,
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

  firmwareSystem = eval {
    services.smartcool.firmwareAttributes = {
      enable = true;
      basePath = "/sys/class/firmware-attributes/asus-armoury/attributes";
      attributes = {
        nv_temp_target = 75;
        ppt_pl1_spl = 15;
      };
      reassertInterval = "60s";
    };
  };

  nvidiaSystem = eval {
    services.smartcool.nvidiaClockCap = {
      enable = true;
      nvidiaSmi = "/run/current-system/sw/bin/nvidia-smi";
      minClockMHz = 210;
      maxClockMHz = 600;
      reassertInterval = "30s";
    };
  };

  fanCurveSystem = eval {
    services.smartcool.asusdFanCurves = {
      enable = true;
      profiles.quiet = [
        {
          fan = "CPU";
          pwm = [0 0 0 0 30 80 140 200];
          temp = [40 55 65 73 78 84 90 95];
        }
        {
          fan = "GPU";
          pwm = [0 0 0 0 30 80 140 200];
          temp = [40 55 65 73 78 84 90 95];
        }
      ];
    };
  };

  firmwareCfg = firmwareSystem.config;
  nvidiaCfg = nvidiaSystem.config;
  fanCurveCfg = fanCurveSystem.config;
  fanCurveText = fanCurveCfg.services.smartcool.asusdFanCurves.renderedText;
in {
  module-firmware-attributes = pkgs.runCommand "smartcool-module-firmware-attributes" {} ''
    test "${firmwareCfg.systemd.services.smartcool-firmware-attributes.serviceConfig.Type}" = oneshot
    test -z "${toString (firmwareCfg.systemd.services.smartcool-firmware-attributes.serviceConfig.RemainAfterExit or "")}"
    test "${firmwareCfg.systemd.timers.smartcool-firmware-attributes.timerConfig.Unit}" = smartcool-firmware-attributes.service
    test "${firmwareCfg.systemd.timers.smartcool-firmware-attributes.timerConfig.OnUnitActiveSec}" = 60s
    grep -F 'write_attr nv_temp_target 75' ${firmwareCfg.systemd.services.smartcool-firmware-attributes.serviceConfig.ExecStart}
    grep -F 'outside firmware range' ${firmwareCfg.systemd.services.smartcool-firmware-attributes.serviceConfig.ExecStart}
    touch $out
  '';

  module-nvidia-clock-cap = pkgs.runCommand "smartcool-module-nvidia-clock-cap" {} ''
    test "${nvidiaCfg.systemd.services.smartcool-nvidia-clock-cap.serviceConfig.Type}" = oneshot
    test "${toString nvidiaCfg.systemd.services.smartcool-nvidia-clock-cap.serviceConfig.RemainAfterExit}" = 1
    test "${nvidiaCfg.systemd.timers.smartcool-nvidia-clock-cap.timerConfig.Unit}" = smartcool-nvidia-clock-cap-reassert.service
    case ${nixpkgs.lib.escapeShellArg nvidiaCfg.systemd.services.smartcool-nvidia-clock-cap.serviceConfig.ExecStart} in
      *"-lgc 210,600"*) ;;
      *) exit 1 ;;
    esac
    case ${nixpkgs.lib.escapeShellArg nvidiaCfg.systemd.services.smartcool-nvidia-clock-cap.serviceConfig.ExecStop} in
      *"-rgc"*) ;;
      *) exit 1 ;;
    esac
    touch $out
  '';

  module-asusd-fan-curves = pkgs.writeText "smartcool-module-asusd-fan-curves" (
    assert builtins.match ".*profiles: \\(.*quiet: \\[.*fan: CPU.*pwm: \\(0, 0, 0, 0, 30, 80, 140, 200\\).*temp: \\(40, 55, 65, 73, 78, 84, 90, 95\\).*" fanCurveText != null;
    assert fanCurveCfg.environment.etc."asusd/fan_curves.ron".text == fanCurveText;
    "ok\n"
  );
}
