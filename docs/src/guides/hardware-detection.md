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

Each direct cooler is a sweep unit, and so is each case-fan `group`. Under
CPU and then GPU stress, each unit runs at full speed or at its floor (30 PWM
for coolers, 0 for case fans). With up to four units the benchmark runs every
combination twice, once in Gray-code order and once reversed, which cancels
linear heat soak. With more units it uses backward elimination instead: from
all fans at full speed it tries dropping one unit at a time, largest first,
measures each candidate twice around an all-max reference, and keeps the drop
when the reduced set still passes. That is 2 + 3k runs rather than 2·2^k,
and it can miss a cheaper set that is not reachable by dropping one unit at a
time. Either way it records the cheapest
fan set (fewest physical fans at full speed) that keeps every target sensor
within noise, or at least 1 °C, of the all-max temperature. It also checks
CPU package power (RAPL, needs root) or each GPU's board power stays within 3%, so
throttling is not mistaken for headroom. If only some of the heat source's
devices report power, or a meter misses readings, every fan is kept. `--generate-nix` then links each fan
only to the loads that need it. A fan that no load needs gets a reserve
curve, which holds its floor until the sensor passes its target temperature.

The detector classifies known CPU chips such as `k10temp`, `coretemp`, and `zenpower`; GPU chips such as `amdgpu`, `nvidia`, and `nouveau`; and common motherboard EC prefixes such as `nct6`, `it8`, `w83`, `f71`, and `asus`.

When multiple hwmon chips share the same name, generated configuration can include `hwmon_instance` to select a specific sysfs instance.
