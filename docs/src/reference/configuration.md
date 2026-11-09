# Configuration

SmartCool configuration is evaluated from Pkl into `sc_core::config::Config`.
The canonical schema is available at `pkl/SmartCoolConfig.pkl`, and user
configuration files can `amends` it for Pkl-side structure checks.

Top-level fields:

- `poll_interval_ms`: daemon poll interval in milliseconds. Must be greater than zero.
- `derivative`: derivative boost configuration.
- `sensors`: temperature sensor definitions. At least one is required unless only `nvidia` is configured.
- `fans`: PWM fan definitions. At least one is required unless only `nvidia` is configured.
- `nvidia`: optional list of NVIDIA GPUs driven through NVML (see below).
- `tuning`: optional advanced tuning settings. Defaults are applied when omitted.
- `hysteresis`: optional degrees C (default `0`). A falling sensor reading must drop more than this below its held value before the fan curves follow it down; rises apply at once. `sc-nixgen` writes `3` into generated configs.

`derivative` fields:

- `window_size`: rolling sample window. Must be at least `2`.
- `boost_threshold`: finite, non-negative temperature rise threshold in C/s that triggers proactive PWM boost.
- `decay_rate`: finite, non-negative PWM units per tick used to relax derivative boost.

Each sensor has:

- `name`: logical sensor name.
- `hwmon`: hwmon chip name.
- `index`: temperature input index, for example `1` for `temp1_input`.
- `hwmon_instance`: optional zero-based chip instance when names are duplicated.

Each fan has:

- `name`: logical fan name.
- `hwmon`: hwmon chip name.
- `pwm_index`: PWM channel index, for example `2` for `pwm2`.
- `hwmon_instance`: optional zero-based chip instance when names are duplicated.
- `topology`: physical position, airflow direction, and optional group.
- `sensors`: names of sensors that drive the fan.
- `curve`: temperature-to-PWM points sorted by ascending temperature.
- `sensor_curves`: optional list of `{ sensor, curve }` for linked sensors whose normal range differs, such as a GPU junction next to CPU Tctl. Sensors without one use `curve`. The fan runs at the highest PWM any linked sensor asks for.

Supported fan positions are `front`, `rear`, `top`, `bottom`, `side`, `cpu_cooler`, and `gpu_cooler`.

Supported airflow directions are `intake` and `exhaust`.

Curve validation requires:

- Every fan curve has at least two points.
- Curve temperatures are strictly ascending.
- Curve PWM values are non-decreasing.
- Every sensor referenced by a fan exists in `sensors`.
- Each `sensor_curves` entry names a sensor linked to that fan, at most once, and its curve follows the same rules.

If any sensor linked to a fan is unavailable or outside `0<temp<=150C`, the daemon commands that fan to PWM `255` until every linked sensor is valid again. Sensors on a runtime-suspended GPU are skipped instead; a fan whose linked sensors are all suspended idles at its curve floor.

Tuning defaults:

- `ewma_span = 20`
- `ccf_buffer_size = 120`
- `ccf_max_lag = 30`
- `step_threshold = 15`
- `response_window = 60`
- `regression_min_samples = 60`

All tuning sizes must be nonzero, `response_window` must be at least `5`, and `ccf_buffer_size` must be at least `ccf_max_lag + 10`.

## NVIDIA fans

Each `nvidia` entry controls the fans of one GPU through NVML (`libnvidia-ml.so.1`, provided by the NixOS module from `hardware.nvidia.package`):

- `device`: PCI bus id such as `"0000:01:00.0"`, or an NVML index such as `"0"`.
- `hysteresis`: optional degrees C (default `0`); a falling temperature must drop more than this below the held value before the curve follows it down.
- `fans`: list of `{ index, disabled = false, curve }`. `curve` is a list of `{ temp, percent }` points.

Validation requires strictly ascending temperatures, non-decreasing percents within `0..=100`, at least two points for every enabled fan, and unique fan indexes. At startup every `index` must be below the driver's `num_fans`.

Each tick the GPU temperature is read, each enabled fan gets its interpolated percent clamped to the driver's min/max range, and disabled fans get the minimum. If the minimum is above `0` the firmware forbids a full stop; a warning is logged once. If the temperature cannot be read, every enabled fan is commanded to the maximum. On shutdown, error, or panic the driver's automatic control is restored.

When `num_fans` is `1` the fans share one controller, so a `disabled` fan is refused at startup; unplug the fan header instead.

```nix
services.smartcool.nvidia = [
  {
    device = "0000:01:00.0";
    hysteresis = 2;
    fans = [
      { index = 0; curve = [ { temp = 40; percent = 30; } { temp = 80; percent = 100; } ]; }
      { index = 1; disabled = true; }
    ];
  }
];
```
