//! Config builder — maps sc-detect's DetectionResult into a SmartCool Config.
//!
//! Owns the policy decisions: default curves, fan-to-sensor mapping heuristics,
//! and derivative/tuning defaults.

use sc_core::config::{Config, CurvePoint, DerivativeConfig, FanConfig, SensorConfig};
use sc_detect::{ChipClass, DetectedFan, DetectedSensor, DetectionResult};

// ─── Default Curves ──────────────────────────────────────────────────────────

const CPU_CURVE: &[(u32, u8)] = &[
    (40, 0),
    (50, 40),
    (60, 80),
    (70, 140),
    (80, 200),
    (90, 255),
];

const GPU_CURVE: &[(u32, u8)] = &[
    (40, 0),
    (55, 60),
    (65, 120),
    (75, 180),
    (85, 230),
    (95, 255),
];

const CASE_CURVE: &[(u32, u8)] = &[
    (40, 0),
    (50, 30),
    (60, 60),
    (70, 100),
    (80, 160),
    (90, 255),
];

// ─── Public API ──────────────────────────────────────────────────────────────

/// Build a complete SmartCool Config from detection results.
pub fn build_config(result: &DetectionResult) -> Config {
    let sensors: Vec<SensorConfig> = result
        .sensors
        .iter()
        .map(|s| SensorConfig {
            name: s.auto_name.clone(),
            hwmon: s.hwmon_chip.clone(),
            index: s.index,
        })
        .collect();

    let fan_sensor_map = map_fans_to_sensors(&result.sensors, &result.fans);

    let fans: Vec<FanConfig> = result
        .fans
        .iter()
        .zip(fan_sensor_map.iter())
        .map(|(fan, mapped_sensors)| {
            let curve = select_curve(fan, mapped_sensors);
            FanConfig {
                name: fan.auto_name.clone(),
                hwmon: fan.hwmon_chip.clone(),
                pwm_index: fan.pwm_index,
                sensors: mapped_sensors.iter().map(|s| s.auto_name.clone()).collect(),
                curve,
            }
        })
        .collect();

    Config {
        poll_interval_ms: 2000,
        derivative: DerivativeConfig {
            window_size: 10,
            boost_threshold: 2.0,
            decay_rate: 0.5,
        },
        sensors,
        fans,
        tuning: Default::default(),
    }
}

/// Print a human-readable summary of detected hardware to stderr.
pub fn print_summary(result: &DetectionResult) {
    eprintln!("Detected hardware:");

    // Chips
    if !result.chips.is_empty() {
        let chip_list: Vec<String> = result
            .chips
            .iter()
            .map(|c| {
                let class = match c.class {
                    ChipClass::CpuTemp => "CPU",
                    ChipClass::GpuTemp => "GPU",
                    ChipClass::MotherboardEc => "EC",
                    ChipClass::Other => "other",
                };
                format!("{} ({})", c.name, class)
            })
            .collect();
        eprintln!("  Chips: {}", chip_list.join(", "));
    }

    // Sensors
    if result.sensors.is_empty() {
        eprintln!("  Sensors: none found");
    } else {
        eprintln!("  Sensors:");
        for s in &result.sensors {
            let label = s.label.as_deref().unwrap_or("-");
            eprintln!(
                "    {:<16} {:<12} temp{}  {:<12} {:>5.1} C",
                s.auto_name, s.hwmon_chip, s.index, label, s.current_temp_c
            );
        }
    }

    // Fans
    if result.fans.is_empty() {
        eprintln!("  Fans: none found");
    } else {
        eprintln!("  Fans:");
        for f in &result.fans {
            let rpm = match f.current_rpm {
                Some(0) => "0 RPM (stopped?)".to_string(),
                Some(r) => format!("{} RPM", r),
                None => "no RPM sensor".to_string(),
            };
            eprintln!(
                "    {:<16} {:<12} pwm{}   {}",
                f.auto_name, f.hwmon_chip, f.pwm_index, rpm
            );
        }
    }

    eprintln!();
}

// ─── Internal ────────────────────────────────────────────────────────────────

