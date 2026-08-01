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

## asusd Firmware Curves

`services.smartcool.asusd` independently manages firmware curves through `asusd`; it does not enable the SmartCool hwmon daemon. `services.asusd.enable` must already be enabled.

```nix
{
  services.asusd.enable = true;

  services.smartcool.asusd = {
    enable = true;
    profiles = {
      balanced = [
        {
          fan = "CPU";
          temp = [30 40 50 60 70 80 90 100];
          pwm = [0 10 30 60 100 150 210 255];
          enabled = true;
        }
        {
          fan = "GPU";
          temp = [30 40 50 60 70 80 90 100];
          pwm = [0 10 30 60 100 150 210 255];
        }
      ];
    };
  };
}
```

Profile names are `balanced`, `performance`, `quiet`, `low_power`, and `custom`; fan names are `CPU`, `GPU`, and `MID`. Every curve has exactly eight non-decreasing temperatures at or below 100°C and eight non-decreasing raw PWM values in `0..255`.

The module writes `/etc/smartcool/asusd.pkl` and starts `smartcool-asusd.service` after and as part of `asusd.service`. The oneshot runs `sc asusd apply --config /etc/smartcool/asusd.pkl --apply`, so it uses the same capability checks, exact read-back, and rollback behavior as the CLI. It does not write `fan_curves.ron` or `services.asusd.fanCurvesConfig`.

Firmware curves are stored and activated by `asusd` when platform profiles change. By contrast, `services.smartcool.enable` starts a live controller that continuously drives hwmon PWM channels from sensor readings. Do not enable the live daemon merely to use firmware curves.
