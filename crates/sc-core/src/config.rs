use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub poll_interval_ms: u64,
    pub derivative: DerivativeConfig,
    pub sensors: Vec<SensorConfig>,
    pub fans: Vec<FanConfig>,
    #[serde(default)]
    pub tuning: TuningConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningConfig {
    /// EWMA smoothing span (samples). Higher = smoother, more lag.
    #[serde(default = "default_ewma_span")]
    pub ewma_span: usize,
    /// Cross-correlation circular buffer size (samples).
    #[serde(default = "default_ccf_buffer_size")]
    pub ccf_buffer_size: usize,
    /// Maximum lag to search in CCF (samples).
    #[serde(default = "default_ccf_max_lag")]
    pub ccf_max_lag: usize,
    /// Minimum PWM change to trigger step response detection.
    #[serde(default = "default_step_threshold")]
    pub step_threshold: u8,
    /// Ticks to record after a step event for exponential fitting.
    #[serde(default = "default_response_window")]
    pub response_window: usize,
    /// Minimum samples before solving OLS regression.
    #[serde(default = "default_regression_min_samples")]
    pub regression_min_samples: usize,
}

impl Default for TuningConfig {
    fn default() -> Self {
        Self {
            ewma_span: default_ewma_span(),
            ccf_buffer_size: default_ccf_buffer_size(),
            ccf_max_lag: default_ccf_max_lag(),
            step_threshold: default_step_threshold(),
            response_window: default_response_window(),
            regression_min_samples: default_regression_min_samples(),
        }
    }
}

fn default_ewma_span() -> usize {
    20
}
fn default_ccf_buffer_size() -> usize {
    120
}
fn default_ccf_max_lag() -> usize {
    30
}
fn default_step_threshold() -> u8 {
    15
}
fn default_response_window() -> usize {
    60
}
fn default_regression_min_samples() -> usize {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivativeConfig {
    /// Number of samples in the rolling window
    pub window_size: usize,
    /// °C/s threshold to trigger proactive fan boost
    pub boost_threshold: f64,
    /// PWM units/s to relax when dT/dt < 0
    pub decay_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorConfig {
    pub name: String,
    /// hwmon chip name (e.g. "k10temp", "nct6799")
    pub hwmon: String,
    /// Temperature sensor index (e.g. 1 → temp1_input)
    pub index: u32,
    /// When multiple hwmon chips share the same name (e.g. two "amdgpu" for
    /// iGPU + dGPU), this 0-based instance index selects which one to use.
    /// Defaults to 0 (first match). Ordered by sysfs hwmon number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hwmon_instance: Option<u32>,
}

/// Physical location of a fan in the case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanPosition {
    Front,
    Rear,
    Top,
    Bottom,
    Side,
    CpuCooler,
    GpuCooler,
}

/// Direction of airflow relative to the case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AirflowDirection {
    /// Pulling cool air into the case
    Intake,
    /// Pushing hot air out of the case
    Exhaust,
}

/// Physical topology of a fan — where it sits and which way it blows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanTopology {
    pub position: FanPosition,
    pub direction: AirflowDirection,
    /// Optional group name — fans in the same group are benchmarked and
    /// controlled together (e.g. "front_intake", "top_exhaust").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanConfig {
    pub name: String,
    /// hwmon chip name for the fan controller
    pub hwmon: String,
    /// PWM channel index (e.g. 2 → pwm2)
    pub pwm_index: u32,
    /// When multiple hwmon chips share the same name, this 0-based instance
    /// index selects which one to use. Defaults to 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hwmon_instance: Option<u32>,
    /// Physical topology — position, airflow direction, group membership
    pub topology: FanTopology,
    /// Names of sensors that drive this fan
    pub sensors: Vec<String>,
    /// Temperature-to-PWM curve points (must be sorted by temp ascending)
    pub curve: Vec<CurvePoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurvePoint {
    /// Temperature threshold in °C
    pub temp: u32,
    /// PWM value (0-255) at this temperature
    pub pwm: u8,
}

impl FanConfig {
    /// Linearly interpolate PWM from the curve for a given temperature.
    pub fn interpolate_pwm(&self, temp_c: f64) -> u8 {
        if self.curve.is_empty() {
            return 0;
        }

        let temp = temp_c;

        // Below lowest point
        if temp <= self.curve[0].temp as f64 {
            return self.curve[0].pwm;
        }

        // Above highest point
        if temp >= self.curve.last().unwrap().temp as f64 {
            return self.curve.last().unwrap().pwm;
        }

        // Find the two surrounding points and interpolate
        for window in self.curve.windows(2) {
            let lo = &window[0];
            let hi = &window[1];
            let lo_temp = lo.temp as f64;
            let hi_temp = hi.temp as f64;

            if temp >= lo_temp && temp <= hi_temp {
                let ratio = (temp - lo_temp) / (hi_temp - lo_temp);
                let pwm = lo.pwm as f64 + ratio * (hi.pwm as f64 - lo.pwm as f64);
                return pwm.round() as u8;
            }
        }

        self.curve.last().unwrap().pwm
    }
}

