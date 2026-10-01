//! Read-only hwmon observations, including firmware fans without manual PWM.
use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize)]
pub struct Snapshot {
    pub fans: Vec<FanReading>,
    pub temperatures: Vec<TemperatureReading>,
    pub curves: Vec<FirmwareCurve>,
}

#[derive(Debug, Serialize)]
pub struct FanReading {
    pub chip: String,
    pub path: PathBuf,
    pub index: u32,
    pub label: Option<String>,
    pub rpm: Option<u32>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TemperatureReading {
    pub chip: String,
    pub path: PathBuf,
    pub index: u32,
    pub label: Option<String>,
    pub temp_c: Option<f64>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FirmwareCurve {
    pub path: PathBuf,
    pub index: u32,
    pub temp: Option<[u8; 8]>,
    pub pwm: Option<[u8; 8]>,
    pub enabled: Option<bool>,
    pub error: Option<String>,
}

/// Enumerate inputs afresh so hwmon renumbering never reuses stale paths.
/// GPU inputs are read only when runtime PM reports the device active.
pub fn read_snapshot(base: &Path) -> Result<Snapshot> {
    let mut snapshot = Snapshot::default();
    let mut chips = fs::read_dir(base)
        .with_context(|| format!("enumerate {}", base.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    chips.sort_by_key(|entry| entry.path());
    for entry in chips {
        let path = entry.path();
        let Some(chip) = text(&path.join("name")) else {
            continue;
        };
        let blocked = if matches!(chip.as_str(), "amdgpu" | "nvidia" | "nouveau") {
            match text(&path.join("device/power/runtime_status")) {
                Some(state) if state == "active" => None,
                Some(state) => Some(format!("device runtime-{state}")),
                None => Some("GPU runtime state unavailable".to_owned()),
            }
        } else {
            None
        };
        let mut files = fs::read_dir(&path)?.collect::<std::io::Result<Vec<_>>>()?;
        files.sort_by_key(|entry| entry.file_name());
        for file in files {
            let name = file.file_name().to_string_lossy().into_owned();
            if let Some(index) = input_index(&name, "fan", "_input") {
                let (rpm, error) = reading::<u32>(&file.path(), blocked.as_deref());
                snapshot.fans.push(FanReading {
                    chip: chip.clone(),
                    path: file.path(),
                    index,
                    label: text(&path.join(format!("fan{index}_label"))),
                    rpm,
                    error,
                });
            } else if let Some(index) = input_index(&name, "temp", "_input") {
                let (value, mut error) = reading::<i64>(&file.path(), blocked.as_deref());
                let temp_c = value
                    .filter(|value| {
                        let valid = (-20_000..=150_000).contains(value);
                        if !valid {
                            error = Some("temperature outside -20..150 C".to_owned());
                        }
                        valid
                    })
                    .map(|value| value as f64 / 1000.0);
                snapshot.temperatures.push(TemperatureReading {
                    chip: chip.clone(),
                    path: file.path(),
                    index,
                    label: text(&path.join(format!("temp{index}_label"))),
                    temp_c,
                    error,
                });
            } else if chip == "asus_custom_fan_curve" {
                if let Some(index) = input_index(&name, "pwm", "_enable") {
                    snapshot.curves.push(read_curve(&path, index));
                }
            }
        }
    }
    Ok(snapshot)
}

fn input_index(name: &str, prefix: &str, suffix: &str) -> Option<u32> {
    name.strip_prefix(prefix)?
        .strip_suffix(suffix)?
        .parse()
        .ok()
}

fn text(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
}

fn reading<T: std::str::FromStr>(path: &Path, blocked: Option<&str>) -> (Option<T>, Option<String>)
where
    T::Err: std::fmt::Display,
{
    if let Some(reason) = blocked {
        return (None, Some(reason.to_owned()));
    }
    match crate::read_sysfs(path) {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error.to_string())),
    }
}

fn read_curve(path: &Path, index: u32) -> FirmwareCurve {
    let points = |kind: &str| -> Result<[u8; 8]> {
        let mut values = [0; 8];
        for (point, value) in values.iter_mut().enumerate() {
            *value = crate::read_sysfs(
                &path.join(format!("pwm{index}_auto_point{}_{kind}", point + 1)),
            )?;
        }
        Ok(values)
    };
    let temp = points("temp");
    let pwm = points("pwm");
    let enabled =
        crate::read_sysfs::<u8>(&path.join(format!("pwm{index}_enable"))).and_then(|mode| {
            anyhow::ensure!(
                matches!(mode, 1 | 2),
                "unknown ASUS curve enable mode {mode}"
            );
            Ok(mode == 1)
        });
    let errors = [
        temp.as_ref().err(),
        pwm.as_ref().err(),
        enabled.as_ref().err(),
    ]
    .into_iter()
    .flatten()
    .map(ToString::to_string)
    .collect::<Vec<_>>();
    FirmwareCurve {
        path: path.to_path_buf(),
        index,
        temp: temp.ok(),
        pwm: pwm.ok(),
        enabled: enabled.ok(),
        error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn reads_firmware_fan_rpm_without_a_pwm_control() {
        let root = tempfile::tempdir().unwrap();
        let chip = root.path().join("hwmon37");
        fs::create_dir(&chip).unwrap();
        fs::write(chip.join("name"), "asus\n").unwrap();
        fs::write(chip.join("fan1_label"), "cpu_fan\n").unwrap();
        fs::write(chip.join("fan1_input"), "2200\n").unwrap();

        let snapshot = read_snapshot(root.path()).unwrap();
        assert_eq!(snapshot.fans.len(), 1);
        assert_eq!(snapshot.fans[0].rpm, Some(2200));
        assert_eq!(snapshot.fans[0].label.as_deref(), Some("cpu_fan"));
        assert!(!chip.join("pwm1").exists());
    }

    #[test]
    fn invalid_rpm_is_unknown_rather_than_stopped() {
        let root = tempfile::tempdir().unwrap();
        let chip = root.path().join("hwmon1");
        fs::create_dir(&chip).unwrap();
        fs::write(chip.join("name"), "asus").unwrap();
        fs::write(chip.join("fan1_input"), "invalid").unwrap();
        let snapshot = read_snapshot(root.path()).unwrap();
        assert_eq!(snapshot.fans[0].rpm, None);
        assert!(snapshot.fans[0].error.is_some());
    }

    #[test]
    fn suspended_gpu_readings_are_not_polled() {
        let root = tempfile::tempdir().unwrap();
        let chip = root.path().join("hwmon2");
        fs::create_dir_all(chip.join("device/power")).unwrap();
        fs::write(chip.join("name"), "amdgpu").unwrap();
        fs::write(chip.join("device/power/runtime_status"), "suspended").unwrap();
        fs::write(chip.join("temp1_input"), "invalid").unwrap();
        let snapshot = read_snapshot(root.path()).unwrap();
        assert_eq!(snapshot.temperatures[0].temp_c, None);
        assert_eq!(
            snapshot.temperatures[0].error.as_deref(),
            Some("device runtime-suspended")
        );
    }

    #[test]
    fn reads_active_firmware_zero_plateau_and_enable_state() {
        let root = tempfile::tempdir().unwrap();
        let chip = root.path().join("hwmon8");
        fs::create_dir(&chip).unwrap();
        fs::write(chip.join("name"), "asus_custom_fan_curve").unwrap();
        fs::write(chip.join("pwm1_enable"), "1").unwrap();
        let temperatures = [40, 50, 55, 60, 65, 75, 85, 90];
        let pwm = [0, 0, 0, 40, 70, 120, 200, 255];
        for point in 1..=8 {
            fs::write(
                chip.join(format!("pwm1_auto_point{point}_temp")),
                temperatures[point - 1].to_string(),
            )
            .unwrap();
            fs::write(
                chip.join(format!("pwm1_auto_point{point}_pwm")),
                pwm[point - 1].to_string(),
            )
            .unwrap();
        }
        let snapshot = read_snapshot(root.path()).unwrap();
        assert_eq!(snapshot.curves[0].temp, Some(temperatures));
        assert_eq!(snapshot.curves[0].pwm, Some(pwm));
        assert_eq!(snapshot.curves[0].enabled, Some(true));
        fs::remove_file(chip.join("pwm1_auto_point1_pwm")).unwrap();
        assert_eq!(read_snapshot(root.path()).unwrap().curves[0].pwm, None);
    }
}
