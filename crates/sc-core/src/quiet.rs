use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Envelope {
    pub name: String,
    pub cpu_pwm_ceiling: u8,
    pub gpu_pwm_ceiling: u8,
    pub cpu_ppt: [i32; 3],
    pub gpu_clock_mhz: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// Source-bound qualification identifier and exact firmware/device identity.
    pub qualification_id: String,
    pub hardware: HardwareIdentity,
    pub profiles: Vec<Envelope>,
    pub gpu_pci_id: String,
    pub energy_path: String,
    pub cpu_soft_c: f64,
    pub gpu_soft_c: f64,
    pub cpu_emergency_c: f64,
    pub gpu_emergency_c: f64,
    pub recovery_hysteresis_c: f64,
    pub recovery_seconds: u64,
    pub gpu_reduced_clock_mhz: u32,
    pub cpu_start_pwm: u8,
    pub gpu_start_pwm: u8,
    /// Last curve segment may escape the normal ceiling only in this band.
    pub cpu_emergency_band_c: f64,
    pub gpu_emergency_band_c: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HardwareIdentity {
    pub product: String,
    pub bios: String,
    pub board: String,
    pub kernel: String,
    pub nvidia_driver: String,
    pub gpu_devices: Vec<String>,
}

pub fn load(path: &Path) -> Result<Config> {
    let config: Config = crate::config::load_pkl(path)?;
    config.validate()?;
    Ok(config)
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.qualification_id.trim().is_empty()
                && !self.hardware.product.trim().is_empty()
                && !self.hardware.bios.trim().is_empty()
                && !self.hardware.board.trim().is_empty(),
            "qualified hardware/firmware identity required"
        );
        validate_pci_id(&self.gpu_pci_id)?;
        ensure!(
            self.profiles.len() == 3,
            "quiet policy requires Quiet, Balanced and Performance envelopes"
        );
        let mut names = std::collections::HashSet::new();
        for profile in &self.profiles {
            ensure!(
                matches!(profile.name.as_str(), "quiet" | "balanced" | "performance")
                    && names.insert(&profile.name),
                "unknown/duplicate quiet profile"
            );
            ensure!(
                (1..=200).contains(&profile.cpu_pwm_ceiling)
                    && (1..=200).contains(&profile.gpu_pwm_ceiling),
                "normal acoustic ceiling must be 1..200"
            );
            ensure!(
                profile.cpu_ppt[0] >= 15
                    && profile.cpu_ppt[2] <= 80
                    && profile.cpu_ppt.windows(2).all(|p| p[0] <= p[1]),
                "invalid CPU power envelope"
            );
            ensure!(
                (210..=1800).contains(&profile.gpu_clock_mhz)
                    && profile.gpu_clock_mhz >= self.gpu_reduced_clock_mhz,
                "invalid GPU clock envelope"
            );
        }
        ensure!(
            self.profiles
                .iter()
                .all(|p| p.cpu_pwm_ceiling == self.profiles[0].cpu_pwm_ceiling
                    && p.gpu_pwm_ceiling == self.profiles[0].gpu_pwm_ceiling),
            "all profiles must share the learned acoustic ceiling"
        );
        ensure!(
            (210..=1200).contains(&self.gpu_reduced_clock_mhz),
            "invalid reduced GPU clock"
        );
        ensure!(
            (1..=200).contains(&self.cpu_start_pwm) && (1..=200).contains(&self.gpu_start_pwm),
            "qualified fan-start PWM required"
        );
        ensure!(
            self.profiles
                .iter()
                .all(|p| self.cpu_start_pwm <= p.cpu_pwm_ceiling
                    && self.gpu_start_pwm <= p.gpu_pwm_ceiling),
            "quiet ceilings must permit qualified fan starts"
        );
        for value in [
            self.cpu_soft_c,
            self.gpu_soft_c,
            self.cpu_emergency_c,
            self.gpu_emergency_c,
            self.recovery_hysteresis_c,
            self.cpu_emergency_band_c,
            self.gpu_emergency_band_c,
        ] {
            ensure!(value.is_finite(), "nonfinite quiet policy");
        }
        ensure!(
            self.cpu_emergency_c.fract() == 0.0
                && self.gpu_emergency_c.fract() == 0.0
                && (2.0..=10.0).contains(&self.cpu_emergency_band_c)
                && (2.0..=5.0).contains(&self.gpu_emergency_band_c),
            "firmware emergency endpoints require integral degrees and narrow bands"
        );
        ensure!(
            self.cpu_soft_c >= 55.0
                && self.cpu_emergency_c <= 90.0
                && self.cpu_emergency_band_c >= 2.0
                && self.cpu_soft_c < self.cpu_emergency_c - self.cpu_emergency_band_c,
            "CPU power reduction must precede the conservative emergency band"
        );
        ensure!(
            self.gpu_soft_c >= 55.0
                && self.gpu_emergency_c <= 80.0
                && self.gpu_emergency_band_c >= 2.0
                && self.gpu_soft_c < self.gpu_emergency_c - self.gpu_emergency_band_c,
            "GPU power reduction must precede the conservative emergency band"
        );
        ensure!(
            (2.0..=10.0).contains(&self.recovery_hysteresis_c)
                && (10..=300).contains(&self.recovery_seconds),
            "invalid recovery hysteresis/dwell"
        );
        Ok(())
    }
}

