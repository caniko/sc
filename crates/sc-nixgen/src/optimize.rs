use sc_core::config::{Config, CurvePoint, FanPosition};
use sc_core::ipc::TuningResponse;

/// An optimized fan configuration produced from tuning data.
pub struct OptimizedFan {
    pub name: String,
    pub sensors: Vec<String>,
    pub curve: Vec<CurvePoint>,
}

/// Result of optimization: one entry per fan in the original config.
pub struct OptimizedConfig {
    pub fans: Vec<OptimizedFan>,
    pub has_tuning_data: bool,
}

/// Default thermal profiles per sensor type.
/// (t_fanon, t_target, t_throttle)
fn sensor_thermal_profile(sensor_name: &str) -> (f64, f64, f64) {
    if sensor_name.starts_with("cpu") {
        if sensor_name.contains("ccd") {
            (45.0, 78.0, 95.0)
        } else {
            (45.0, 80.0, 95.0)
        }
    } else if sensor_name.starts_with("gpu") {
        if sensor_name.contains("junction") {
            (50.0, 90.0, 110.0)
        } else if sensor_name.contains("mem") {
            (50.0, 85.0, 100.0)
        } else {
            // GPU edge
            (45.0, 80.0, 100.0)
        }
    } else {
        // Board/other sensors
        (40.0, 70.0, 90.0)
    }
}

