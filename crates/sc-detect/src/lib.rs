//! Hardware discovery for SmartCool.
//!
//! Scans `/sys/class/hwmon/` to discover temperature sensors and controllable
//! fans. Classifies chips as CPU, GPU, motherboard EC, or other. Provides
//! auto-naming based on chip type and sensor labels.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

// ─── Constants ───────────────────────────────────────────────────────────────

const HWMON_BASE: &str = "/sys/class/hwmon";
const MAX_TEMP_INDEX: u32 = 16;
const MAX_PWM_INDEX: u32 = 8;

const CPU_CHIPS: &[&str] = &["k10temp", "coretemp", "zenpower"];
const GPU_CHIPS: &[&str] = &["amdgpu", "nvidia", "nouveau"];
const EC_PREFIXES: &[&str] = &["nct6", "it8", "w83", "f71", "asus"];

// ─── Public Types ────────────────────────────────────────────────────────────

/// Classification of an hwmon chip by function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChipClass {
    CpuTemp,
    GpuTemp,
    MotherboardEc,
    Other,
}

/// A discovered hwmon chip with its sysfs path and classification.
#[derive(Debug)]
pub struct HwmonChip {
    pub name: String,
    pub path: PathBuf,
    pub class: ChipClass,
}

/// A discovered temperature sensor.
#[derive(Debug)]
pub struct DetectedSensor {
    pub auto_name: String,
    pub hwmon_chip: String,
    pub index: u32,
    pub label: Option<String>,
    pub current_temp_c: f64,
}

/// A discovered controllable fan (PWM channel).
#[derive(Debug)]
pub struct DetectedFan {
    pub auto_name: String,
    pub hwmon_chip: String,
    pub pwm_index: u32,
    pub current_rpm: Option<u32>,
}

/// Complete hardware detection result.
pub struct DetectionResult {
    pub chips: Vec<HwmonChip>,
    pub sensors: Vec<DetectedSensor>,
    pub fans: Vec<DetectedFan>,
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Detect all available hardware sensors and fans.
///
/// When `include_ec` is false, only CPU and GPU temperature sensors are
/// discovered. Fans are always scanned from all chips (since EC chips
/// typically host the PWM controllers).
pub fn detect_hardware(include_ec: bool) -> Result<DetectionResult> {
    let chips = enumerate_chips()?;
    let sensors = discover_sensors(&chips, include_ec)?;
    let fans = discover_fans(&chips)?;
    Ok(DetectionResult {
        chips,
        sensors,
        fans,
    })
}

/// Classify a chip name by its function.
pub fn classify_chip(name: &str) -> ChipClass {
    if CPU_CHIPS.contains(&name) {
        ChipClass::CpuTemp
    } else if GPU_CHIPS.contains(&name) {
        ChipClass::GpuTemp
    } else if EC_PREFIXES.iter().any(|p| name.starts_with(p)) {
        ChipClass::MotherboardEc
    } else {
        ChipClass::Other
    }
}

/// Enumerate all hwmon chips in `/sys/class/hwmon/`.
pub fn enumerate_chips() -> Result<Vec<HwmonChip>> {
    let base = Path::new(HWMON_BASE);
    if !base.exists() {
        anyhow::bail!("{} does not exist — are you running on Linux?", HWMON_BASE);
    }

    let mut chips = Vec::new();
    let entries = fs::read_dir(base).context("failed to read /sys/class/hwmon")?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name_path = path.join("name");

        let name = match read_sysfs_string(&name_path) {
            Some(n) => n,
            None => continue,
        };

        let class = classify_chip(&name);
        chips.push(HwmonChip { name, path, class });
    }

    // Sort: CPU first, GPU second, EC third, Other last
    chips.sort_by_key(|c| match c.class {
        ChipClass::CpuTemp => 0,
        ChipClass::GpuTemp => 1,
        ChipClass::MotherboardEc => 2,
        ChipClass::Other => 3,
    });

    Ok(chips)
}

