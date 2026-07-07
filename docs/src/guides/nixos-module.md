# NixOS Module

SmartCool provides a NixOS module at `nixosModules.default`.

The module defines `services.smartcool` with these options:

- `enable`: starts the SmartCool daemon.
- `package`: package used for the daemon binary.
- `settings`: attribute set serialized to `/etc/smartcool/config.ron` through `ronix`.

A minimal shape looks like this:

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
      sensors = [];
      fans = [];
    };
  };
}
```

The real configuration must include at least one sensor and one fan. The module test keeps an empty sensor and fan list only to verify service wiring and RON serialization.

When enabled, the service:

- Writes `/etc/smartcool/config.ron`.
- Starts `sc daemon -c /etc/smartcool/config.ron`.
- Uses `Type=notify`.
- Restarts on failure.
- Creates the runtime directory used by the IPC socket.
