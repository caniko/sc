# SmartCool

<!-- simit:badges:start -->

![CI](https://img.shields.io/badge/CI-managed-2088ff) [![docs](https://img.shields.io/badge/docs-enabled-6f42c1)](docs)

<!-- simit:badges:end -->

SmartCool is a Linux fan control daemon for systems that expose temperature sensors and PWM fan controls through `/sys/class/hwmon`. NVIDIA GPU fans are driven per fan through NVML (`nvidia` config, `sc nvidia`), including holding a faulty fan at the firmware minimum.

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
      derivative = {
        window_size = 10;
        boost_threshold = 2.0;
        decay_rate = 0.5;
      };
      sensors = [{name = "cpu"; hwmon = "k10temp"; index = 1;}];
      fans = [{
        name = "cpu-fan";
        hwmon = "nct6799";
        pwm_index = 2;
        topology = {position = "cpu_cooler"; direction = "exhaust";};
        sensors = ["cpu"];
        curve = [{temp = 40; pwm = 100;} {temp = 90; pwm = 255;}];
      }];
    };
  };
}
```

Real deployments must provide host-specific sensors, fans, and conservative fan curves before enabling control. A missing linked sensor commands full PWM until valid readings return.

`sc-nixgen detect` and `sc-nixgen benchmark` are review-first commands:
detection emits a disabled starter module, and benchmarking is read-only until
`--apply` is explicitly supplied. `sc-nixgen identify --fan <name>` provides a
single-channel inspection/test flow. Applied identification and benchmark
transactions restore each fan's original PWM and control mode on exit,
including interrupted or failed runs.

`sc asusd` separately discovers, previews, transactionally applies, and resets
ASUS firmware fan curves over D-Bus. Read-only `status`, `verify`, and bounded
`monitor --expect-stopped` commands distinguish stored/driver curve state from
observed fan RPM, including firmware fans without writable PWM controls. The independent
`services.smartcool.asusd` NixOS option configures those curves without enabling
SmartCool's live hwmon daemon.

The shared [firmware-table contract](docs/src/reference/firmware-tables.md)
provides offline full-table validation and transaction-state tests for future
backends. It does not expose Intel GPU control. Intel i915/Xe monitoring skips
input reads when runtime PM is inactive or unknown and preserves zero RPM as a
reading without interpreting which physical fan it represents.
