use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub poll_interval_ms: u64,
    pub derivative: DerivativeConfig,
    pub sensors: Vec<SensorConfig>,
    pub fans: Vec<FanConfig>,
    #[serde(default)]
    pub tuning: TuningConfig,
}

#[derive(Debug, Serialize, Deserialize)]
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

fn default_ewma_span() -> usize { 20 }
fn default_ccf_buffer_size() -> usize { 120 }
fn default_ccf_max_lag() -> usize { 30 }
fn default_step_threshold() -> u8 { 15 }
fn default_response_window() -> usize { 60 }
fn default_regression_min_samples() -> usize { 60 }

#[derive(Debug, Serialize, Deserialize)]
pub struct DerivativeConfig {
    /// Number of samples in the rolling window
    pub window_size: usize,
    /// °C/s threshold to trigger proactive fan boost
    pub boost_threshold: f64,
    /// PWM units/s to relax when dT/dt < 0
    pub decay_rate: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SensorConfig {
    pub name: String,
    /// hwmon chip name (e.g. "k10temp", "nct6799")
    pub hwmon: String,
    /// Temperature sensor index (e.g. 1 → temp1_input)
    pub index: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FanConfig {
    pub name: String,
    /// hwmon chip name for the fan controller
    pub hwmon: String,
    /// PWM channel index (e.g. 2 → pwm2)
    pub pwm_index: u32,
    /// Names of sensors that drive this fan
    pub sensors: Vec<String>,
    /// Temperature-to-PWM curve points (must be sorted by temp ascending)
    pub curve: Vec<CurvePoint>,
}

#[derive(Debug, Serialize, Deserialize)]
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

        let temp = temp_c as f64;

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
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;

    let config: Config = ron::from_str(&content)
        .with_context(|| format!("failed to parse RON config: {}", path.display()))?;

    validate(&config)?;
    Ok(config)
}

fn validate(config: &Config) -> Result<()> {
    anyhow::ensure!(!config.sensors.is_empty(), "at least one sensor is required");
    anyhow::ensure!(!config.fans.is_empty(), "at least one fan is required");
    anyhow::ensure!(config.poll_interval_ms > 0, "poll_interval_ms must be > 0");
    anyhow::ensure!(
        config.derivative.window_size >= 2,
        "derivative window_size must be >= 2"
    );

    let sensor_names: Vec<&str> = config.sensors.iter().map(|s| s.name.as_str()).collect();

    for fan in &config.fans {
        anyhow::ensure!(!fan.curve.is_empty(), "fan '{}' has empty curve", fan.name);

        // Verify curve is sorted by temperature
        for window in fan.curve.windows(2) {
            anyhow::ensure!(
                window[0].temp < window[1].temp,
                "fan '{}' curve must be sorted by ascending temperature",
                fan.name
            );
        }

        // Verify all referenced sensors exist
        for sensor_ref in &fan.sensors {
            anyhow::ensure!(
                sensor_names.contains(&sensor_ref.as_str()),
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
    fn parse_ron_config() {
        let ron_str = r#"
            Config(
                poll_interval_ms: 2000,
                derivative: DerivativeConfig(
                    window_size: 10,
                    boost_threshold: 2.0,
                    decay_rate: 0.5,
                ),
                sensors: [
                    SensorConfig(name: "cpu", hwmon: "k10temp", index: 1),
                ],
                fans: [
                    FanConfig(
                        name: "test",
                        hwmon: "nct6799",
                        pwm_index: 1,
                        sensors: ["cpu"],
                        curve: [
                            CurvePoint(temp: 45, pwm: 20),
                            CurvePoint(temp: 85, pwm: 200),
                        ],
                    ),
                ],
            )
        "#;

        let config: Config = ron::from_str(ron_str).unwrap();
        assert_eq!(config.sensors.len(), 1);
        assert_eq!(config.fans.len(), 1);
        assert_eq!(config.fans[0].curve.len(), 2);
    }
}
