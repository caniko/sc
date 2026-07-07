# SmartCool

<!-- simit:badges:start -->
[![Nix](https://img.shields.io/badge/Nix-managed-5277c3)](flake.nix) [![docs](https://img.shields.io/badge/docs-enabled-6f42c1)](docs) [![crates.io](https://img.shields.io/badge/crates.io-ready-f46623)](https://crates.io/crates/sc-core)
<!-- simit:badges:end -->

SmartCool is a Linux fan control daemon for systems that expose temperature sensors and PWM fan controls through `/sys/class/hwmon`.

The repository contains the `sc` daemon and CLI, shared configuration and IPC types, hwmon access helpers, hardware detection, and `sc-nixgen` for generating NixOS configuration from detected hardware and optional tuning data.

## Development

Build the daemon:

```sh
nix build .#smartcool
```

Run the full flake checks:

```sh
nix flake check
```

Enter the development shell:

```sh
nix develop
```

## NixOS

The flake exposes `nixosModules.default`. A host can import it and enable:

```nix
{
  services.smartcool = {
    enable = true;
    settings = {
      poll_interval_ms = 2000;
      sensors = [];
      fans = [];
    };
  };
}
```

Real deployments must provide host-specific sensors, fans, and conservative fan curves before enabling control.
