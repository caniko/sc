# Runtime Commands

The `sc` binary exposes the daemon and query commands.

Run the daemon with the default configuration path:

```sh
sc daemon
```

Run the daemon with an explicit RON file:

```sh
sc daemon --config /path/to/config.ron
```

Validate a RON configuration:

```sh
sc config --validate /path/to/config.ron
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