/// Generate optimized fan configurations from tuning data.
///
/// If tuning data is available, uses the coupling matrix to:
/// 1. Reassign sensors to fans based on measured coupling strength
/// 2. Compute curve shape exponent (gamma) from effectiveness
/// 3. Generate urgency-normalized curves calibrated to thermal profiles
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
                        curve: f
                            .curve
                            .iter()
                            .map(|p| CurvePoint {
                                temp: p.temp,
                                pwm: p.pwm,
                            })
                            .collect(),
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
            // Find all sensors with significant *cooling* coupling to this fan.
            // Only negative beta means the fan cools that sensor.
            // Positive beta means running the fan heats that sensor (e.g. exhaust
            // pulling hot air past a nearby component) — exclude those.
            let mut coupled_sensors: Vec<(String, f64, f64)> = tuning
                .coupling_matrix
                .iter()
                .filter(|e| e.fan == fan_config.name && e.beta < -coupling_threshold)
                .map(|e| (e.sensor.clone(), e.beta, e.r_squared))
                .collect();

            // Sort by coupling strength (strongest first)
            coupled_sensors.sort_by(|a, b| b.1.abs().partial_cmp(&a.1.abs()).unwrap());

            // Fall back to original sensors if no coupling data for this fan
            let sensors = if coupled_sensors.is_empty() {
                fan_config.sensors.clone()
            } else {
                coupled_sensors.iter().map(|(s, _, _)| s.clone()).collect()
            };

            let max_beta = coupled_sensors
                .first()
                .map(|(_, b, _)| b.abs())
                .unwrap_or(0.0);

            // Compute gamma based on effectiveness
            let gamma = effectiveness_to_gamma(max_beta);

            // Determine the most important sensor for curve temperature range
            let primary_sensor = sensors.first().cloned().unwrap_or_default();

            // Generate urgency-based curve
            let curve = generate_urgency_curve(fan_config, &primary_sensor, gamma);

            OptimizedFan {
                name: fan_config.name.clone(),
                sensors,
                curve,
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
/// - High effectiveness (|beta| > 1.0): gamma = 1.5 (convex, gentle ramp — fan is effective)
/// - Medium effectiveness (0.3-1.0): gamma = 1.0 (linear)
/// - Low effectiveness (< 0.3): gamma = 0.7 (concave, aggressive ramp — fan needs to work harder)
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

/// Generate a 7-point urgency-normalized curve.
///
/// Uses sensor thermal profiles to set meaningful temperature breakpoints
/// rather than arbitrary linear spacing. The curve maps:
///   urgency=0 (t_fanon) → pwm_min
///   urgency=1 (t_throttle) → 255
///
/// Non-uniform urgency levels concentrate points where control matters most.
fn generate_urgency_curve(
    fan_config: &sc_core::config::FanConfig,
    primary_sensor: &str,
    gamma: f64,
) -> Vec<CurvePoint> {
    let (t_fanon, _t_target, t_throttle) = sensor_thermal_profile(primary_sensor);

    // Determine minimum PWM based on fan position
    let pwm_min: f64 = match fan_config.topology.position {
        FanPosition::CpuCooler | FanPosition::GpuCooler => 30.0,
        _ => 0.0,
    };

    // Non-uniform urgency levels: more resolution in the 0.3-0.8 range
    let urgency_levels: &[f64] = &[0.0, 0.15, 0.35, 0.55, 0.75, 0.90, 1.0];

    urgency_levels
        .iter()
        .map(|&u| {
            let temp = t_fanon + u * (t_throttle - t_fanon);
            let pwm_factor = u.powf(gamma);
            let pwm = pwm_min + pwm_factor * (255.0 - pwm_min);
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
    use sc_core::config::{AirflowDirection, FanTopology};

    fn test_topology() -> FanTopology {
        FanTopology {
            position: FanPosition::Front,
            direction: AirflowDirection::Intake,
            group: None,
        }
    }

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
    fn urgency_curve_has_7_points() {
        let fan = sc_core::config::FanConfig {
            name: "test".into(),
            hwmon: "nct6799".into(),
            pwm_index: 1,
            hwmon_instance: None,
            topology: test_topology(),
            sensors: vec!["cpu".into()],
            curve: vec![
                CurvePoint { temp: 40, pwm: 20 },
                CurvePoint { temp: 80, pwm: 200 },
            ],
        };
        let curve = generate_urgency_curve(&fan, "cpu", 1.0);
        assert_eq!(curve.len(), 7);
    }

    #[test]
    fn urgency_curve_cpu_range() {
        let fan = sc_core::config::FanConfig {
            name: "cpu_fan".into(),
            hwmon: "nct6799".into(),
            pwm_index: 1,
            hwmon_instance: None,
            topology: FanTopology {
                position: FanPosition::CpuCooler,
                direction: AirflowDirection::Exhaust,
                group: None,
            },
            sensors: vec!["cpu".into()],
            curve: vec![],
        };
        let curve = generate_urgency_curve(&fan, "cpu", 1.0);

        // First point should be at t_fanon=45
        assert_eq!(curve[0].temp, 45);
        // CPU cooler should have minimum PWM (30)
        assert_eq!(curve[0].pwm, 30);
        // Last point at t_throttle=95
        assert_eq!(curve[6].temp, 95);
        assert_eq!(curve[6].pwm, 255);
    }

    #[test]
    fn urgency_curve_case_fan_starts_at_zero() {
        let fan = sc_core::config::FanConfig {
            name: "front_fan".into(),
            hwmon: "nct6799".into(),
            pwm_index: 1,
            hwmon_instance: None,
            topology: test_topology(),
            sensors: vec!["cpu".into()],
            curve: vec![],
        };
        let curve = generate_urgency_curve(&fan, "cpu", 1.0);
        // Case fans can go to PWM 0
        assert_eq!(curve[0].pwm, 0);
    }

    #[test]
    fn urgency_curve_convex_below_linear() {
        let fan = sc_core::config::FanConfig {
            name: "test".into(),
            hwmon: "nct6799".into(),
            pwm_index: 1,
            hwmon_instance: None,
            topology: test_topology(),
            sensors: vec!["cpu".into()],
            curve: vec![],
        };
        let linear = generate_urgency_curve(&fan, "cpu", 1.0);
        let convex = generate_urgency_curve(&fan, "cpu", 1.5);
        // Midpoint of convex curve should be lower than linear
        assert!(
            convex[3].pwm <= linear[3].pwm,
            "convex midpoint {}  should be <= linear midpoint {}",
            convex[3].pwm,
            linear[3].pwm
        );
    }

    #[test]
    fn gpu_junction_thermal_profile() {
        let (t_fanon, t_target, t_throttle) = sensor_thermal_profile("gpu1_junction");
        assert_eq!(t_fanon, 50.0);
        assert_eq!(t_target, 90.0);
        assert_eq!(t_throttle, 110.0);
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
                hwmon_instance: None,
            }],
            fans: vec![sc_core::config::FanConfig {
                name: "fan".into(),
                hwmon: "nct6799".into(),
                pwm_index: 1,
                hwmon_instance: None,
                topology: test_topology(),
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
