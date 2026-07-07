# Configuration

SmartCool configuration is evaluated from Pkl into `sc_core::config::Config`.
The canonical schema is available at `pkl/SmartCoolConfig.pkl`, and user
configuration files can `amends` it for Pkl-side structure checks.

Top-level fields:

- `poll_interval_ms`: daemon poll interval in milliseconds. Must be greater than zero.
- `derivative`: derivative boost configuration.
- `sensors`: temperature sensor definitions. At least one is required.
- `fans`: PWM fan definitions. At least one is required.
- `tuning`: optional advanced tuning settings. Defaults are applied when omitted.

`derivative` fields:

- `window_size`: rolling sample window. Must be at least `2`.
- `boost_threshold`: temperature rise threshold in C/s that triggers proactive PWM boost.
- `decay_rate`: PWM units per tick used to relax derivative boost.

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

Supported fan positions are `front`, `rear`, `top`, `bottom`, `side`, `cpu_cooler`, and `gpu_cooler`.

Supported airflow directions are `intake` and `exhaust`.

Curve validation requires:

- Every fan curve has at least one point.
- Curve temperatures are strictly ascending.
- Every sensor referenced by a fan exists in `sensors`.

Tuning defaults:

- `ewma_span = 20`
- `ccf_buffer_size = 120`
- `ccf_max_lag = 30`
- `step_threshold = 15`
- `response_window = 60`
- `regression_min_samples = 60`
