use sc_core::config::{Config, CurvePoint};
use sc_core::ipc::TuningResponse;

/// An optimized fan configuration produced from tuning data.
pub struct OptimizedFan {
    pub name: String,
    pub sensors: Vec<String>,
    pub curve: Vec<CurvePoint>,
    pub gamma: f64,
    pub max_beta: f64,
}

/// Result of optimization: one entry per fan in the original config.
pub struct OptimizedConfig {
    pub fans: Vec<OptimizedFan>,
    pub has_tuning_data: bool,
}

/// Generate optimized fan configurations from tuning data.
///
/// If tuning data is available, uses the coupling matrix to:
/// 1. Reassign sensors to fans based on measured coupling strength
/// 2. Compute curve shape exponent (gamma) from effectiveness
/// 3. Generate new 5-point curves calibrated to measured gains
pub fn optimize_config(
    config: &Config,
    tuning: Option<&TuningResponse>,
    coupling_threshold: f64,
) -> OptimizedConfig {
    let tuning = match tuning {
        Some(t) if !t.coupling_matrix.is_empty() => t,
        _ => {
            // No tuning data — return original curves unchanged
            return OptimizedConfig {
                fans: config
                    .fans
                    .iter()
                    .map(|f| OptimizedFan {
                        name: f.name.clone(),
                        sensors: f.sensors.clone(),
                        curve: f.curve.iter().map(|p| CurvePoint { temp: p.temp, pwm: p.pwm }).collect(),
                        gamma: 1.0,
                        max_beta: 0.0,
                    })
                    .collect(),
                has_tuning_data: false,
            };
        }
    };

    let fans = config
        .fans
        .iter()
        .map(|fan_config| {
            // Find all sensors with significant coupling to this fan
            let mut coupled_sensors: Vec<(String, f64)> = tuning
                .coupling_matrix
                .iter()
                .filter(|e| e.fan == fan_config.name && e.beta.abs() >= coupling_threshold)
                .map(|e| (e.sensor.clone(), e.beta))
                .collect();

            // Sort by coupling strength (strongest first)
            coupled_sensors.sort_by(|a, b| b.1.abs().partial_cmp(&a.1.abs()).unwrap());

            // Fall back to original sensors if no coupling data for this fan
            let sensors = if coupled_sensors.is_empty() {
                fan_config.sensors.clone()
            } else {
                coupled_sensors.iter().map(|(s, _)| s.clone()).collect()
            };

            let max_beta = coupled_sensors
                .first()
                .map(|(_, b)| b.abs())
                .unwrap_or(0.0);

            // Compute gamma based on effectiveness
            let gamma = effectiveness_to_gamma(max_beta);

            // Generate optimized curve
            let curve = generate_curve(fan_config, gamma);

            OptimizedFan {
                name: fan_config.name.clone(),
                sensors,
                curve,
                gamma,
                max_beta,
            }
        })
        .collect();

    OptimizedConfig {
        fans,
        has_tuning_data: true,
    }
}

/// Map coupling effectiveness to curve shape exponent.
///
/// - High effectiveness (|beta| > 1.0): gamma = 1.5 (convex, gentle ramp)
/// - Medium effectiveness (0.3-1.0): gamma = 1.0 (linear)
/// - Low effectiveness (< 0.3): gamma = 0.7 (concave, aggressive ramp)
fn effectiveness_to_gamma(beta_abs: f64) -> f64 {
    if beta_abs > 1.0 {
        1.5
    } else if beta_abs >= 0.3 {
        // Linear interpolation from 1.0 to 1.5 across [0.3, 1.0]
        1.0 + (beta_abs - 0.3) / 0.7 * 0.5
    } else if beta_abs > 0.0 {
        // Linear interpolation from 0.7 to 1.0 across [0.0, 0.3]
        0.7 + beta_abs / 0.3 * 0.3
    } else {
        1.0 // No data, default linear
    }
}

