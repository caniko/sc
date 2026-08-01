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

Show the supported profiles and their stored firmware curves:

```sh
sc asusd status
```

Preview a typed Pkl configuration without changing firmware state:

```sh
sc asusd apply --config /path/to/asusd.pkl
```

Apply it explicitly:

```sh
sc asusd apply --config /path/to/asusd.pkl --apply
```

Before writing, SmartCool verifies every requested profile and fan and snapshots every curve it will touch. Each write is read back exactly. A write or verification failure restores attempted curves in reverse order. Raw PWM values remain in the firmware's `0..255` scale.

Reset one profile to its firmware defaults, first as a preview and then explicitly:

```sh
sc asusd reset --profile quiet
sc asusd reset --profile quiet --apply
```

Reset also snapshots the profile and restores it if reset or read-back verification fails. The active platform profile is checked and preserved across both operations.

These are persistent `asusd` firmware curves. They are separate from `sc daemon`, which continuously reads hwmon temperatures and writes live hwmon PWM values using SmartCool's derivative tracking and analytics.
