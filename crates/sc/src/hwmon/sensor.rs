use anyhow::Result;
use std::path::PathBuf;

use super::{find_hwmon_path, read_sysfs};

/// A resolved temperature sensor bound to a sysfs path.
pub struct Sensor {
    pub name: String,
    /// Path to tempN_input (reads millidegrees Celsius)
    input_path: PathBuf,
    /// Path to tempN_label (optional human-readable label)
    #[allow(dead_code)]
    label_path: PathBuf,
}

impl Sensor {
    /// Create a sensor from config, resolving the hwmon sysfs path.
    pub fn from_config(name: &str, hwmon_name: &str, index: u32) -> Result<Self> {
        let hwmon_path = find_hwmon_path(hwmon_name)?;

        let input_path = hwmon_path.join(format!("temp{}_input", index));
        let label_path = hwmon_path.join(format!("temp{}_label", index));

        anyhow::ensure!(
            input_path.exists(),
            "sensor '{}': {} does not exist",
            name,
            input_path.display()
        );

        Ok(Self {
            name: name.to_string(),
            input_path,
            label_path,
        })
    }

    /// Read the current temperature in degrees Celsius.
    pub fn read_temp_c(&self) -> Result<f64> {
        let millidegrees: i64 = read_sysfs(&self.input_path)?;
        Ok(millidegrees as f64 / 1000.0)
    }

    /// Read the sensor label, if available.
    #[allow(dead_code)]
    pub fn read_label(&self) -> Option<String> {
        std::fs::read_to_string(&self.label_path)
            .ok()
            .map(|s| s.trim().to_string())
    }
}
