//! NVML fan control. Takes manual control on the first write and hands every
//! managed fan back to the driver on drop (shutdown, error and panic paths).
use anyhow::{Context, Result};
use nvml_wrapper::{enum_wrappers::device::TemperatureSensor, Device, Nvml};
use sc_core::nvidia::{check_hardware, plan, NvidiaGpu};

/// Resolve a PCI bus id ("0000:01:00.0") or a device index ("0").
pub fn open<'a>(nvml: &'a Nvml, device: &str) -> Result<Device<'a>> {
    match device.parse::<u32>() {
        Ok(index) => nvml.device_by_index(index),
        Err(_) => nvml.device_by_pci_bus_id(device),
    }
    .with_context(|| format!("failed to open nvidia device '{device}'"))
}

pub struct GpuController<'a> {
    dev: Device<'a>,
    cfg: NvidiaGpu,
    held: Option<f64>,
    range: (u32, u32),
    warned: bool,
}

impl<'a> GpuController<'a> {
    pub fn new(nvml: &'a Nvml, cfg: NvidiaGpu) -> Result<Self> {
        let dev = open(nvml, &cfg.device)?;
        check_hardware(&cfg, dev.num_fans().context("read num_fans")?)?;
        let range = dev.min_max_fan_speed().context("read fan speed range")?;
        Ok(Self {
            dev,
            cfg,
            held: None,
            range,
            warned: false,
        })
    }

    /// Normalised PCI bus id as reported by NVML, so an index and its bus id compare equal.
    pub fn bus_id(&self) -> Result<String> {
        Ok(self.dev.pci_info().context("read PCI info")?.bus_id)
    }

    /// Daemon step: write errors are logged, not fatal; a lost device then reads
    /// as a failed temperature and drives the 100% fail-safe.
    pub fn tick(&mut self) {
        if let Err(e) = self.try_tick() {
            tracing::error!(device = %self.cfg.device, error = %e, "fan control step failed");
        }
    }

    /// One control step that reports the first fan write failure (all fans are still attempted).
    pub fn try_tick(&mut self) -> Result<()> {
        let temp = match self.dev.temperature(TemperatureSensor::Gpu) {
            Ok(t) => Some(t as f64),
            Err(e) => {
                tracing::error!(device = %self.cfg.device, error = %e, "GPU temperature unavailable; commanding fail-safe");
                None
            }
        };
        if self.range.0 > 0 && !self.warned && self.cfg.fans.iter().any(|f| f.disabled) {
            self.warned = true;
            tracing::warn!(
                device = %self.cfg.device,
                min = self.range.0,
                "firmware forbids a full stop; disabled fans are held at the minimum"
            );
        }
        let mut first = None;
        for (index, pct) in plan(&self.cfg, &mut self.held, temp, self.range) {
            if let Err(e) = self.dev.set_fan_speed(index, pct) {
                tracing::error!(device = %self.cfg.device, fan = index, pct, error = %e, "failed to set fan speed");
                first.get_or_insert(anyhow::anyhow!("fan {index} set to {pct}%: {e}"));
            }
        }
        first.map_or(Ok(()), Err)
    }

    pub fn restore(&mut self) {
        for fan in &self.cfg.fans {
            if let Err(e) = self.dev.set_default_fan_speed(fan.index) {
                tracing::error!(device = %self.cfg.device, fan = fan.index, error = %e, "failed to restore automatic fan control");
            }
        }
    }
}

impl Drop for GpuController<'_> {
    fn drop(&mut self) {
        // Idempotent: restoring twice is harmless; never skipped on unwind.
        self.restore();
    }
}
