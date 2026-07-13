# Hardware Detection

`sc-nixgen detect` scans `/sys/class/hwmon` for temperature sensors and controllable PWM channels.

By default, sensor discovery focuses on CPU and GPU chips. Fan PWM channels are scanned from all discovered chips because motherboard embedded controllers commonly host PWM controls.

Run detection:

```sh
nix run .#sc-nixgen -- detect --output smartcool.nix
```

Detection is an inventory operation. The generated module is deliberately
disabled (`services.smartcool.enable = false`) until the hardware mapping and
curves have been reviewed. It must not be activated merely because a PWM
channel was found.

Inspect one channel without writing sysfs:

```sh
nix run .#sc-nixgen -- identify --fan fan1
```

To perform the short, reversible test, opt in explicitly:

```sh
nix run .#sc-nixgen -- identify --fan fan1 --apply --duration 5
```

The command records the original PWM and control mode and restores both on
normal exit, errors, panics, or SIGINT/SIGTERM. Do not use `--apply` while a
different fan-control daemon is managing the same channel.

Include motherboard embedded-controller sensors:

```sh
nix run .#sc-nixgen -- detect --include-ec --output smartcool.nix
```

Generate a topology template for benchmarking:

```sh
nix run .#sc-nixgen -- detect \
  --output smartcool.nix \
  --topology-template topology.pkl
```

Edit the topology template to match the case layout, then run:

```sh
nix run .#sc-nixgen -- benchmark --topology topology.pkl
```

The benchmark command is also read-only unless `--apply` is supplied. Review
the topology and confirm a recovery path first:

```sh
nix run .#sc-nixgen -- benchmark --topology topology.pkl --apply
```

Benchmark writes are guarded by the same transaction-safe restoration path and
the generated Nix module is only produced after an applied benchmark has
collected tuning data.

The detector classifies known CPU chips such as `k10temp`, `coretemp`, and `zenpower`; GPU chips such as `amdgpu`, `nvidia`, and `nouveau`; and common motherboard EC prefixes such as `nct6`, `it8`, `w83`, `f71`, and `asus`.

When multiple hwmon chips share the same name, generated configuration can include `hwmon_instance` to select a specific sysfs instance.
