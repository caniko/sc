use anyhow::Result;
use std::path::PathBuf;

use crate::{find_hwmon_paths, read_sysfs, write_sysfs};

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
    /// The original pwm value before we took control (for restore on shutdown)
    original_pwm: u8,
    /// Last PWM value written (to avoid redundant writes)
    last_pwm: Option<u8>,
}

impl Fan {
    /// Create a fan controller from config, resolving the hwmon sysfs path.
    /// `instance` selects which hwmon chip to use when multiple share the same
    /// name (e.g. two "amdgpu"). `None` picks the first with the requested pwmN.
    pub fn from_config(
        name: &str,
        hwmon_name: &str,
        pwm_index: u32,
        instance: Option<u32>,
    ) -> Result<Self> {
        let mut paths = find_hwmon_paths(hwmon_name)?;
        paths.sort();
        let hwmon_path = if let Some(idx) = instance {
            paths.get(idx as usize).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "fan '{}': '{}' has {} instances but hwmon_instance={} requested",
                    name,
                    hwmon_name,
                    paths.len(),
                    idx
                )
            })?
        } else {
            paths
                .iter()
                .find(|p| p.join(format!("pwm{}", pwm_index)).exists())
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "fan '{}': no '{}' hwmon instance has pwm{}",
                        name,
                        hwmon_name,
                        pwm_index
                    )
                })?
        };

        let rpm_path = hwmon_path.join(format!("fan{}_input", pwm_index));
        let pwm_path = hwmon_path.join(format!("pwm{}", pwm_index));
        let enable_path = hwmon_path.join(format!("pwm{}_enable", pwm_index));

        // Save the original enable mode for restoration on shutdown
        let original_enable: u8 = read_sysfs(&enable_path)?;
        let original_pwm: u8 = read_sysfs(&pwm_path)?;

        Ok(Self {
            name: name.to_string(),
            rpm_path,
            pwm_path,
            enable_path,
            original_enable,
            original_pwm,
            last_pwm: None,
        })
    }

    /// Create a fan controller from an already-resolved hwmon sysfs path.
    /// Use this when the caller already knows the exact hwmon directory
    /// (e.g. from sc-detect which enumerates all hwmon instances).
    pub fn from_path(name: &str, hwmon_path: &std::path::Path, pwm_index: u32) -> Result<Self> {
        let rpm_path = hwmon_path.join(format!("fan{}_input", pwm_index));
        let pwm_path = hwmon_path.join(format!("pwm{}", pwm_index));
        let enable_path = hwmon_path.join(format!("pwm{}_enable", pwm_index));

        anyhow::ensure!(
            pwm_path.exists(),
            "fan '{}': {} does not exist",
            name,
            pwm_path.display()
        );

        let original_enable: u8 = read_sysfs(&enable_path)?;
        let original_pwm: u8 = read_sysfs(&pwm_path)?;

        Ok(Self {
            name: name.to_string(),
            rpm_path,
            pwm_path,
            enable_path,
            original_enable,
            original_pwm,
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
        // Restore the duty cycle before handing control back to the kernel or
        // firmware.  This avoids leaving a user-selected PWM behind when the
        // original mode is manual or when a driver applies it immediately.
        let mut errors = Vec::new();
        if let Err(error) = write_sysfs(&self.pwm_path, &self.original_pwm.to_string()) {
            errors.push(format!("PWM restoration failed: {error:#}"));
        }
        // Always attempt to restore the control mode, even when the PWM write
        // was rejected by a driver.  Leaving a fan in manual mode is the more
        // dangerous failure and must not be hidden behind the first error.
        if let Err(error) = write_sysfs(&self.enable_path, &self.original_enable.to_string()) {
            errors.push(format!("control-mode restoration failed: {error:#}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("fan '{}': {}", self.name, errors.join("; "))
        }
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

/// Owns a temporary manual-control session for one or more fans.
///
/// The benchmark and identification commands must never leave a machine in a
/// mutated fan state when they fail.  Holding this guard for the whole
/// transaction makes restoration unconditional on every normal return,
/// `anyhow` error, and Rust panic.  The signal handler used by the CLI turns
/// SIGINT/SIGTERM into an ordinary error so this drop path also runs for an
/// interrupted operation.
pub struct FanControlGuard<'a> {
    fans: &'a mut [Fan],
    restored: bool,
}

impl<'a> FanControlGuard<'a> {
    pub fn new(fans: &'a mut [Fan]) -> Self {
        Self {
            fans,
            restored: false,
        }
    }

    pub fn fans_mut(&mut self) -> &mut [Fan] {
        self.restored = false;
        self.fans
    }

    pub fn restore(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        for fan in self.fans.iter() {
            if let Err(error) = fan.restore_original() {
                errors.push(format!("{}: {error:#}", fan.name));
            }
        }
        if errors.is_empty() {
            self.restored = true;
            Ok(())
        } else {
            anyhow::bail!("failed to restore fan state: {}", errors.join("; "))
        }
    }
}

impl Drop for FanControlGuard<'_> {
    fn drop(&mut self) {
        if !self.restored {
            if let Err(error) = self.restore() {
                tracing::error!(%error, "failed to restore fan state");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn guard_restores_pwm_and_enable_mode() {
        let directory = tempfile::tempdir().unwrap();
        let pwm = directory.path().join("pwm1");
        let enable = directory.path().join("pwm1_enable");
        fs::write(&pwm, "73").unwrap();
        fs::write(&enable, "2").unwrap();

        let mut fan = Fan::from_path("test", directory.path(), 1).unwrap();
        {
            let mut guard = FanControlGuard::new(std::slice::from_mut(&mut fan));
            let fan = &mut guard.fans_mut()[0];
            fan.set_manual().unwrap();
            fan.write_pwm(201).unwrap();
            assert_eq!(fs::read_to_string(&pwm).unwrap(), "201");
        }

        assert_eq!(fs::read_to_string(&pwm).unwrap(), "73");
        assert_eq!(fs::read_to_string(&enable).unwrap(), "2");
    }

    #[test]
    fn guard_restores_every_fan_during_error() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for directory in [first.path(), second.path()] {
            fs::write(directory.join("pwm1"), "73").unwrap();
            fs::write(directory.join("pwm1_enable"), "5").unwrap();
        }

        let mut fans = vec![
            Fan::from_path("first", first.path(), 1).unwrap(),
            Fan::from_path("second", second.path(), 1).unwrap(),
        ];
        let result = (|| -> Result<()> {
            let mut guard = FanControlGuard::new(&mut fans);
            for fan in guard.fans_mut() {
                fan.set_manual().unwrap();
                fan.write_pwm(200).unwrap();
            }
            anyhow::bail!("injected failure")
        })();

        assert!(result.is_err());
        for directory in [first.path(), second.path()] {
            assert_eq!(fs::read_to_string(directory.join("pwm1")).unwrap(), "73");
            assert_eq!(
                fs::read_to_string(directory.join("pwm1_enable")).unwrap(),
                "5"
            );
        }
    }
}
