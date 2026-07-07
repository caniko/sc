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

  basicSystem = eval {
    services.smartcool.settings = {
      poll_interval_ms = 2000;
      sensors = [];
      fans = [];
    };
  };

  basicCfg = basicSystem.config;
in {
  module-daemon = pkgs.runCommand "smartcool-module-daemon" {} ''
    test "${basicCfg.systemd.services.smartcool.serviceConfig.Type}" = notify
    case ${nixpkgs.lib.escapeShellArg basicCfg.systemd.services.smartcool.serviceConfig.ExecStart} in
      *"/bin/sc daemon -c /etc/smartcool/config.ron"*) ;;
      *) exit 1 ;;
    esac
    grep -F 'poll_interval_ms: 2000' ${basicCfg.environment.etc."smartcool/config.ron".source}
    touch $out
  '';
}
