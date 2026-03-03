use anyhow::Result;
use std::path::PathBuf;

use super::{find_hwmon_path, read_sysfs, write_sysfs};

/// PWM enable modes for fan control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[allow(dead_code)]
pub enum PwmEnable {
    /// Full speed (no control)
    FullSpeed = 0,
    /// Manual PWM control
    Manual = 1,
    /// Automatic (thermal cruise, etc.)
    Auto = 2,
    /// SmartFan III
    SmartFan3 = 3,
    /// SmartFan IV
    SmartFan4 = 4,
    /// SmartFan IV (variant used by nct6799)
    SmartFan4v2 = 5,
}

/// A resolved PWM fan controller bound to sysfs paths.
pub struct Fan {
    pub name: String,
    /// Path to fanN_input (reads RPM)
    rpm_path: PathBuf,
    /// Path to pwmN (read/write PWM 0-255)
    pwm_path: PathBuf,
    /// Path to pwmN_enable (read/write control mode)
    enable_path: PathBuf,
    /// The original pwm_enable value before we took control (for restore on shutdown)
    original_enable: u8,
    /// Last PWM value written (to avoid redundant writes)
    last_pwm: Option<u8>,
}

impl Fan {
    /// Create a fan controller from config, resolving the hwmon sysfs path.
    pub fn from_config(name: &str, hwmon_name: &str, pwm_index: u32) -> Result<Self> {
        let hwmon_path = find_hwmon_path(hwmon_name)?;

        let rpm_path = hwmon_path.join(format!("fan{}_input", pwm_index));
        let pwm_path = hwmon_path.join(format!("pwm{}", pwm_index));
        let enable_path = hwmon_path.join(format!("pwm{}_enable", pwm_index));

        anyhow::ensure!(
            pwm_path.exists(),
            "fan '{}': {} does not exist",
            name,
            pwm_path.display()
        );

        // Save the original enable mode for restoration on shutdown
        let original_enable: u8 = read_sysfs(&enable_path)?;

        Ok(Self {
            name: name.to_string(),
            rpm_path,
            pwm_path,
            enable_path,
            original_enable,
            last_pwm: None,
        })
    }

    /// Switch fan to manual PWM control mode.
    pub fn set_manual(&self) -> Result<()> {
        write_sysfs(&self.enable_path, &(PwmEnable::Manual as u8).to_string())
    }

    /// Restore the original fan control mode (called on daemon shutdown).
    pub fn restore_original(&self) -> Result<()> {
        tracing::info!(
            fan = %self.name,
            mode = self.original_enable,
            "restoring original fan control mode"
        );
        write_sysfs(&self.enable_path, &self.original_enable.to_string())
    }

    /// Read current fan speed in RPM.
    pub fn read_rpm(&self) -> Result<u32> {
        if self.rpm_path.exists() {
            read_sysfs(&self.rpm_path)
        } else {
            Ok(0)
        }
    }

    /// Read current PWM value (0-255).
    pub fn read_pwm(&self) -> Result<u8> {
        read_sysfs(&self.pwm_path)
    }

    /// Write a PWM value (0-255), skipping if unchanged from last write.
    pub fn write_pwm(&mut self, pwm: u8) -> Result<()> {
        if self.last_pwm == Some(pwm) {
            return Ok(());
        }
        write_sysfs(&self.pwm_path, &pwm.to_string())?;
        self.last_pwm = Some(pwm);
        Ok(())
    }
}