/// Map each fan to the sensors it should respond to.
fn map_fans_to_sensors<'a>(
    sensors: &'a [DetectedSensor],
    fans: &[DetectedFan],
) -> Vec<Vec<&'a DetectedSensor>> {
    let cpu_sensors: Vec<&DetectedSensor> = sensors
        .iter()
        .filter(|s| sc_detect::classify_chip(&s.hwmon_chip) == ChipClass::CpuTemp)
        .collect();

    let gpu_sensors: Vec<&DetectedSensor> = sensors
        .iter()
        .filter(|s| sc_detect::classify_chip(&s.hwmon_chip) == ChipClass::GpuTemp)
        .collect();

    fans.iter()
        .map(|fan| {
            let fan_class = sc_detect::classify_chip(&fan.hwmon_chip);
            match fan_class {
                // GPU fans → GPU sensors from the same chip
                ChipClass::GpuTemp => {
                    let same_chip: Vec<&DetectedSensor> = gpu_sensors
                        .iter()
                        .filter(|s| s.hwmon_chip == fan.hwmon_chip)
                        .copied()
                        .collect();
                    if same_chip.is_empty() {
                        // Fallback: all GPU sensors
                        gpu_sensors.clone()
                    } else {
                        same_chip
                    }
                }
                // EC/motherboard fans → all CPU + GPU sensors
                _ => {
                    let mut all: Vec<&DetectedSensor> = Vec::new();
                    all.extend(&cpu_sensors);
                    all.extend(&gpu_sensors);
                    if all.is_empty() {
                        // No CPU/GPU sensors found, use whatever we have
                        sensors.iter().collect()
                    } else {
                        all
                    }
                }
            }
        })
        .collect()
}

/// Select the appropriate default curve based on fan context.
fn select_curve(fan: &DetectedFan, mapped_sensors: &[&DetectedSensor]) -> Vec<CurvePoint> {
    let fan_class = sc_detect::classify_chip(&fan.hwmon_chip);

    let curve_points = if fan_class == ChipClass::GpuTemp {
        // Fan on a GPU chip → GPU curve
        GPU_CURVE
    } else if mapped_sensors
        .iter()
        .all(|s| sc_detect::classify_chip(&s.hwmon_chip) == ChipClass::GpuTemp)
    {
        // Fan mapped only to GPU sensors → GPU curve
        GPU_CURVE
    } else if mapped_sensors
        .iter()
        .any(|s| sc_detect::classify_chip(&s.hwmon_chip) == ChipClass::CpuTemp)
    {
        // Fan mapped to at least one CPU sensor → CPU curve
        CPU_CURVE
    } else {
        CASE_CURVE
    };

    curve_points
        .iter()
        .map(|&(temp, pwm)| CurvePoint { temp, pwm })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_detect::{DetectedFan, DetectedSensor, DetectionResult, HwmonChip};
    use std::path::PathBuf;

    fn cpu_sensor() -> DetectedSensor {
        DetectedSensor {
            auto_name: "cpu".into(),
            hwmon_chip: "k10temp".into(),
            index: 1,
            label: Some("Tctl".into()),
            current_temp_c: 45.0,
        }
    }

    fn gpu_sensor() -> DetectedSensor {
        DetectedSensor {
            auto_name: "gpu".into(),
            hwmon_chip: "amdgpu".into(),
            index: 1,
            label: Some("edge".into()),
            current_temp_c: 38.0,
        }
    }

    fn ec_fan() -> DetectedFan {
        DetectedFan {
            auto_name: "fan1".into(),
            hwmon_chip: "nct6799".into(),
            pwm_index: 1,
            current_rpm: Some(800),
        }
    }

    fn gpu_fan() -> DetectedFan {
        DetectedFan {
            auto_name: "gpu_fan".into(),
            hwmon_chip: "amdgpu".into(),
            pwm_index: 1,
            current_rpm: Some(1200),
        }
    }

    #[test]
    fn build_config_basic() {
        let result = DetectionResult {
            chips: vec![
                HwmonChip {
                    name: "k10temp".into(),
                    path: PathBuf::from("/sys/class/hwmon/hwmon0"),
                    class: ChipClass::CpuTemp,
                },
                HwmonChip {
                    name: "nct6799".into(),
                    path: PathBuf::from("/sys/class/hwmon/hwmon1"),
                    class: ChipClass::MotherboardEc,
                },
            ],
            sensors: vec![cpu_sensor()],
            fans: vec![ec_fan()],
        };

        let config = build_config(&result);
        assert_eq!(config.sensors.len(), 1);
        assert_eq!(config.sensors[0].name, "cpu");
        assert_eq!(config.fans.len(), 1);
        assert_eq!(config.fans[0].sensors, vec!["cpu"]);
        assert_eq!(config.fans[0].curve.len(), CPU_CURVE.len());
    }

    #[test]
    fn gpu_fan_gets_gpu_curve() {
        let result = DetectionResult {
            chips: vec![],
            sensors: vec![cpu_sensor(), gpu_sensor()],
            fans: vec![gpu_fan()],
        };

        let config = build_config(&result);
        assert_eq!(config.fans[0].curve.len(), GPU_CURVE.len());
        assert_eq!(config.fans[0].curve[0].temp, GPU_CURVE[0].0);
    }

    #[test]
    fn ec_fan_maps_to_all_cpu_gpu_sensors() {
        let result = DetectionResult {
            chips: vec![],
            sensors: vec![cpu_sensor(), gpu_sensor()],
            fans: vec![ec_fan()],
        };

        let config = build_config(&result);
        assert_eq!(config.fans[0].sensors, vec!["cpu", "gpu"]);
    }

}
