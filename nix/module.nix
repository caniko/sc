{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.smartcool;

  pklString = value:
    "\"${builtins.replaceStrings ["\\" "\"" "\n" "\r" "\t"] ["\\\\" "\\\"" "\\n" "\\r" "\\t"] value}\"";

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
    environment.etc."smartcool/config.pkl".text = pklModule cfg.settings + "\n";

    systemd.services.smartcool = {
      description = "SmartCool fan control daemon";
      wantedBy = ["multi-user.target"];
      after = ["local-fs.target"];
      serviceConfig = {
        Type = "notify";
        ExecStart = "${cfg.package}/bin/sc daemon -c /etc/smartcool/config.pkl";
        Restart = "on-failure";
        RuntimeDirectory = "smartcool";
      };
    };
  };
}
