{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.smartcool;

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
in {
  options.services.smartcool = {
    enable = lib.mkEnableOption "SmartCool fan control daemon";

    package = lib.mkPackageOption pkgs "smartcool" {};

    settings = lib.mkOption {
      type = lib.types.attrs;
      default = {};
      description = "SmartCool configuration serialized to Pkl.";
    };
  };

  config = lib.mkIf cfg.enable {
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
  };
}
