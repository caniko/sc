{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.smartcool;
  asusdCfg = cfg.asusd;

  pklString = value: "\"${builtins.replaceStrings ["\\" "\"" "\n" "\r" "\t"] ["\\\\" "\\\"" "\\n" "\\r" "\\t"] value}\"";

  pklKey = key:
    if builtins.match "[A-Za-z_][A-Za-z0-9_]*" key != null
    then key
    else throw "services.smartcool.settings contains unsupported Pkl key ${key}";

  pklValue = value:
    if value == null
    then "null"
    else if builtins.isBool value
    then
      if value
      then "true"
      else "false"
    else if builtins.isInt value
    then toString value
    else if builtins.isFloat value
    then let
      text = toString value;
    in
      if builtins.match ".*[.eE].*" text != null
      then text
      else "${text}.0"
    else if builtins.isString value
    then pklString value
    else if builtins.isList value
    then
      if value == []
      then "new Listing {}"
      else ''
        new Listing {
        ${lib.concatMapStringsSep "\n" (item: "  ${pklValue item}") value}
        }
      ''
    else if builtins.isAttrs value
    then
      if value == {}
      then "new {}"
      else ''
        new {
        ${lib.concatStringsSep "\n" (lib.mapAttrsToList (name: item: "  ${pklKey name} = ${pklValue item}") value)}
        }
      ''
    else throw "services.smartcool.settings contains unsupported value type ${builtins.typeOf value}";

  pklModule = value:
    if builtins.isAttrs value
    then lib.concatStringsSep "\n" (lib.mapAttrsToList (name: item: "${pklKey name} = ${pklValue item}") value)
    else throw "services.smartcool.settings must be an attribute set";

  asusdCurveType = lib.types.submodule {
    options = {
      fan = lib.mkOption {
        type = lib.types.enum ["CPU" "GPU" "MID"];
        description = "asusd firmware fan identifier.";
      };
      temp = lib.mkOption {
        type = lib.types.listOf (lib.types.ints.between 0 100);
        description = "Eight non-decreasing firmware curve temperatures in degrees Celsius.";
      };
      pwm = lib.mkOption {
        type = lib.types.listOf (lib.types.ints.between 0 255);
        description = "Eight non-decreasing raw firmware PWM values in the 0..255 range.";
      };
      enabled = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Whether asusd should activate this firmware curve.";
      };
    };
  };

  nonDecreasing = values:
    lib.all lib.id (lib.zipListsWith (left: right: left <= right) values (lib.drop 1 values));
  profileValid = curves: let
    fans = map (curve: curve.fan) curves;
  in
    curves
    != []
    && builtins.length fans
    == builtins.length (lib.unique fans)
    && lib.all (
      curve:
        builtins.length curve.temp
        == 8
        && builtins.length curve.pwm == 8
        && nonDecreasing curve.temp
        && nonDecreasing curve.pwm
    )
    curves;
  knownProfiles = ["balanced" "performance" "quiet" "low_power" "custom"];
  asusdCurve = curve: ''
    new FanCurve {
      fan = ${pklString curve.fan}
      temp = ${pklValue curve.temp}
      pwm = ${pklValue curve.pwm}
      enabled = ${pklValue curve.enabled}
    }
  '';
  asusdProfile = name: curves: ''
    new Profile {
      name = ${pklString name}
      curves = new Listing {
        ${lib.concatMapStringsSep "\n" asusdCurve curves}
      }
    }
  '';
  asusdPkl = ''
    amends "${../pkl/AsusdConfig.pkl}"

    profiles = new Listing {
      ${lib.concatStringsSep "\n" (lib.mapAttrsToList asusdProfile asusdCfg.profiles)}
    }
  '';
in {
  options.services.smartcool = {
    enable = lib.mkEnableOption "SmartCool fan control daemon";

    package = lib.mkPackageOption pkgs "smartcool" {};

    settings = lib.mkOption {
      type = lib.types.attrs;
      default = {};
      description = "SmartCool configuration serialized to Pkl.";
    };

    asusd = {
      enable = lib.mkEnableOption "declarative asusd firmware fan curves";

      profiles = lib.mkOption {
        type = lib.types.attrsOf (lib.types.listOf asusdCurveType);
        default = {};
        description = "asusd firmware fan curves keyed by platform profile.";
        example = lib.literalExpression ''
          {
            balanced = [
              {
                fan = "CPU";
                temp = [30 40 50 60 70 80 90 100];
                pwm = [0 10 30 60 100 150 210 255];
              }
            ];
          }
        '';
      };
    };
  };

  config = lib.mkMerge [
    (lib.mkIf (cfg.enable || asusdCfg.enable) {
      environment.systemPackages = [cfg.package];
    })

    (lib.mkIf cfg.enable {
      assertions = [
        {
          assertion = !config.services.thinkfan.enable;
          message = "services.smartcool and services.thinkfan cannot be enabled together";
        }
      ];

      environment.etc."smartcool/config.pkl".text = pklModule cfg.settings + "\n";

      systemd.services = {
        smartcool = {
          description = "SmartCool fan control daemon";
          wantedBy = ["multi-user.target"];
          after = ["local-fs.target"];
          conflicts = ["thinkfan.service"];
          serviceConfig = {
            Type = "notify";
            NotifyAccess = "main";
            ExecStart = "${cfg.package}/bin/sc daemon -c /etc/smartcool/config.pkl";
            Restart = "on-failure";
            RestartSec = "1s";
            RuntimeDirectory = "smartcool";
            PrivateNetwork = true;
            WatchdogSec = "${toString (lib.max 10000 ((cfg.settings.poll_interval_ms or 2000) * 3))}ms";
          };
        };

        smartcool-sleep = {
          description = "Release SmartCool fan control before sleep";
          wantedBy = ["sleep.target"];
          before = ["sleep.target"];
          serviceConfig.Type = "oneshot";
          script = "${config.systemd.package}/bin/systemctl stop smartcool.service";
        };

        smartcool-wakeup = {
          description = "Restart SmartCool fan control after waking";
          wantedBy = ["sleep.target"];
          after = [
            "suspend.target"
            "suspend-then-hibernate.target"
            "hybrid-sleep.target"
            "hibernate.target"
          ];
          serviceConfig.Type = "oneshot";
          script = "${config.systemd.package}/bin/systemctl start smartcool.service";
        };
      };
    })

    (lib.mkIf asusdCfg.enable {
      assertions = [
        {
          assertion = config.services.asusd.enable;
          message = "services.smartcool.asusd requires services.asusd.enable";
        }
        {
          assertion = asusdCfg.profiles != {};
          message = "services.smartcool.asusd.profiles must not be empty";
        }
        {
          assertion = lib.all (name: builtins.elem name knownProfiles) (builtins.attrNames asusdCfg.profiles);
          message = "services.smartcool.asusd.profiles contains an unknown profile name";
        }
        {
          assertion = lib.all profileValid (builtins.attrValues asusdCfg.profiles);
          message = "services.smartcool.asusd profiles require at least one unique fan and exactly 8 non-decreasing temp/PWM values";
        }
      ];

      environment.etc."smartcool/asusd.pkl".text = asusdPkl;

      systemd.services.smartcool-asusd = {
        description = "Apply SmartCool asusd firmware fan curves";
        wantedBy = ["multi-user.target"];
        after = ["asusd.service"];
        requires = ["asusd.service"];
        partOf = ["asusd.service"];
        restartTriggers = [config.environment.etc."smartcool/asusd.pkl".source];
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = "${cfg.package}/bin/sc asusd apply --config /etc/smartcool/asusd.pkl --apply";
        };
      };
    })
  ];
}
