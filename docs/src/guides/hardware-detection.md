# Hardware Detection

`sc-nixgen detect` scans `/sys/class/hwmon` for temperature sensors and controllable PWM channels.

By default, sensor discovery focuses on CPU and GPU chips. Fan PWM channels are scanned from all discovered chips because motherboard embedded controllers commonly host PWM controls.

Run detection:

```sh
nix run .#sc-nixgen -- detect --output smartcool.nix
```

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

The detector classifies known CPU chips such as `k10temp`, `coretemp`, and `zenpower`; GPU chips such as `amdgpu`, `nvidia`, and `nouveau`; and common motherboard EC prefixes such as `nct6`, `it8`, `w83`, `f71`, and `asus`.

When multiple hwmon chips share the same name, generated configuration can include `hwmon_instance` to select a specific sysfs instance.
