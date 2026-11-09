use anyhow::Result;
use std::path::PathBuf;

use crate::{find_hwmon_paths, read_sysfs};

/// A resolved temperature sensor bound to a sysfs path.
pub struct Sensor {
    pub name: String,
    /// Path to tempN_input (reads millidegrees Celsius)
    input_path: PathBuf,
    /// Runtime-PM status file, for GPU chips only.
    runtime_status: Option<PathBuf>,
}

impl Sensor {
    /// Create a sensor from config, resolving the hwmon sysfs path.
    /// `instance` selects which hwmon chip to use when multiple share the same
    /// name. `None` picks the first with the requested tempN_input.
    pub fn from_config(
        name: &str,
        hwmon_name: &str,
        index: u32,
        instance: Option<u32>,
    ) -> Result<Self> {
        let mut paths = find_hwmon_paths(hwmon_name)?;
        paths.sort();
        let hwmon_path = if let Some(idx) = instance {
            paths.get(idx as usize).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "sensor '{}': '{}' has {} instances but hwmon_instance={} requested",
                    name,
                    hwmon_name,
                    paths.len(),
                    idx
                )
            })?
        } else {
            paths
                .iter()
                .find(|p| p.join(format!("temp{}_input", index)).exists())
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "sensor '{}': no '{}' hwmon instance has temp{}_input",
                        name,
                        hwmon_name,
                        index
                    )
                })?
        };

        let input_path = hwmon_path.join(format!("temp{}_input", index));

        Ok(Self {
            name: name.to_string(),
            runtime_status: runtime_status_path(&hwmon_path),
            input_path,
        })
    }

    /// Create a sensor from an already-resolved hwmon sysfs path.
    pub fn from_path(name: &str, hwmon_path: &std::path::Path, index: u32) -> Result<Self> {
        let input_path = hwmon_path.join(format!("temp{}_input", index));

        anyhow::ensure!(
            input_path.exists(),
            "sensor '{}': {} does not exist",
            name,
            input_path.display()
        );

        Ok(Self {
            name: name.to_string(),
            runtime_status: runtime_status_path(hwmon_path),
            input_path,
        })
    }

    /// True while this sensor's GPU is runtime-suspended. A suspended GPU
    /// produces no heat, and polling it can wake it, so callers skip it.
    pub fn is_parked(&self) -> bool {
        self.runtime_status
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .is_some_and(|s| matches!(s.trim(), "suspended" | "suspending"))
    }

    /// The hwmon directory this sensor belongs to.
    pub fn hwmon_dir(&self) -> Option<&std::path::Path> {
        self.input_path.parent()
    }

    /// Read the current temperature in degrees Celsius.
    pub fn read_temp_c(&self) -> Result<f64> {
        let millidegrees: i64 = read_sysfs(&self.input_path)?;
        Ok(millidegrees as f64 / 1000.0)
    }
}

fn runtime_status_path(hwmon_path: &std::path::Path) -> Option<PathBuf> {
    let chip = std::fs::read_to_string(hwmon_path.join("name")).ok()?;
    crate::is_gpu_chip(chip.trim()).then(|| hwmon_path.join("device/power/runtime_status"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspended_gpu_sensor_is_parked() {
        let dir = tempfile::tempdir().unwrap();
        let chip = dir.path();
        std::fs::create_dir_all(chip.join("device/power")).unwrap();
        std::fs::write(chip.join("name"), "amdgpu\n").unwrap();
        std::fs::write(chip.join("temp1_input"), "45000\n").unwrap();
        let status = chip.join("device/power/runtime_status");
        let sensor = Sensor::from_path("gpu", chip, 1).unwrap();

        std::fs::write(&status, "suspended\n").unwrap();
        assert!(sensor.is_parked());
        std::fs::write(&status, "active\n").unwrap();
        assert!(!sensor.is_parked());

        std::fs::write(chip.join("name"), "k10temp\n").unwrap();
        std::fs::write(&status, "suspended\n").unwrap();
        assert!(!Sensor::from_path("cpu", chip, 1).unwrap().is_parked());
    }
}
