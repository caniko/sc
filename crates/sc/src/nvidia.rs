use anyhow::{Context, Result};
use clap::Subcommand;
use nvml_wrapper::{enum_wrappers::device::TemperatureSensor, Nvml};
use sc_core::nvidia::{NvidiaFan, NvidiaGpu, NvidiaPoint};
use sc_hwmon::nvidia::{open, GpuController};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Subcommand)]
pub enum Command {
    /// Read-only: per GPU name, driver, temperature, power and fan state
    Status,
    /// Spin one fan to 100% with the others at the minimum, then restore defaults
    Identify {
        /// PCI bus id (0000:01:00.0) or NVML device index
        #[arg(long)]
        device: String,
        /// Fan index to spin
        #[arg(long)]
        fan: u32,
        #[arg(long, default_value = "15")]
        seconds: u64,
    },
}

pub fn run(command: Command) -> Result<()> {
    let nvml = Nvml::init()
        .context("failed to initialize NVML (is libnvidia-ml.so.1 on the library path?)")?;
    match command {
        Command::Status => status(&nvml),
        Command::Identify {
            device,
            fan,
            seconds,
        } => identify(&nvml, &device, fan, seconds),
    }
}

fn status(nvml: &Nvml) -> Result<()> {
    let driver = nvml.sys_driver_version().unwrap_or_default();
    for index in 0..nvml.device_count()? {
        let dev = open(nvml, &index.to_string())?;
        println!(
            "[{index}] {} driver={driver} temp={}C power={:.0}W",
            dev.name().unwrap_or_default(),
            dev.temperature(TemperatureSensor::Gpu)
                .map_or("?".into(), |t| t.to_string()),
            dev.power_usage().unwrap_or(0) as f64 / 1000.0,
        );
        let num = dev.num_fans().unwrap_or(0);
        let (min, max) = dev.min_max_fan_speed().unwrap_or((0, 0));
        println!("    num_fans={num} min={min}% max={max}%");
        for fan in 0..num {
            // NVML has no target readback; the policy tells whether we or the driver drive the fan.
            println!(
                "    fan {fan}: speed={}% policy={}",
                dev.fan_speed(fan).map_or("?".into(), |s| s.to_string()),
                dev.fan_control_policy(fan)
                    .map_or("?".into(), |p| format!("{p:?}")),
            );
        }
    }
    Ok(())
}

fn identify(nvml: &Nvml, device: &str, fan: u32, seconds: u64) -> Result<()> {
    anyhow::ensure!(seconds > 0, "seconds must be greater than zero");
    let num = open(nvml, device)?.num_fans()?;
    anyhow::ensure!(fan < num, "fan {fan} does not exist (num_fans = {num})");
    // Reuse the daemon's controller: target fan on a flat 100% curve, others held at the minimum.
    let flat = [(0, 100), (1, 100)].map(|(temp, percent)| NvidiaPoint { temp, percent });
    let cfg = NvidiaGpu {
        device: device.into(),
        hysteresis: 0,
        fans: (0..num)
            .map(|index| NvidiaFan {
                index,
                disabled: index != fan,
                curve: if index == fan { flat.to_vec() } else { vec![] },
            })
            .collect(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(sig, Arc::clone(&stop))?;
    }
    // Dropping the controller restores automatic control, including on Ctrl-C (flag, not abort).
    let mut gpu = GpuController::new(nvml, cfg)?;
    println!("fan {fan} at 100%, others at minimum for {seconds}s (Ctrl-C to stop)");
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end && !stop.load(Ordering::Relaxed) {
        gpu.tick();
        std::thread::sleep(Duration::from_millis(500));
    }
    drop(gpu);
    println!("restored automatic fan control");
    Ok(())
}