pub fn validate_pci_id(id: &str) -> Result<()> {
    let bytes = id.as_bytes();
    ensure!(
        bytes.len() == 12
            && bytes[4] == b':'
            && bytes[7] == b':'
            && bytes[10] == b'.'
            && bytes
                .iter()
                .enumerate()
                .all(|(i, b)| matches!(i, 4 | 7 | 10) || b.is_ascii_hexdigit())
            && (b'0'..=b'7').contains(&bytes[11]),
        "GPU must be a full PCI BDF (dddd:bb:ss.f)"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Normal,
    Limited,
    Emergency,
    TelemetryFault,
}

pub struct Controller {
    pub mode: Mode,
    cool_since: Option<f64>,
}

pub struct ThermalInput {
    pub now: f64,
    pub cpu: Option<f64>,
    pub gpu: Option<f64>,
    pub gpu_required: bool,
    pub cpu_rate: f64,
    pub gpu_rate: f64,
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            mode: Mode::Normal,
            cool_since: None,
        }
    }
}

impl Controller {
    /// Predictive reduction is confined to power; maximum fan requests require
    /// the actual emergency temperature or unavailable required telemetry.
    pub fn update(&mut self, config: &Config, input: ThermalInput) -> Mode {
        let ThermalInput {
            now,
            cpu,
            gpu,
            gpu_required,
            cpu_rate,
            gpu_rate,
        } = input;
        let valid = |t: f64| t.is_finite() && (0.0..=120.0).contains(&t);
        let cpu = cpu.filter(|t| valid(*t));
        let gpu = gpu.filter(|t| valid(*t));
        let next = match cpu {
            _ if !now.is_finite()
                || !cpu_rate.is_finite()
                || !gpu_rate.is_finite()
                || (gpu_required && gpu.is_none()) =>
            {
                Mode::TelemetryFault
            }
            None => Mode::TelemetryFault,
            Some(cpu)
                if cpu >= config.cpu_emergency_c
                    || gpu.is_some_and(|t| t >= config.gpu_emergency_c) =>
            {
                Mode::Emergency
            }
            Some(cpu)
                if cpu + cpu_rate.max(0.0) * 5.0 >= config.cpu_soft_c
                    || gpu.is_some_and(|t| t + gpu_rate.max(0.0) * 5.0 >= config.gpu_soft_c) =>
            {
                Mode::Limited
            }
            Some(_) => Mode::Normal,
        };
        // Do not bounce full fan requests at the emergency threshold. A real
        // cool margin is required before dropping back into the quiet envelope.
        let emergency_hot = cpu
            .is_some_and(|t| t >= config.cpu_emergency_c - config.recovery_hysteresis_c)
            || gpu.is_some_and(|t| t >= config.gpu_emergency_c - config.recovery_hysteresis_c);
        if self.mode == Mode::Emergency && emergency_hot && next != Mode::TelemetryFault {
            self.cool_since = None;
            return self.mode;
        }
        if next != Mode::Normal {
            self.mode = next;
            self.cool_since = None;
        } else if self.mode != Mode::Normal {
            let cool = cpu.unwrap() < config.cpu_soft_c - config.recovery_hysteresis_c
                && gpu.is_none_or(|t| t < config.gpu_soft_c - config.recovery_hysteresis_c);
            if cool {
                let since = *self.cool_since.get_or_insert(now);
                if now - since >= config.recovery_seconds as f64 {
                    self.mode = Mode::Normal;
                    self.cool_since = None;
                }
            } else {
                self.cool_since = None;
            }
            // Valid readings recover emergency/fault fan requests to Limited;
            // reduced power remains until the full cool dwell is satisfied.
            if next == Mode::Normal && self.mode != Mode::Normal {
                self.mode = Mode::Limited;
            }
        }
        self.mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(
        now: f64,
        cpu: Option<f64>,
        gpu: Option<f64>,
        gpu_required: bool,
        cpu_rate: f64,
        gpu_rate: f64,
    ) -> ThermalInput {
        ThermalInput {
            now,
            cpu,
            gpu,
            gpu_required,
            cpu_rate,
            gpu_rate,
        }
    }
    fn config() -> Config {
        Config {
            qualification_id: "fixture".into(),
            hardware: HardwareIdentity {
                product: "fixture".into(),
                bios: "1".into(),
                board: "fixture".into(),
                kernel: "fixture".into(),
                nvidia_driver: "fixture".into(),
                gpu_devices: vec!["fixture".into()],
            },
            profiles: ["quiet", "balanced", "performance"]
                .into_iter()
                .map(|name| Envelope {
                    name: name.into(),
                    cpu_pwm_ceiling: 100,
                    gpu_pwm_ceiling: 100,
                    cpu_ppt: [15, 25, 35],
                    gpu_clock_mhz: 1200,
                })
                .collect(),
            gpu_pci_id: "0000:01:00.0".into(),
            energy_path: "/sys/class/powercap/intel-rapl:0".into(),
            cpu_soft_c: 80.0,
            gpu_soft_c: 72.0,
            cpu_emergency_c: 90.0,
            gpu_emergency_c: 80.0,
            recovery_hysteresis_c: 5.0,
            recovery_seconds: 30,
            gpu_reduced_clock_mhz: 600,
            cpu_start_pwm: 40,
            gpu_start_pwm: 40,
            cpu_emergency_band_c: 5.0,
            gpu_emergency_band_c: 2.0,
        }
    }
    #[test]
    fn temperature_prediction_reduces_power_without_requesting_emergency_fans() {
        let mut controller = Controller::default();
        assert_eq!(
            controller.update(&config(), sample(0.0, Some(76.0), None, false, 1.0, 0.0)),
            Mode::Limited
        );
        assert_eq!(
            controller.update(&config(), sample(1.0, Some(90.0), None, false, 0.0, 0.0)),
            Mode::Emergency
        );
    }
    #[test]
    fn suspended_gpu_is_distinct_from_a_required_missing_sensor() {
        let mut controller = Controller::default();
        assert_eq!(
            controller.update(&config(), sample(0.0, Some(50.0), None, false, 0.0, 0.0)),
            Mode::Normal
        );
        assert_eq!(
            controller.update(&config(), sample(1.0, Some(50.0), None, true, 0.0, 0.0)),
            Mode::TelemetryFault
        );
        assert_eq!(
            controller.update(
                &config(),
                sample(2.0, Some(50.0), Some(40.0), true, 0.0, 0.0)
            ),
            Mode::Limited
        );
        assert_eq!(
            controller.update(
                &config(),
                sample(33.0, Some(50.0), Some(40.0), true, 0.0, 0.0)
            ),
            Mode::Normal
        );
    }
    #[test]
    fn recovery_dwell_resets_when_heat_returns() {
        let mut controller = Controller::default();
        controller.update(&config(), sample(0.0, Some(82.0), None, false, 0.0, 0.0));
        controller.update(&config(), sample(1.0, Some(70.0), None, false, 0.0, 0.0));
        controller.update(&config(), sample(20.0, Some(79.0), None, false, 0.0, 0.0));
        controller.update(&config(), sample(21.0, Some(70.0), None, false, 0.0, 0.0));
        assert_eq!(
            controller.update(&config(), sample(40.0, Some(70.0), None, false, 0.0, 0.0)),
            Mode::Limited
        );
        assert_eq!(
            controller.update(&config(), sample(52.0, Some(70.0), None, false, 0.0, 0.0)),
            Mode::Normal
        );
    }
    #[test]
    fn emergency_requests_do_not_chatter_around_the_trip_temperature() {
        let mut controller = Controller::default();
        assert_eq!(
            controller.update(&config(), sample(0.0, Some(90.0), None, false, 0.0, 0.0)),
            Mode::Emergency
        );
        assert_eq!(
            controller.update(&config(), sample(1.0, Some(89.0), None, false, 0.0, 0.0)),
            Mode::Emergency
        );
        assert_eq!(
            controller.update(&config(), sample(2.0, Some(84.0), None, false, 0.0, 0.0)),
            Mode::Limited
        );
        assert_eq!(
            controller.update(
                &config(),
                sample(3.0, Some(50.0), None, false, f64::NAN, 0.0)
            ),
            Mode::TelemetryFault
        );
    }
    #[test]
    fn policy_rejects_profile_escape_and_invalid_device_paths() {
        let mut policy = config();
        assert!(policy.validate().is_ok());
        policy.profiles[2].cpu_pwm_ceiling = 190;
        assert!(policy.validate().is_err());
        policy = config();
        policy.gpu_pci_id = "../devices".into();
        assert!(policy.validate().is_err());
        policy = config();
        policy.cpu_soft_c = 89.0;
        assert!(policy.validate().is_err());
    }
}
