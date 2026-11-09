//! Heat-source power readings for the benchmark.
//!
//! Temperature alone cannot tell whether a fan matters for a CPU that boosts
//! until it reaches its temperature limit (Zen, recent Intel): a slower fan
//! leaves the temperature unchanged and lowers clocks instead. Package power
//! exposes that throttling.

use std::path::{Path, PathBuf};
use std::time::Instant;

pub enum PowerMeter {
    /// Cumulative energy counter in µJ (RAPL), wrapping at `max_uj`.
    Energy { path: PathBuf, max_uj: u64 },
    /// Firmware-averaged power in µW (hwmon `power1_average`/`power1_input`).
    Average { path: PathBuf },
}

/// Reading taken at the start of a sampling window.
pub struct PowerStart {
    at: Instant,
    value: u64,
}

impl PowerMeter {
    /// CPU package energy via powercap (root-only on most kernels).
    pub fn cpu() -> Option<Self> {
        let zones = std::fs::read_dir("/sys/class/powercap").ok()?;
        let mut zones: Vec<PathBuf> = zones.filter_map(|e| e.ok().map(|e| e.path())).collect();
        zones.sort();
        zones.into_iter().find_map(|zone| {
            let name = std::fs::read_to_string(zone.join("name")).ok()?;
            if !name.trim().starts_with("package") {
                return None;
            }
            let path = zone.join("energy_uj");
            read_u64(&path)?;
            let max_uj = read_u64(&zone.join("max_energy_range_uj")).unwrap_or(u64::MAX);
            Some(Self::Energy { path, max_uj })
        })
    }

    /// GPU board power from the GPU's hwmon directory.
    pub fn hwmon(dir: &Path) -> Option<Self> {
        ["power1_average", "power1_input"]
            .iter()
            .map(|f| dir.join(f))
            .find(|p| read_u64(p).is_some())
            .map(|path| Self::Average { path })
    }

    pub fn label(&self) -> String {
        match self {
            Self::Energy { path, .. } | Self::Average { path } => path.display().to_string(),
        }
    }

    pub fn begin(&self) -> Option<PowerStart> {
        let path = match self {
            Self::Energy { path, .. } | Self::Average { path } => path,
        };
        Some(PowerStart {
            at: Instant::now(),
            value: read_u64(path)?,
        })
    }

    /// Mean power in watts since `start`.
    pub fn end(&self, start: PowerStart) -> Option<f64> {
        match self {
            Self::Energy { path, max_uj } => {
                let now = read_u64(path)?;
                let secs = start.at.elapsed().as_secs_f64();
                (secs > 0.0).then(|| energy_delta_uj(start.value, now, *max_uj) as f64 / 1e6 / secs)
            }
            Self::Average { path } => {
                let now = read_u64(path)?;
                Some((start.value + now) as f64 / 2.0 / 1e6)
            }
        }
    }
}

fn energy_delta_uj(start: u64, end: u64, max_uj: u64) -> u64 {
    if end >= start {
        end - start
    } else {
        // Counter wrapped once within the window.
        max_uj - start + end
    }
}

fn read_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn energy_counter_wraps() {
        assert_eq!(energy_delta_uj(100, 350, 1000), 250);
        assert_eq!(energy_delta_uj(900, 50, 1000), 150);
    }
}
