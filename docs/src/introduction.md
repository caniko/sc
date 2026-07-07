# Introduction

SmartCool is an intelligent fan control daemon for Linux systems that expose temperature sensors and PWM fan controls through `/sys/class/hwmon`.

The repository contains:

- `smartcool`, which builds the `sc` daemon and CLI.
- `sc-core`, shared configuration and IPC types.
- `sc-hwmon`, sysfs access for sensors and fans.
- `sc-detect`, hwmon discovery.
- `sc-nixgen`, NixOS configuration generation from detected hardware and optional tuning data.
- A NixOS module exposed as `nixosModules.default`.

The daemon reads a Pkl configuration, sets configured fans to manual PWM mode, polls configured sensors, applies temperature curve control with derivative-based boosts, and exposes status, analytics, and tuning data through a Unix socket at `/run/smartcool/sc.sock`.

SmartCool is hardware-facing software. Validate generated configuration before enabling it, and make sure every controlled fan has a conservative curve for the machine it protects.
