//! NVIDIA (NVML) fan policy: config types and the pure curve logic, kept free
//! of NVML so it is testable without a GPU.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvidiaGpu {
    /// PCI bus id ("0000:01:00.0") or NVML device index ("0").
    pub device: String,
    /// Degrees C a falling temperature must drop below the held value
    /// before the curve follows it down.
    #[serde(default)]
    pub hysteresis: u32,
    pub fans: Vec<NvidiaFan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvidiaFan {
    pub index: u32,
    /// Hold this fan at the firmware minimum.
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub curve: Vec<NvidiaPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvidiaPoint {
    pub temp: u32,
    pub percent: u32,
}

pub fn validate(gpus: &[NvidiaGpu]) -> Result<()> {
    let mut devices = HashSet::new();
    for gpu in gpus {
        ensure!(
            devices.insert(gpu.device.as_str()),
            "duplicate nvidia device '{}'",
            gpu.device
        );
        ensure!(
            !gpu.fans.is_empty(),
            "nvidia device '{}' needs at least one fan",
            gpu.device
        );
        let mut indexes = HashSet::new();
        for fan in &gpu.fans {
            ensure!(
                indexes.insert(fan.index),
                "nvidia device '{}' lists fan {} twice",
                gpu.device,
                fan.index
            );
            if fan.disabled {
                continue;
            }
            ensure!(
                fan.curve.len() >= 2,
                "nvidia device '{}' fan {} curve needs at least two points",
                gpu.device,
                fan.index
            );
            for w in fan.curve.windows(2) {
                ensure!(
                    w[0].temp < w[1].temp && w[0].percent <= w[1].percent,
                    "nvidia device '{}' fan {} curve must have ascending temps and non-decreasing percents",
                    gpu.device,
                    fan.index
                );
            }
            ensure!(
                fan.curve.iter().all(|p| p.temp <= 150 && p.percent <= 100),
                "nvidia device '{}' fan {} curve is outside 0..=150C / 0..=100%",
                gpu.device,
                fan.index
            );
        }
    }
    Ok(())
}

/// Startup check against what the driver reports.
pub fn check_hardware(gpu: &NvidiaGpu, num_fans: u32) -> Result<()> {
    for fan in &gpu.fans {
        ensure!(
            fan.index < num_fans,
            "nvidia device '{}' fan {} does not exist (num_fans = {num_fans})",
            gpu.device,
            fan.index
        );
        ensure!(
            !(fan.disabled && num_fans == 1),
            "nvidia device '{}' reports one fan controller: fans are ganged, so per-fan off is impossible in software; unplug the fan header instead",
            gpu.device
        );
    }
    Ok(())
}

pub fn percent_at(curve: &[NvidiaPoint], temp: f64) -> u32 {
    let (Some(first), Some(last)) = (curve.first(), curve.last()) else {
        return 0;
    };
    if temp <= first.temp as f64 {
        return first.percent;
    }
    if temp >= last.temp as f64 {
        return last.percent;
    }
    for w in curve.windows(2) {
        let (lo, hi) = (&w[0], &w[1]);
        if temp <= hi.temp as f64 {
            let r = (temp - lo.temp as f64) / (hi.temp - lo.temp) as f64;
            return (lo.percent as f64 + r * (hi.percent as f64 - lo.percent as f64)).round()
                as u32;
        }
    }
    last.percent
}

/// One tick: (fan index, percent) to command. `held` carries the hysteresis
/// state. `None` temperature is the fail-safe: every enabled fan to maximum.
/// Disabled fans always get `min`; if `min > 0` the firmware forbids a full stop.
pub fn plan(
    gpu: &NvidiaGpu,
    held: &mut Option<f64>,
    temp: Option<f64>,
    (min, max): (u32, u32),
) -> Vec<(u32, u32)> {
    let eff = temp.map(|t| match *held {
        Some(h) if t < h && h - t <= gpu.hysteresis as f64 => h,
        _ => t,
    });
    *held = eff;
    gpu.fans
        .iter()
        .map(|f| {
            let pct = match (f.disabled, eff) {
                (true, _) => min,
                (false, None) => max,
                (false, Some(t)) => percent_at(&f.curve, t),
            };
            (f.index, pct.clamp(min, max))
        })
        .collect()
}