/// Generate a 5-point curve using the power-law shape.
fn generate_curve(
    fan_config: &sc_core::config::FanConfig,
    gamma: f64,
) -> Vec<CurvePoint> {
    let t_low = fan_config.curve.first().map(|p| p.temp).unwrap_or(45) as f64;
    let t_high = fan_config.curve.last().map(|p| p.temp).unwrap_or(85) as f64;
    let pwm_min = fan_config.curve.first().map(|p| p.pwm).unwrap_or(20) as f64;
    let pwm_max = fan_config.curve.last().map(|p| p.pwm).unwrap_or(255) as f64;

    let points = 5;
    (0..points)
        .map(|i| {
            let frac = i as f64 / (points - 1) as f64;
            let temp = t_low + frac * (t_high - t_low);
            let base_factor = frac.powf(gamma);
            let pwm = pwm_min + base_factor * (pwm_max - pwm_min);
            CurvePoint {
                temp: temp.round() as u32,
                pwm: pwm.round().clamp(0.0, 255.0) as u8,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_high_effectiveness() {
        assert!((effectiveness_to_gamma(1.5) - 1.5).abs() < 0.01);
        assert!((effectiveness_to_gamma(2.0) - 1.5).abs() < 0.01);
    }

    #[test]
    fn gamma_medium_effectiveness() {
        assert!((effectiveness_to_gamma(0.65) - 1.25).abs() < 0.01);
    }

    #[test]
    fn gamma_low_effectiveness() {
        assert!((effectiveness_to_gamma(0.15) - 0.85).abs() < 0.01);
    }

    #[test]
    fn gamma_zero() {
        assert!((effectiveness_to_gamma(0.0) - 1.0).abs() < 0.01);
    }

    #[test]
    fn generate_curve_linear() {
        let fan = sc_core::config::FanConfig {
            name: "test".into(),
            hwmon: "nct6799".into(),
            pwm_index: 1,
            sensors: vec!["cpu".into()],
            curve: vec![
                CurvePoint { temp: 40, pwm: 20 },
                CurvePoint { temp: 80, pwm: 200 },
            ],
        };
        let curve = generate_curve(&fan, 1.0);
        assert_eq!(curve.len(), 5);
        assert_eq!(curve[0].temp, 40);
        assert_eq!(curve[0].pwm, 20);
        assert_eq!(curve[4].temp, 80);
        assert_eq!(curve[4].pwm, 200);
        // Midpoint should be linear: (20+200)/2 = 110
        assert_eq!(curve[2].pwm, 110);
    }

    #[test]
    fn generate_curve_convex() {
        let fan = sc_core::config::FanConfig {
            name: "test".into(),
            hwmon: "nct6799".into(),
            pwm_index: 1,
            sensors: vec!["cpu".into()],
            curve: vec![
                CurvePoint { temp: 40, pwm: 20 },
                CurvePoint { temp: 80, pwm: 200 },
            ],
        };
        let curve = generate_curve(&fan, 1.5);
        // With gamma=1.5, midpoint should be below linear (convex curve)
        assert!(curve[2].pwm < 110, "midpoint pwm={}, expected < 110", curve[2].pwm);
    }

    #[test]
    fn generate_curve_concave() {
        let fan = sc_core::config::FanConfig {
            name: "test".into(),
            hwmon: "nct6799".into(),
            pwm_index: 1,
            sensors: vec!["cpu".into()],
            curve: vec![
                CurvePoint { temp: 40, pwm: 20 },
                CurvePoint { temp: 80, pwm: 200 },
            ],
        };
        let curve = generate_curve(&fan, 0.7);
        // With gamma=0.7, midpoint should be above linear (concave curve)
        assert!(curve[2].pwm > 110, "midpoint pwm={}, expected > 110", curve[2].pwm);
    }

    #[test]
    fn optimize_without_tuning_keeps_original() {
        let config = sc_core::config::Config {
            poll_interval_ms: 2000,
            derivative: sc_core::config::DerivativeConfig {
                window_size: 10,
                boost_threshold: 2.0,
                decay_rate: 0.5,
            },
            sensors: vec![sc_core::config::SensorConfig {
                name: "cpu".into(),
                hwmon: "k10temp".into(),
                index: 1,
            }],
            fans: vec![sc_core::config::FanConfig {
                name: "fan".into(),
                hwmon: "nct6799".into(),
                pwm_index: 1,
                sensors: vec!["cpu".into()],
                curve: vec![
                    CurvePoint { temp: 45, pwm: 30 },
                    CurvePoint { temp: 85, pwm: 200 },
                ],
            }],
            tuning: Default::default(),
        };

        let result = optimize_config(&config, None, 0.1);
        assert!(!result.has_tuning_data);
        assert_eq!(result.fans[0].curve[0].pwm, 30);
        assert_eq!(result.fans[0].curve[1].pwm, 200);
    }
}
