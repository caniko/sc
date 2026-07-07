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
      sensors = [];
      fans = [];
    };
  };

  basicCfg = basicSystem.config;
in {
  module-daemon = pkgs.runCommand "smartcool-module-daemon" {
    nativeBuildInputs = [
      pklx
      pkgs.cacert
    ];
    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
  } ''
    test "${basicCfg.systemd.services.smartcool.serviceConfig.Type}" = notify
    case ${nixpkgs.lib.escapeShellArg basicCfg.systemd.services.smartcool.serviceConfig.ExecStart} in
      *"/bin/sc daemon -c /etc/smartcool/config.pkl"*) ;;
      *) exit 1 ;;
    esac
    pklx eval ${basicCfg.environment.etc."smartcool/config.pkl".source} > "$TMPDIR/config.nix"
    grep -F 'poll_interval_ms = 2000' "$TMPDIR/config.nix"
    touch $out
  '';
}
