//! Read-only ASUS status, verification, and bounded monitoring.
use super::verify::{curve_mismatches, fan_rpms};
use super::{
    discover, print_stored_curves, FanCurvesBusProxy, PlatformBusProxy, Profile, RuntimeState,
};
use anyhow::{Context, Result};
use sc_core::asusd_config;
use sc_hwmon::telemetry::{read_snapshot, Snapshot};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const HWMON: &str = "/sys/class/hwmon";

fn snapshot(platform: &PlatformBusProxy<'_>, expected: Profile) -> Result<Snapshot> {
    let before = Profile::from_id(platform.platform_profile()?)?;
    let snapshot = read_snapshot(Path::new(HWMON))?;
    let after = Profile::from_id(platform.platform_profile()?)?;
    anyhow::ensure!(before == expected && after == expected,
        "monitor/observation pinned to '{}', observed '{}' then '{}'; start a new observation after the profile settles",
        expected.name(), before.name(), after.name());
    Ok(snapshot)
}

fn stored_json(runtime: &RuntimeState) -> BTreeMap<&str, &Vec<asusd_config::WireCurve>> {
    runtime
        .profiles
        .iter()
        .map(|profile| (profile.name(), &runtime.curves[profile]))
        .collect()
}

fn print_live(snapshot: &Snapshot) {
    println!("Observed RPM (zero means stopped; unavailable is not zero):");
    for fan in &snapshot.fans {
        println!(
            "  {}/{}: {}",
            fan.chip,
            fan.label
                .clone()
                .unwrap_or_else(|| format!("fan{}", fan.index)),
            fan.rpm.map_or_else(
                || format!(
                    "unavailable ({})",
                    fan.error.as_deref().unwrap_or("unknown")
                ),
                |rpm| format!("{rpm} RPM")
            )
        );
    }
    for temperature in &snapshot.temperatures {
        println!(
            "  {}/{}: {}",
            temperature.chip,
            temperature
                .label
                .clone()
                .unwrap_or_else(|| format!("temp{}", temperature.index)),
            temperature.temp_c.map_or_else(
                || format!(
                    "unavailable ({})",
                    temperature.error.as_deref().unwrap_or("unknown")
                ),
                |temp| format!("{temp:.1} C")
            )
        );
    }
    println!("Kernel-exposed ASUS curves (driver state, not EC read-back):");
    for curve in &snapshot.curves {
        println!(
            "  fan{} enabled={:?} temp={:?} pwm={:?} error={:?}",
            curve.index, curve.enabled, curve.temp, curve.pwm, curve.error
        );
    }
}

pub(super) fn status(
    platform: &PlatformBusProxy<'_>,
    fans: &FanCurvesBusProxy<'_>,
    json_output: bool,
) -> Result<()> {
    let runtime = discover(platform, fans)?;
    let snapshot = snapshot(platform, runtime.active)?;
    if json_output {
        println!(
            "{}",
            json!({"active_profile": runtime.active.name(), "stored_curves": stored_json(&runtime), "observations": snapshot})
        );
    } else {
        print_stored_curves(&runtime);
        print_live(&snapshot);
    }
    Ok(())
}

pub(super) fn verify(
    platform: &PlatformBusProxy<'_>,
    fans: &FanCurvesBusProxy<'_>,
    path: &Path,
    json_output: bool,
) -> Result<()> {
    let config = asusd_config::load(path)?;
    let runtime = discover(platform, fans)?;
    let snapshot = snapshot(platform, runtime.active)?;
    let issues = curve_mismatches(&config, &runtime, &snapshot);
    if json_output {
        println!(
            "{}",
            json!({"active_profile": runtime.active.name(), "stored_curves": stored_json(&runtime), "observations": snapshot, "curve_matches": issues.is_empty(), "issues": issues})
        );
    } else {
        print_live(&snapshot);
        for issue in &issues {
            eprintln!("  {issue}");
        }
    }
    anyhow::ensure!(
        issues.is_empty(),
        "ASUS stored/kernel curve verification failed"
    );
    if !json_output {
        println!("Stored declarations and active kernel curve match. This does not establish fan silence; sample RPM with `sc asusd monitor --expect-stopped`.");
    }
    Ok(())
}

pub(super) fn monitor(
    platform: &PlatformBusProxy<'_>,
    fans: &FanCurvesBusProxy<'_>,
    duration_seconds: u64,
    interval_ms: u64,
    expect_stopped: bool,
    json_output: bool,
) -> Result<()> {
    let runtime = discover(platform, fans)?;
    let start = Instant::now();
    let duration = Duration::from_secs(duration_seconds);
    let interval = Duration::from_millis(interval_ms);
    let mut samples = 0u64;
    let mut stopped_samples = 0u64;
    let mut max_rpm = BTreeMap::<String, u32>::new();
    loop {
        let observation = snapshot(platform, runtime.active);
        let rpms = observation
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))
            .and_then(|snapshot| fan_rpms(&runtime, snapshot));
        let stopped = rpms
            .as_ref()
            .is_ok_and(|values| values.values().all(|rpm| *rpm == 0));
        if let Ok(values) = &rpms {
            for (fan, rpm) in values {
                let max = max_rpm.entry(fan.clone()).or_default();
                *max = (*max).max(*rpm);
            }
        }
        samples += 1;
        stopped_samples += u64::from(stopped);
        let elapsed = start.elapsed().as_secs_f64();
        if json_output {
            println!(
                "{}",
                json!({"type": "sample", "timestamp_ms": SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(), "elapsed_seconds": elapsed, "active_profile": runtime.active.name(), "observations": observation.as_ref().ok(), "all_stopped": stopped, "error": rpms.err().map(|error| format!("{error:#}"))})
            );
        } else {
            println!(
                "{elapsed:.1}s all_stopped={stopped} RPM={:?} error={:?}",
                rpms.as_ref().ok(),
                rpms.as_ref().err().map(ToString::to_string)
            );
            if let Ok(snapshot) = &observation {
                print_live(snapshot);
            }
        }
        io::stdout().flush().context("flush monitor output")?;
        let remaining = duration.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(interval.min(remaining));
    }
    let all_stopped = samples > 0 && samples == stopped_samples;
    if json_output {
        println!(
            "{}",
            json!({"type": "summary", "elapsed_seconds": start.elapsed().as_secs_f64(), "samples": samples, "stopped_samples": stopped_samples, "all_stopped": all_stopped, "max_rpm": max_rpm})
        );
    } else {
        println!("{stopped_samples}/{samples} samples stopped; maximum RPM: {max_rpm:?}");
    }
    anyhow::ensure!(
        !expect_stopped || all_stopped,
        "fan-stop verification failed: {stopped_samples}/{samples} samples stopped"
    );
    Ok(())
}