pub fn load(path: &Path) -> Result<Config> {
    let config: Config = load_pkl(path)
        .with_context(|| format!("failed to evaluate Pkl config: {}", path.display()))?;

    validate(&config)?;
    Ok(config)
}

fn load_pkl<T>(path: &Path) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create Pkl evaluation runtime")?;

    runtime
        .block_on(pklx::eval_to_typed(
            path,
            pklx::pklr::EvalOptions::default(),
        ))
        .map_err(|e| anyhow::anyhow!("failed to evaluate Pkl {}: {}", path.display(), e))
}

fn validate(config: &Config) -> Result<()> {
    anyhow::ensure!(
        !config.sensors.is_empty(),
        "at least one sensor is required"
    );
    anyhow::ensure!(!config.fans.is_empty(), "at least one fan is required");
    anyhow::ensure!(config.poll_interval_ms > 0, "poll_interval_ms must be > 0");
    anyhow::ensure!(
        config.derivative.window_size >= 2,
        "derivative window_size must be >= 2"
    );
    anyhow::ensure!(
        config.derivative.boost_threshold.is_finite() && config.derivative.boost_threshold >= 0.0,
        "derivative boost_threshold must be finite and >= 0"
    );
    anyhow::ensure!(
        config.derivative.decay_rate.is_finite() && config.derivative.decay_rate >= 0.0,
        "derivative decay_rate must be finite and >= 0"
    );
    anyhow::ensure!(config.tuning.ewma_span > 0, "tuning ewma_span must be > 0");
    anyhow::ensure!(
        config.tuning.ccf_buffer_size > 0,
        "tuning ccf_buffer_size must be > 0"
    );
    let minimum_ccf_buffer = config
        .tuning
        .ccf_max_lag
        .checked_add(10)
        .ok_or_else(|| anyhow::anyhow!("tuning ccf_max_lag is too large"))?;
    anyhow::ensure!(
        config.tuning.ccf_buffer_size >= minimum_ccf_buffer,
        "tuning ccf_buffer_size must be at least ccf_max_lag + 10"
    );
    anyhow::ensure!(
        config.tuning.step_threshold > 0,
        "tuning step_threshold must be > 0"
    );
    anyhow::ensure!(
        config.tuning.response_window >= 5,
        "tuning response_window must be >= 5"
    );
    anyhow::ensure!(
        config.tuning.regression_min_samples > 0,
        "tuning regression_min_samples must be > 0"
    );

    let mut sensor_names = HashSet::new();
    for sensor in &config.sensors {
        anyhow::ensure!(
            !sensor.name.trim().is_empty(),
            "sensor name must not be empty"
        );
        anyhow::ensure!(
            !sensor.hwmon.trim().is_empty(),
            "sensor '{}' has empty hwmon",
            sensor.name
        );
        anyhow::ensure!(
            sensor.index > 0,
            "sensor '{}' index must be > 0",
            sensor.name
        );
        anyhow::ensure!(
            sensor_names.insert(sensor.name.as_str()),
            "duplicate sensor name '{}'",
            sensor.name
        );
    }

    let mut fan_names = HashSet::new();
    let mut fan_channels = HashSet::new();

    for fan in &config.fans {
        anyhow::ensure!(!fan.name.trim().is_empty(), "fan name must not be empty");
        anyhow::ensure!(
            !fan.hwmon.trim().is_empty(),
            "fan '{}' has empty hwmon",
            fan.name
        );
        anyhow::ensure!(
            fan.pwm_index > 0,
            "fan '{}' pwm_index must be > 0",
            fan.name
        );
        anyhow::ensure!(
            fan_names.insert(fan.name.as_str()),
            "duplicate fan name '{}'",
            fan.name
        );
        anyhow::ensure!(
            fan_channels.insert((&fan.hwmon, fan.hwmon_instance, fan.pwm_index)),
            "fan '{}' shares its hwmon channel with another fan",
            fan.name
        );
        anyhow::ensure!(
            !fan.sensors.is_empty(),
            "fan '{}' must reference at least one sensor",
            fan.name
        );
        anyhow::ensure!(
            fan.curve.len() >= 2,
            "fan '{}' curve must contain at least two points",
            fan.name
        );

        // Verify curve is sorted by temperature
        for window in fan.curve.windows(2) {
            anyhow::ensure!(
                window[0].temp < window[1].temp,
                "fan '{}' curve must be sorted by ascending temperature",
                fan.name
            );
            anyhow::ensure!(
                window[0].pwm <= window[1].pwm,
                "fan '{}' curve PWM must be non-decreasing",
                fan.name
            );
        }

        for point in &fan.curve {
            anyhow::ensure!(
                point.temp <= 150,
                "fan '{}' curve temperature {}°C is outside the supported 0..=150°C range",
                fan.name,
                point.temp
            );
        }

        // Verify all referenced sensors exist
        for sensor_ref in &fan.sensors {
            anyhow::ensure!(
                sensor_names.contains(sensor_ref.as_str()),
                "fan '{}' references unknown sensor '{}'",
                fan.name,
                sensor_ref
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_fan() -> FanConfig {
        FanConfig {
            name: "test".into(),
            hwmon: "nct6799".into(),
            pwm_index: 1,
            hwmon_instance: None,
            topology: FanTopology {
                position: FanPosition::Front,
                direction: AirflowDirection::Intake,
                group: None,
            },
            sensors: vec!["cpu".into()],
            curve: vec![
                CurvePoint { temp: 45, pwm: 20 },
                CurvePoint { temp: 55, pwm: 40 },
                CurvePoint { temp: 65, pwm: 80 },
                CurvePoint { temp: 75, pwm: 130 },
                CurvePoint { temp: 85, pwm: 200 },
            ],
        }
    }

    fn test_config() -> Config {
        Config {
            poll_interval_ms: 2000,
            derivative: DerivativeConfig {
                window_size: 10,
                boost_threshold: 2.0,
                decay_rate: 0.5,
            },
            sensors: vec![SensorConfig {
                name: "cpu".into(),
                hwmon: "k10temp".into(),
                index: 1,
                hwmon_instance: None,
            }],
            fans: vec![test_fan()],
            tuning: Default::default(),
        }
    }

    #[test]
    fn interpolate_below_curve() {
        let fan = test_fan();
        assert_eq!(fan.interpolate_pwm(30.0), 20);
    }

    #[test]
    fn interpolate_above_curve() {
        let fan = test_fan();
        assert_eq!(fan.interpolate_pwm(100.0), 200);
    }

    #[test]
    fn interpolate_exact_point() {
        let fan = test_fan();
        assert_eq!(fan.interpolate_pwm(55.0), 40);
    }

    #[test]
    fn interpolate_midpoint() {
        let fan = test_fan();
        // Midpoint between (45, 20) and (55, 40) → 30
        assert_eq!(fan.interpolate_pwm(50.0), 30);
    }

    #[test]
    fn validation_rejects_non_monotonic_pwm() {
        let mut config = test_config();
        config.fans[0].curve[1].pwm = 10;
        let error = validate(&config).unwrap_err().to_string();
        assert!(error.contains("PWM must be non-decreasing"));
    }

    #[test]
    fn validation_rejects_duplicate_hardware_channel() {
        let mut config = test_config();
        let mut duplicate = test_fan();
        duplicate.name = "second".into();
        config.fans.push(duplicate);
        let error = validate(&config).unwrap_err().to_string();
        assert!(error.contains("shares its hwmon channel"));
    }

    #[test]
    fn validation_rejects_unsafe_tuning_values() {
        let mut config = test_config();
        config.tuning.ccf_buffer_size = 0;
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("ccf_buffer_size"));

        let mut config = test_config();
        config.tuning.ccf_buffer_size = 20;
        config.tuning.ccf_max_lag = 11;
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("ccf_max_lag + 10"));

        let mut config = test_config();
        config.tuning.response_window = 0;
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("response_window"));
    }

    #[test]
    fn validation_rejects_non_finite_derivative_values() {
        let mut config = test_config();
        config.derivative.boost_threshold = f64::NAN;
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("boost_threshold"));

        let mut config = test_config();
        config.derivative.decay_rate = f64::INFINITY;
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("decay_rate"));
    }

    #[test]
    fn parse_pkl_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.pkl");
        std::fs::write(
            &path,
            r#"
poll_interval_ms = 2000
derivative = new {
  window_size = 10
  boost_threshold = 2.0
  decay_rate = 0.5
}
sensors = new Listing {
  new {
    name = "cpu"
    hwmon = "k10temp"
    index = 1
  }
}
fans = new Listing {
  new {
    name = "test"
    hwmon = "nct6799"
    pwm_index = 1
    topology = new {
      position = "cpu_cooler"
      direction = "exhaust"
    }
    sensors = new Listing { "cpu" }
    curve = new Listing {
      new { temp = 45; pwm = 20 }
      new { temp = 85; pwm = 200 }
    }
  }
}
"#,
        )
        .unwrap();

        let config = load(&path).unwrap();
        assert_eq!(config.sensors.len(), 1);
        assert_eq!(config.fans.len(), 1);
        assert_eq!(config.fans[0].curve.len(), 2);
    }

    #[test]
    fn invalid_pkl_error_mentions_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.pkl");
        std::fs::write(&path, "not valid pkl =").unwrap();

        let err = load(&path).unwrap_err().to_string();
        assert!(err.contains("failed to evaluate Pkl config"));
        assert!(err.contains("invalid.pkl"));
    }

    #[test]
    fn pkl_config_still_requires_sensors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-sensors.pkl");
        std::fs::write(
            &path,
            r#"
poll_interval_ms = 2000
derivative = new {
  window_size = 10
  boost_threshold = 2.0
  decay_rate = 0.5
}
sensors = new Listing {}
fans = new Listing {}
"#,
        )
        .unwrap();

        let err = load(&path).unwrap_err().to_string();
        assert!(err.contains("at least one sensor is required"));
    }
}
