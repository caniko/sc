use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

pub type WireCurve = (String, [u8; 8], [u8; 8], bool);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub profiles: Vec<ProfileConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub name: String,
    pub curves: Vec<CurveConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurveConfig {
    pub fan: String,
    pub temp: Vec<u32>,
    pub pwm: Vec<u32>,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    Balanced,
    Performance,
    Quiet,
    LowPower,
    Custom,
}

impl Profile {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "balanced" => Ok(Self::Balanced),
            "performance" => Ok(Self::Performance),
            "quiet" => Ok(Self::Quiet),
            "lowpower" | "low-power" | "low_power" => Ok(Self::LowPower),
            "custom" => Ok(Self::Custom),
            _ => anyhow::bail!("unknown asusd profile '{value}'"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Performance => "performance",
            Self::Quiet => "quiet",
            Self::LowPower => "low_power",
            Self::Custom => "custom",
        }
    }

    pub fn id(self) -> u32 {
        match self {
            Self::Balanced => 0,
            Self::Performance => 1,
            Self::Quiet => 2,
            Self::LowPower => 3,
            Self::Custom => 4,
        }
    }

    pub fn from_id(value: u32) -> Result<Self> {
        match value {
            0 => Ok(Self::Balanced),
            1 => Ok(Self::Performance),
            2 => Ok(Self::Quiet),
            3 => Ok(Self::LowPower),
            4 => Ok(Self::Custom),
            _ => anyhow::bail!("asusd returned unknown platform profile {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fan {
    Cpu,
    Gpu,
    Mid,
}

impl Fan {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "cpu" => Ok(Self::Cpu),
            "gpu" => Ok(Self::Gpu),
            "mid" => Ok(Self::Mid),
            _ => anyhow::bail!("unknown asusd fan '{value}'"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Gpu => "GPU",
            Self::Mid => "MID",
        }
    }
}

impl CurveConfig {
    pub fn wire(&self) -> Result<WireCurve> {
        let fan = Fan::parse(&self.fan)?;
        let temp = self
            .temp
            .iter()
            .copied()
            .map(u8::try_from)
            .collect::<std::result::Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| anyhow::anyhow!("fan '{}' must have exactly 8 temperatures", self.fan))?;
        let pwm = self
            .pwm
            .iter()
            .copied()
            .map(u8::try_from)
            .collect::<std::result::Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| anyhow::anyhow!("fan '{}' must have exactly 8 PWM values", self.fan))?;
        Ok((fan.name().to_owned(), pwm, temp, self.enabled))
    }
}

pub fn load(path: &Path) -> Result<Config> {
    let config = super::config::load_pkl(path)
        .with_context(|| format!("failed to evaluate asusd Pkl config: {}", path.display()))?;
    validate(&config)?;
    Ok(config)
}

pub fn validate(config: &Config) -> Result<()> {
    anyhow::ensure!(
        !config.profiles.is_empty(),
        "at least one asusd profile is required"
    );

    let mut profiles = HashSet::new();
    for profile in &config.profiles {
        let profile_id = Profile::parse(&profile.name)?;
        anyhow::ensure!(
            profiles.insert(profile_id),
            "duplicate asusd profile '{}'",
            profile.name
        );
        anyhow::ensure!(
            !profile.curves.is_empty(),
            "asusd profile '{}' must contain at least one fan curve",
            profile.name
        );

        let mut fans = HashSet::new();
        for curve in &profile.curves {
            let fan = Fan::parse(&curve.fan)?;
            anyhow::ensure!(
                fans.insert(fan),
                "duplicate fan '{}' in profile '{}'",
                curve.fan,
                profile.name
            );
            anyhow::ensure!(
                curve.temp.len() == 8,
                "fan '{}' in profile '{}' must have exactly 8 temperatures",
                curve.fan,
                profile.name
            );
            anyhow::ensure!(
                curve.pwm.len() == 8,
                "fan '{}' in profile '{}' must have exactly 8 PWM values",
                curve.fan,
                profile.name
            );
            anyhow::ensure!(
                curve.temp.iter().all(|value| *value <= 100),
                "fan '{}' in profile '{}' temperatures must be <= 100",
                curve.fan,
                profile.name
            );
            anyhow::ensure!(
                curve.pwm.iter().all(|value| *value <= 255),
                "fan '{}' in profile '{}' PWM values must be <= 255",
                curve.fan,
                profile.name
            );
            anyhow::ensure!(
                curve.temp.windows(2).all(|pair| pair[0] <= pair[1]),
                "fan '{}' in profile '{}' temperatures must be non-decreasing",
                curve.fan,
                profile.name
            );
            anyhow::ensure!(
                curve.pwm.windows(2).all(|pair| pair[0] <= pair[1]),
                "fan '{}' in profile '{}' PWM values must be non-decreasing",
                curve.fan,
                profile.name
            );
        }
    }
    Ok(())
}

