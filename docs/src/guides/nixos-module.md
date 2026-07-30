# NixOS Module

SmartCool provides a NixOS module at `nixosModules.default`.

The module defines `services.smartcool` with these options:

- `enable`: starts the SmartCool daemon.
- `package`: package used for the daemon binary.
- `settings`: attribute set serialized to `/etc/smartcool/config.pkl`.

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

When enabled, the service:

- Writes `/etc/smartcool/config.pkl`.
- Starts `sc daemon -c /etc/smartcool/config.pkl`.
- Uses `Type=notify`.
- Reports ready only after a successful control tick and services the systemd watchdog.
- Restarts on failure.
- Conflicts with Thinkfan and releases fan control across suspend/resume.
- Creates the runtime directory used by the IPC socket.
