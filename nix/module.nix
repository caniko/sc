{ronixLib}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.smartcool;
in {
  options.services.smartcool = {
    enable = lib.mkEnableOption "SmartCool fan control daemon";

    package = lib.mkPackageOption pkgs "smartcool" {};

    settings = lib.mkOption {
      type = lib.types.attrs;
      default = {};
      description = "SmartCool configuration serialized to RON.";
    };
  };

  config = lib.mkIf cfg.enable {
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
  };
}
