use anyhow::Result;
use std::path::PathBuf;

use crate::{find_hwmon_paths, read_sysfs};

/// A resolved temperature sensor bound to a sysfs path.
pub struct Sensor {
    pub name: String,
    /// Path to tempN_input (reads millidegrees Celsius)
    input_path: PathBuf,
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
            input_path,
        })
    }

    /// Read the current temperature in degrees Celsius.
    pub fn read_temp_c(&self) -> Result<f64> {
        let millidegrees: i64 = read_sysfs(&self.input_path)?;
        Ok(millidegrees as f64 / 1000.0)
    }
}