fn enabled_by_default() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            profiles: vec![ProfileConfig {
                name: "Balanced".into(),
                curves: vec![CurveConfig {
                    fan: "cpu".into(),
                    temp: vec![30, 40, 50, 60, 70, 80, 90, 100],
                    pwm: vec![0, 10, 30, 60, 100, 150, 210, 255],
                    enabled: true,
                }],
            }],
        }
    }

    #[test]
    fn accepts_normalized_names_and_preserves_raw_pwm() {
        let config = test_config();
        validate(&config).unwrap();
        assert_eq!(Profile::parse("LOW-power").unwrap(), Profile::LowPower);
        assert_eq!(config.profiles[0].curves[0].wire().unwrap().1[7], 255);
        assert!(Profile::parse("qu!iet").is_err());
    }

    #[test]
    fn rejects_empty_and_duplicate_profiles() {
        assert!(validate(&Config { profiles: vec![] }).is_err());
        let mut config = test_config();
        config.profiles[0].curves.clear();
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("at least one fan curve"));

        let mut config = test_config();
        config.profiles.push(config.profiles[0].clone());
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
    }

    #[test]
    fn rejects_unknown_and_duplicate_fans() {
        let mut config = test_config();
        config.profiles[0].curves[0].fan = "chassis".into();
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("unknown"));

        let mut config = test_config();
        let duplicate = config.profiles[0].curves[0].clone();
        config.profiles[0].curves.push(duplicate);
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
    }

    #[test]
    fn rejects_wrong_lengths_and_unsafe_values() {
        let mut config = test_config();
        config.profiles[0].curves[0].temp.pop();
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("exactly 8"));

        let mut config = test_config();
        config.profiles[0].curves[0].temp[7] = 101;
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("<= 100"));

        let mut config = test_config();
        config.profiles[0].curves[0].pwm[7] = 256;
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("<= 255"));
    }

    #[test]
    fn rejects_decreasing_curves() {
        let mut config = test_config();
        config.profiles[0].curves[0].temp[1] = 20;
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("temperatures must be non-decreasing"));

        let mut config = test_config();
        config.profiles[0].curves[0].pwm[1] = 20;
        config.profiles[0].curves[0].pwm[2] = 10;
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("PWM values must be non-decreasing"));
    }

    #[test]
    fn loads_pkl_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asusd.pkl");
        std::fs::write(
            &path,
            r#"
profiles = new Listing {
  new {
    name = "quiet"
    curves = new Listing {
      new {
        fan = "CPU"
        temp = new Listing { 30; 40; 50; 60; 70; 80; 90; 100 }
        pwm = new Listing { 0; 10; 30; 60; 100; 150; 210; 255 }
        enabled = true
      }
    }
  }
}
"#,
        )
        .unwrap();

        let config = load(&path).unwrap();
        assert_eq!(config.profiles[0].curves[0].wire().unwrap().0, "CPU");
    }
}
