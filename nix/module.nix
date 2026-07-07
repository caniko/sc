{ronixLib}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.smartcool;
  inherit (lib) concatMapStringsSep concatStringsSep literalExpression mapAttrsToList mkEnableOption mkIf mkMerge mkOption optionalAttrs types;

  firmwareCfg = cfg.firmwareAttributes;
  nvidiaCfg = cfg.nvidiaClockCap;
  fanCurveCfg = cfg.asusdFanCurves;

  firmwareScript = pkgs.writeShellScript "smartcool-firmware-attributes" ''
    set -euo pipefail

    base=${lib.escapeShellArg firmwareCfg.basePath}

    read_value() {
      local path="$1"
      if [ -r "$path" ]; then
        cat "$path" 2>/dev/null || true
      fi
    }

    write_attr() {
      local attr="$1"
      local desired="$2"
      local attr_dir="$base/$attr"
      local target="$attr_dir/current_value"
      local min_value=""
      local max_value=""
      local current=""

      if [ ! -e "$target" ]; then
        echo "smartcool-firmware-attributes: $target missing" >&2
        exit 1
      fi

      if [ -e "$attr_dir/min_value" ] && [ -e "$attr_dir/max_value" ]; then
        min_value="$(read_value "$attr_dir/min_value")"
        max_value="$(read_value "$attr_dir/max_value")"
        echo "smartcool-firmware-attributes: $attr accepts $min_value..$max_value; desired $desired"
        if [ -n "$min_value" ] && [ -n "$max_value" ] \
          && { [ "$desired" -lt "$min_value" ] || [ "$desired" -gt "$max_value" ]; }; then
          echo "smartcool-firmware-attributes: $attr=$desired outside firmware range $min_value..$max_value" >&2
          exit 1
        fi
      fi

      current="$(read_value "$target")"
      if [ "$current" != "$desired" ]; then
        echo "smartcool-firmware-attributes: $attr $current -> $desired"
        if ! printf '%s\n' "$desired" > "$target"; then
          echo "smartcool-firmware-attributes: failed to set $attr=$desired" >&2
          exit 1
        fi
      fi
    }

    ${concatStringsSep "\n" (mapAttrsToList (name: value: "write_attr ${lib.escapeShellArg name} ${lib.escapeShellArg (toString value)}") firmwareCfg.attributes)}

    if [ -e "$base/pending_reboot/current_value" ]; then
      pending="$(read_value "$base/pending_reboot/current_value")"
      if [ "$pending" = "1" ]; then
        echo "smartcool-firmware-attributes: pending_reboot=1; one or more writes require reboot"
      fi
    fi
  '';

  nvidiaClockCapCommand = "${nvidiaCfg.nvidiaSmi} -lgc ${toString nvidiaCfg.minClockMHz},${toString nvidiaCfg.maxClockMHz}";

  ronTuple = values: "(${concatStringsSep ", " (map toString values)})";

  fanCurveBlock = curve: ''
    (
        fan: ${curve.fan},
        pwm: ${ronTuple curve.pwm},
        temp: ${ronTuple curve.temp},
        enabled: ${if curve.enabled then "true" else "false"},
    ),
  '';

  profileBlock = name: curves: ''
    ${name}: [
        ${concatMapStringsSep "\n" fanCurveBlock curves}
    ],
  '';

  renderedFanCurves = ''
    (
        profiles: (
            ${concatStringsSep "\n" (mapAttrsToList profileBlock fanCurveCfg.profiles)}
        ),
    )
  '';

  curvePointType = types.submodule {
    options = {
      fan = mkOption {
        type = types.enum ["CPU" "GPU" "MID" "CPU" "GPU"];
        description = "asusd fan identifier.";
      };

      pwm = mkOption {
        type = types.listOf types.int;
        description = "PWM points for this fan curve.";
        example = [0 0 0 0 30 80 140 200];
      };

      temp = mkOption {
        type = types.listOf types.int;
        description = "Temperature points in degrees Celsius.";
        example = [40 55 65 73 78 84 90 95];
      };

      enabled = mkOption {
        type = types.bool;
        default = true;
        description = "Whether this asusd fan curve entry is enabled.";
      };
    };
  };
