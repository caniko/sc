//! NVML fan sweep: hold the healthy fans at each step, record the settled GPU
//! temperature, and suggest a curve. Review-first: dry run unless `--apply`.
use crate::interrupt::InterruptGuard;
use anyhow::{Context, Result};
use nvml_wrapper::{enum_wrappers::device::TemperatureSensor, Nvml};
use sc_core::nvidia::{NvidiaFan, NvidiaGpu, NvidiaPoint};
use sc_hwmon::nvidia::{open, GpuController};
use std::time::Duration;

pub struct Args {
    pub device: String,
    pub disabled: Vec<u32>,
    pub target_c: u32,
    pub hold_secs: u64,
    pub apply: bool,
}

const STEPS: [u32; 8] = [30, 40, 50, 60, 70, 80, 90, 100];

pub fn run(a: Args) -> Result<String> {
    let nvml = Nvml::init().context("failed to initialize NVML")?;
    let dev = open(&nvml, &a.device)?;
    let num = dev.num_fans()?;
    let (min, max) = dev.min_max_fan_speed()?;
    let healthy: Vec<u32> = (0..num).filter(|i| !a.disabled.contains(i)).collect();
    anyhow::ensure!(
        !healthy.is_empty(),
        "every fan is disabled; nothing to sweep"
    );
    let steps: Vec<u32> = STEPS.iter().map(|s| (*s).clamp(min, max)).collect();
    eprintln!(
        "{}: {num} fan(s), healthy {healthy:?}, disabled {:?} held at {min}%; sweep {steps:?} for {}s each",
        a.device, a.disabled, a.hold_secs
    );
    if !a.apply {
        eprintln!(
            "Dry run: no fan writes. Start your GPU load, then re-run with --apply (state is restored on exit)."
        );
        return Ok(String::new());
    }

    let interrupt = InterruptGuard::new()?;
    let mut measured = Vec::new();
    for pct in steps {
        let flat = [0, 1].map(|temp| NvidiaPoint { temp, percent: pct });
        let cfg = NvidiaGpu {
            device: a.device.clone(),
            hysteresis: 0,
            fans: (0..num)
                .map(|index| NvidiaFan {
                    index,
                    disabled: a.disabled.contains(&index),
                    curve: flat.to_vec(),
                })
                .collect(),
        };
        // Dropped (restored to automatic) at the end of each step and on any error/interrupt.
        let mut gpu = GpuController::new(&nvml, cfg)?;
        let mut last = Vec::new();
        for _ in 0..a.hold_secs {
            interrupt.check()?;
            gpu.tick();
            last.push(dev.temperature(TemperatureSensor::Gpu)? as f64);
            interrupt.sleep(Duration::from_secs(1))?;
        }
        let tail = &last[last.len().saturating_sub(5)..];
        let temp = tail.iter().sum::<f64>() / tail.len().max(1) as f64;
        eprintln!("  {pct:>3}% -> {temp:.1}C");
        measured.push((pct, temp));
    }

    // ponytail: single load level; the lowest step that holds the target is the
    // curve's anchor and the shape around it is fixed. Re-run under lighter and
    // heavier load and merge the anchors for a measured multi-point curve.
    let floor = measured.first().map_or(min, |m| m.0);
    let anchor = measured
        .iter()
        .find(|(_, t)| *t <= a.target_c as f64)
        .map_or(max, |m| m.0);
    let t = a.target_c;
    let curve = [(t - 20, floor), (t, anchor), (t + 10, max)];
    let mut out = String::from(
        "# Suggested curve for the healthy fans; the disabled fan contributed nothing during the sweep.\n",
    );
    for (temp, percent) in dedup(&curve) {
        out.push_str(&format!("{{ temp = {temp}; percent = {percent}; }}\n"));
    }
    Ok(out)
}

/// Keep temps strictly ascending and percents non-decreasing (anchor may equal floor or max).
fn dedup(points: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &(t, p) in points {
        if out.last().is_none_or(|l| t > l.0 && p >= l.1) {
            out.push((t, p));
        }
    }
    out
}