/// Discover temperature sensors from the given chips.
///
/// When `include_ec` is false, only CPU and GPU chips are scanned for
/// temperature sensors.
pub fn discover_sensors(chips: &[HwmonChip], include_ec: bool) -> Result<Vec<DetectedSensor>> {
    let mut sensors = Vec::new();
    let mut existing_names: Vec<String> = Vec::new();

    // Count chips per name for multi-GPU detection
    let gpu_chip_count = chips
        .iter()
        .filter(|c| c.class == ChipClass::GpuTemp)
        .count();

    let mut gpu_index = 0u32;

    for chip in chips {
        let scan = match chip.class {
            ChipClass::CpuTemp | ChipClass::GpuTemp => true,
            ChipClass::MotherboardEc => include_ec,
            ChipClass::Other => false,
        };
        if !scan {
            continue;
        }

        let is_multi_gpu = chip.class == ChipClass::GpuTemp && gpu_chip_count > 1;
        let current_gpu_idx = if chip.class == ChipClass::GpuTemp {
            let idx = gpu_index;
            gpu_index += 1;
            Some(idx)
        } else {
            None
        };

        for index in 1..=MAX_TEMP_INDEX {
            let input_path = chip.path.join(format!("temp{}_input", index));
            if !input_path.exists() {
                continue;
            }

            let millidegrees = match read_sysfs_i64(&input_path) {
                Some(v) => v,
                None => {
                    eprintln!(
                        "warning: could not read {}, skipping",
                        input_path.display()
                    );
                    continue;
                }
            };

            let temp_c = millidegrees as f64 / 1000.0;

            // Skip obviously invalid readings
            if temp_c < -20.0 || temp_c > 150.0 {
                continue;
            }

            let label_path = chip.path.join(format!("temp{}_label", index));
            let label = read_sysfs_string(&label_path);

            let auto_name = auto_name_sensor(
                chip,
                index,
                &label,
                &existing_names,
                is_multi_gpu,
                current_gpu_idx,
            );
            existing_names.push(auto_name.clone());

            sensors.push(DetectedSensor {
                auto_name,
                hwmon_chip: chip.name.clone(),
                index,
                label,
                current_temp_c: temp_c,
            });
        }
    }

    Ok(sensors)
}

/// Discover controllable fans (PWM channels) from all chips.
pub fn discover_fans(chips: &[HwmonChip]) -> Result<Vec<DetectedFan>> {
    let mut fans = Vec::new();
    let mut fan_counter = 0usize;

    let gpu_chip_count = chips
        .iter()
        .filter(|c| c.class == ChipClass::GpuTemp)
        .count();
    let mut gpu_fan_index = 0u32;

    for chip in chips {
        for pwm_index in 1..=MAX_PWM_INDEX {
            let pwm_path = chip.path.join(format!("pwm{}", pwm_index));
            if !pwm_path.exists() {
                continue;
            }

            let rpm_path = chip.path.join(format!("fan{}_input", pwm_index));
            let current_rpm = read_sysfs_i64(&rpm_path).map(|v| v as u32);

            let is_multi_gpu = chip.class == ChipClass::GpuTemp && gpu_chip_count > 1;

            let auto_name = if chip.class == ChipClass::GpuTemp {
                let name = if is_multi_gpu {
                    format!("gpu{}_fan", gpu_fan_index)
                } else {
                    "gpu_fan".to_string()
                };
                gpu_fan_index += 1;
                name
            } else {
                fan_counter += 1;
                format!("fan{}", fan_counter)
            };

            fans.push(DetectedFan {
                auto_name,
                hwmon_chip: chip.name.clone(),
                pwm_index,
                current_rpm,
            });
        }
    }

    Ok(fans)
}

// ─── Internal Helpers ────────────────────────────────────────────────────────

fn auto_name_sensor(
    chip: &HwmonChip,
    index: u32,
    label: &Option<String>,
    existing: &[String],
    is_multi_gpu: bool,
    gpu_idx: Option<u32>,
) -> String {
    let base = match chip.class {
        ChipClass::CpuTemp => auto_name_cpu_sensor(index, label),
        ChipClass::GpuTemp => auto_name_gpu_sensor(label, is_multi_gpu, gpu_idx.unwrap_or(0)),
        ChipClass::MotherboardEc => auto_name_ec_sensor(index, label),
        ChipClass::Other => format!("{}_temp{}", chip.name, index),
    };

    // Deduplicate
    if existing.contains(&base) {
        let deduped = format!("{}_{}", base, index);
        if existing.contains(&deduped) {
            format!("{}_{}_{}", chip.name, base, index)
        } else {
            deduped
        }
    } else {
        base
    }
}

fn auto_name_cpu_sensor(index: u32, label: &Option<String>) -> String {
    match label.as_deref() {
        Some("Tctl") | Some("Tdie") => "cpu".to_string(),
        Some(l) if l.starts_with("Tccd") => {
            let suffix = l.trim_start_matches("Tccd");
            format!("cpu_ccd{}", suffix)
        }
        Some(l) if l.starts_with("Core ") => {
            let suffix = l.trim_start_matches("Core ").replace(' ', "");
            format!("cpu_core{}", suffix)
        }
        Some(l) => format!("cpu_{}", sanitize_label(l)),
        None if index == 1 => "cpu".to_string(),
        None => format!("cpu_temp{}", index),
    }
}

fn auto_name_gpu_sensor(label: &Option<String>, is_multi_gpu: bool, gpu_idx: u32) -> String {
    let prefix = if is_multi_gpu {
        format!("gpu{}", gpu_idx)
    } else {
        "gpu".to_string()
    };

    match label.as_deref() {
        Some("edge") => prefix,
        Some("junction") => format!("{}_junction", prefix),
        Some("mem") => format!("{}_mem", prefix),
        Some(l) => format!("{}_{}", prefix, sanitize_label(l)),
        None => prefix,
    }
}

