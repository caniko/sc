# Runtime Commands

The `sc` binary exposes the daemon and query commands.

Run the daemon with the default configuration path:

```sh
sc daemon
```

Run the daemon with an explicit Pkl file:

```sh
sc daemon --config /path/to/config.pkl
```

Validate a Pkl configuration:

```sh
sc config --validate /path/to/config.pkl
```

Query current sensor and fan state:

```sh
sc status
```

Show cooling effectiveness analytics:

```sh
sc analytics
```

Show advanced thermal tuning data:

```sh
sc tuning
```

The query commands connect to `/run/smartcool/sc.sock`. If the daemon is not running or the socket is unavailable, the IPC client reports that it failed to connect to the SmartCool daemon.

## asusd Firmware Curves

`sc asusd` talks directly to the running `asusd` service over the system D-Bus. It discovers platform profiles and CPU, GPU, or MID fan support at runtime; it does not use model allowlists or invoke `asusctl`.

Show supported profiles, stored curves, kernel-exposed curves, and observed RPM/temperatures:

```sh
sc asusd status
sc asusd status --json
```

Preview a typed Pkl configuration without changing firmware state:

```sh
sc asusd apply --config /path/to/asusd.pkl
```

Apply it explicitly:

```sh
sc asusd apply --config /path/to/asusd.pkl --apply
```

Before writing, SmartCool verifies every requested profile and fan and snapshots every curve it will touch. Each write is read back exactly from asusd's stored configuration. A write or verification failure restores attempted curves in reverse order. Raw PWM values remain in the firmware's `0..255` scale, including zero-PWM fan-stop regions.

Verify all declared profiles against stored configuration and the **active** profile against kernel-exposed curve points and enable state:

```sh
sc asusd verify --config /etc/smartcool/asusd.pkl --json
```

Verification does not switch profiles. Unsupported, mismatched, ambiguous, or unreadable active curves fail verification. Inactive profiles are checked only in asusd storage. Kernel curve attributes reflect driver state, not an independent EC read-back; matching points do not prove that the physical fans stopped.

Measure a settled 15-minute fan-stop window:

```sh
sc asusd monitor --duration-seconds 900 --interval-ms 5000 --expect-stopped --json
```

Monitoring is read-only and needs no SmartCool daemon or writable PWM controls. It discovers hwmon chips and labels anew for each sample, including ASUS RPM-only fans. Missing/unreadable RPM is unknown, never zero. It does not poll GPU inputs unless runtime PM reports the device active. A profile change invalidates subsequent samples; rerun after the profile settles.

JSON monitoring emits one `type: "sample"` object per sample, followed by a `type: "summary"` object with sample counts and maximum RPM per expected fan. `--expect-stopped` exits unsuccessfully unless **every** sample has a readable zero RPM for every ASUS fan discovered through asusd. Duration is bounded to 1–86400 seconds and intervals to 250–60000 milliseconds. This proves sampled fan stop, not continuous silence between samples or under untested workloads.

Reset one profile to its firmware defaults, first as a preview and then explicitly:

```sh
sc asusd reset --profile quiet
sc asusd reset --profile quiet --apply
```

Reset also snapshots the profile and restores it if reset or read-back verification fails. The active platform profile is checked and preserved across both operations.

These are persistent `asusd` firmware curves. They are separate from `sc daemon`, which continuously reads hwmon temperatures and writes live hwmon PWM values using SmartCool's derivative tracking and analytics.
