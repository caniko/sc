# Installation

SmartCool is packaged by this repository's Nix flake.

To build the daemon package from the repository root:

```sh
nix build .#smartcool
```

To build the NixOS configuration generator:

```sh
nix build .#sc-nixgen
```

For development, enter the flake shell:

```sh
nix develop
```

The NixOS module is exposed as `nixosModules.default`. A host flake can import it and set `services.smartcool.enable = true`.

The service writes its runtime configuration to `/etc/smartcool/config.pkl` and starts:

```sh
sc daemon -c /etc/smartcool/config.pkl
```

The daemon expects Linux hwmon devices under `/sys/class/hwmon`. Hardware discovery and live control commands need access to the relevant sysfs files.