fn auto_name_ec_sensor(index: u32, label: &Option<String>) -> String {
    match label.as_deref() {
        Some(l) => format!("board_{}", sanitize_label(l)),
        None => format!("board_temp{}", index),
    }
}

/// Sanitize a sysfs label into a valid config name (lowercase, no spaces).
fn sanitize_label(label: &str) -> String {
    label
        .to_lowercase()
        .replace(' ', "_")
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '_', "")
}

fn read_sysfs_string(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn read_sysfs_i64(path: &Path) -> Option<i64> {
    read_sysfs_string(path)?.parse().ok()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_known_chips() {
        assert_eq!(classify_chip("k10temp"), ChipClass::CpuTemp);
        assert_eq!(classify_chip("coretemp"), ChipClass::CpuTemp);
        assert_eq!(classify_chip("zenpower"), ChipClass::CpuTemp);
        assert_eq!(classify_chip("amdgpu"), ChipClass::GpuTemp);
        assert_eq!(classify_chip("nvidia"), ChipClass::GpuTemp);
        assert_eq!(classify_chip("nouveau"), ChipClass::GpuTemp);
        assert_eq!(classify_chip("nct6799"), ChipClass::MotherboardEc);
        assert_eq!(classify_chip("nct6775"), ChipClass::MotherboardEc);
        assert_eq!(classify_chip("it8622"), ChipClass::MotherboardEc);
        assert_eq!(classify_chip("w83627"), ChipClass::MotherboardEc);
        assert_eq!(classify_chip("asus_wmi_sensors"), ChipClass::MotherboardEc);
        assert_eq!(classify_chip("acpitz"), ChipClass::Other);
        assert_eq!(classify_chip("iwlwifi_1"), ChipClass::Other);
    }

    #[test]
    fn cpu_sensor_naming() {
        assert_eq!(auto_name_cpu_sensor(1, &Some("Tctl".into())), "cpu");
        assert_eq!(auto_name_cpu_sensor(2, &Some("Tdie".into())), "cpu");
        assert_eq!(auto_name_cpu_sensor(3, &Some("Tccd1".into())), "cpu_ccd1");
        assert_eq!(auto_name_cpu_sensor(4, &Some("Tccd2".into())), "cpu_ccd2");
        assert_eq!(auto_name_cpu_sensor(1, &Some("Core 0".into())), "cpu_core0");
        assert_eq!(auto_name_cpu_sensor(1, &None), "cpu");
        assert_eq!(auto_name_cpu_sensor(3, &None), "cpu_temp3");
    }

    #[test]
    fn gpu_sensor_naming_single() {
        assert_eq!(auto_name_gpu_sensor(&Some("edge".into()), false, 0), "gpu");
        assert_eq!(
            auto_name_gpu_sensor(&Some("junction".into()), false, 0),
            "gpu_junction"
        );
        assert_eq!(
            auto_name_gpu_sensor(&Some("mem".into()), false, 0),
            "gpu_mem"
        );
        assert_eq!(auto_name_gpu_sensor(&None, false, 0), "gpu");
    }

    #[test]
    fn gpu_sensor_naming_multi() {
        assert_eq!(
            auto_name_gpu_sensor(&Some("edge".into()), true, 0),
            "gpu0"
        );
        assert_eq!(
            auto_name_gpu_sensor(&Some("edge".into()), true, 1),
            "gpu1"
        );
        assert_eq!(
            auto_name_gpu_sensor(&Some("junction".into()), true, 0),
            "gpu0_junction"
        );
    }

    #[test]
    fn ec_sensor_naming() {
        assert_eq!(
            auto_name_ec_sensor(1, &Some("SYSTIN".into())),
            "board_systin"
        );
        assert_eq!(
            auto_name_ec_sensor(2, &Some("CPUTIN".into())),
            "board_cputin"
        );
        assert_eq!(auto_name_ec_sensor(3, &None), "board_temp3");
    }

    #[test]
    fn sanitize_labels() {
        assert_eq!(sanitize_label("Core 0"), "core_0");
        assert_eq!(sanitize_label("SYSTIN"), "systin");
        assert_eq!(sanitize_label("edge"), "edge");
        assert_eq!(sanitize_label("Temp +3.3V"), "temp_33v");
    }

    #[test]
    fn deduplication() {
        let chip = HwmonChip {
            name: "k10temp".into(),
            path: PathBuf::from("/sys/class/hwmon/hwmon0"),
            class: ChipClass::CpuTemp,
        };
        let existing = vec!["cpu".to_string()];
        // Second "cpu" sensor gets deduplicated
        let name = auto_name_sensor(&chip, 2, &Some("Tdie".into()), &existing, false, None);
        assert_eq!(name, "cpu_2");
    }
}