in {
  options.services.smartcool = {
    enable = mkEnableOption "SmartCool fan control daemon";

    package = lib.mkPackageOption pkgs "smartcool" {};

    settings = mkOption {
      type = types.attrs;
      default = {};
      description = "SmartCool configuration serialized to RON.";
    };

    firmwareAttributes = {
      enable = mkEnableOption "declarative firmware-attribute writes";

      basePath = mkOption {
        type = types.path;
        default = "/sys/class/firmware-attributes/asus-armoury/attributes";
        description = "Directory containing firmware attribute subdirectories.";
      };

      attributes = mkOption {
        type = types.attrsOf types.int;
        default = {};
        description = "Firmware attribute current_value targets.";
        example = literalExpression ''
          {
            nv_temp_target = 75;
            ppt_pl1_spl = 15;
          }
        '';
      };

      reassertInterval = mkOption {
        type = types.str;
        default = "60s";
        description = "systemd OnUnitActiveSec interval for reasserting firmware attributes.";
      };

      onBootSec = mkOption {
        type = types.str;
        default = "30s";
        description = "systemd OnBootSec delay before the first reassertion.";
      };

      unitName = mkOption {
        type = types.str;
        default = "smartcool-firmware-attributes";
        description = "Base systemd unit name for firmware attribute application.";
      };
    };

    nvidiaClockCap = {
      enable = mkEnableOption "NVIDIA graphics clock cap";

      nvidiaSmi = mkOption {
        type = types.path;
        default = "${pkgs.linuxPackages.nvidia_x11.bin}/bin/nvidia-smi";
        defaultText = literalExpression ''"''${pkgs.linuxPackages.nvidia_x11.bin}/bin/nvidia-smi"'';
        description = "Path to nvidia-smi.";
      };

      minClockMHz = mkOption {
        type = types.ints.positive;
        description = "Minimum graphics clock lock in MHz.";
      };

      maxClockMHz = mkOption {
        type = types.ints.positive;
        description = "Maximum graphics clock lock in MHz.";
      };

      reassertInterval = mkOption {
        type = types.str;
        default = "30s";
        description = "systemd OnUnitActiveSec interval for reasserting the clock cap.";
      };

      onBootSec = mkOption {
        type = types.str;
        default = "1min";
        description = "systemd OnBootSec delay before the first clock-cap reassertion.";
      };

      unitName = mkOption {
        type = types.str;
        default = "smartcool-nvidia-clock-cap";
        description = "Base systemd unit name for the NVIDIA clock-cap lifecycle service and timer.";
      };
    };

    asusdFanCurves = {
      enable = mkEnableOption "asusd fan curve rendering";

      profiles = mkOption {
        type = types.attrsOf (types.listOf curvePointType);
        default = {};
        description = "asusd fan curves keyed by profile name.";
        example = literalExpression ''
          {
            quiet = [
              {
                fan = "CPU";
                pwm = [0 0 0 0 30 80 140 200];
                temp = [40 55 65 73 78 84 90 95];
              }
            ];
          }
        '';
      };

      renderedText = mkOption {
        type = types.lines;
        readOnly = true;
        internal = true;
        description = "Rendered asusd fan_curves.ron text.";
      };
    };
  };

  config = mkMerge [
    (mkIf cfg.enable {
      environment.etc."smartcool/config.ron".text = ronixLib.toRON 0 cfg.settings;

      systemd.services.smartcool = {
        description = "SmartCool fan control daemon";
        wantedBy = ["multi-user.target"];
        after = ["local-fs.target"];
        serviceConfig = {
          Type = "notify";
          ExecStart = "${cfg.package}/bin/sc daemon -c /etc/smartcool/config.ron";
          Restart = "on-failure";
          RuntimeDirectory = "smartcool";
        };
      };
    })

    (mkIf firmwareCfg.enable {
      assertions = [
        {
          assertion = firmwareCfg.attributes != {};
          message = "services.smartcool.firmwareAttributes.attributes must not be empty when firmwareAttributes.enable is true.";
        }
      ];

      systemd.services.${firmwareCfg.unitName} = {
        description = "Apply SmartCool firmware attributes";
        wantedBy = ["multi-user.target"];
        after = ["systemd-modules-load.service"];
        serviceConfig = {
          Type = "oneshot";
          ExecStart = firmwareScript;
        };
      };

      systemd.timers.${firmwareCfg.unitName} = {
        description = "Periodically reassert SmartCool firmware attributes";
        wantedBy = ["timers.target"];
        timerConfig = {
          OnBootSec = firmwareCfg.onBootSec;
          OnUnitActiveSec = firmwareCfg.reassertInterval;
          AccuracySec = "5s";
          Unit = "${firmwareCfg.unitName}.service";
        };
      };
    })

    (mkIf nvidiaCfg.enable {
      assertions = [
        {
          assertion = nvidiaCfg.minClockMHz <= nvidiaCfg.maxClockMHz;
          message = "services.smartcool.nvidiaClockCap.minClockMHz must be <= maxClockMHz.";
        }
      ];

      systemd.services.${nvidiaCfg.unitName} = {
        description = "Apply SmartCool NVIDIA graphics clock cap";
        wantedBy = ["multi-user.target"];
        after = ["nvidia-persistenced.service"];
        requires = ["nvidia-persistenced.service"];
        serviceConfig = {
          Type = "oneshot";
          ExecStartPre = "${nvidiaCfg.nvidiaSmi} -pm 1";
          ExecStart = nvidiaClockCapCommand;
          ExecStop = "${nvidiaCfg.nvidiaSmi} -rgc || true";
          RemainAfterExit = true;
        };
      };

      systemd.services.${nvidiaCfg.unitName + "-reassert"} = {
        description = "Reassert SmartCool NVIDIA graphics clock cap";
        after = ["nvidia-persistenced.service"];
        requires = ["nvidia-persistenced.service"];
        serviceConfig = {
          Type = "oneshot";
          ExecStart = nvidiaClockCapCommand;
        };
      };

      systemd.timers.${nvidiaCfg.unitName} = {
        description = "Periodically reassert SmartCool NVIDIA graphics clock cap";
        wantedBy = ["timers.target"];
        timerConfig = {
          OnBootSec = nvidiaCfg.onBootSec;
          OnUnitActiveSec = nvidiaCfg.reassertInterval;
          AccuracySec = "1s";
          Unit = "${nvidiaCfg.unitName}-reassert.service";
        };
      };
    })

    (mkIf fanCurveCfg.enable {
      assertions = [
        {
          assertion = fanCurveCfg.profiles != {};
          message = "services.smartcool.asusdFanCurves.profiles must not be empty when asusdFanCurves.enable is true.";
        }
      ];

      services.smartcool.asusdFanCurves.renderedText = renderedFanCurves;
      environment.etc."asusd/fan_curves.ron".text = renderedFanCurves;
    })
  ];
}
