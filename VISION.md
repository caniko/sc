# SmartCool vision

SmartCool is a Linux fan control daemon for systems that expose temperature
sensors and PWM fan controls through `/sys/class/hwmon`. The repository holds
the `sc` daemon and CLI, shared configuration and IPC types, hwmon access,
hardware detection, `sc-nixgen` for generating NixOS configuration, and `sc
asusd` for ASUS firmware fan curves over D-Bus. It is used by people who want
declarative, host-specific fan policy instead of a vendor utility.

## Problem

hwmon exposes raw sensors and PWM channels but no policy. A machine needs curves
that keep it cool without being audible, and those curves depend on which
sensor feeds which fan, which the kernel does not tell you. On ASUS laptops the
fan curve is owned by firmware and reachable only over D-Bus, and the stored
curve must be distinguished from observed fan behaviour. Both sides are
hardware-facing, so a wrong write can leave a machine without cooling.

## Principles

- Fail safe. A fan whose linked sensor is missing commands full PWM until valid
  readings return; the daemon sets configured fans to manual mode and only
  drives what it is told to drive.
- Observe before mutating. `sc-nixgen detect` emits a disabled starter module,
  benchmarking is read-only until `--apply`, and `sc asusd` separates read-only
  `status`, `verify`, and `monitor --expect-stopped` from the commands that
  write.
- Mutations are transactional. Applied identification and benchmark runs
  restore each fan's original PWM and control mode on exit, including
  interrupted or failed runs; asusd applies and resets firmware curves the same
  way.
- Evidence is caller-supplied. The firmware-table contract is pure validation
  and staging: it performs no sysfs, mailbox, or management-library I/O and
  grants no hardware capability. Temperature tables are validated for length,
  ordering, and range rather than padded with unknown points.
- Configuration is Pkl and is the single source of truth; the NixOS module and
  the CLI read the same format.
- Acoustic learning reports relative dBFS, explicitly not calibrated dB(A), and
  rejects an interrupted observation.

## Scope

- `sc` daemon and CLI: Pkl configuration, polling, temperature curves with
  derivative boosts, a Unix socket for status, analytics, and tuning.
- `sc-core`: shared configuration and IPC types, including firmware curve
  validation.
- `sc-hwmon`: sysfs sensor and fan access with multi-instance support.
- `sc-detect`: hwmon discovery.
- `sc-nixgen`: hardware detection, NixOS module generation, fan identification,
  and a stress-based benchmark workflow.
- `sc asusd`: discovery, preview, transactional apply and reset of ASUS firmware
  fan curves; acoustic observation and guarded learning.
- A NixOS module exposed as `nixosModules.default`, with an independent
  `services.smartcool.asusd` option.

## Non-goals

- It does not expose Intel GPU control. The README states this directly; Intel
  i915/Xe support is monitoring only, and it skips input reads when runtime PM
  is inactive or unknown.
- It does not claim calibrated loudness. Acoustic results are relative digital
  levels for a fixed capture setup.
- It does not pick safe thermal limits for a host; the firmware-table contract
  validates representation, not whether a board is safe.

## Direction

Recent work builds out asusd: verifying kernel curves, monitoring firmware fan
stop, complete firmware-table validation, and acoustic learning. The most
recent commit migrates configuration loading to the registry `pklx` crate. The
docs site tracks guides for NixOS, hardware detection, runtime commands, and
acoustic learning.

## Place in the fleet

Canix consumes SmartCool as a flake input (`sc`): the nomad host imports
`inputs.sc.nixosModules.default`, the NixOS module set applies
`inputs.sc.overlays.default`, and `flake/checks/home_profiles.nix` asserts the
nomad `smartcool-asusd` service wiring, config file mode, restart triggers, and
profiles.
