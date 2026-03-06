use anyhow::{Context, Result};
use std::path::Path;
use std::thread;
use std::time::Duration;

use sc_core::config::{
    AirflowDirection, Config, CurvePoint, DerivativeConfig, FanConfig, FanPosition, FanTopology,
    SensorConfig,
};
use sc_core::ipc::{CouplingEntry, StepResponseEntry, TuningResponse};
use sc_detect::DetectionResult;
use sc_hwmon::fan::Fan;
use sc_hwmon::sensor::Sensor;

/// PWM levels for the targeted sweep (low, mid, high, max).
const PWM_LEVELS: &[u8] = &[40, 100, 180, 255];

/// Polling interval during sampling (ms).
const POLL_MS: u64 = 500;

/// Minimum temperature variance (°C²) to consider a coupling meaningful.
const MIN_TEMP_VARIANCE: f64 = 0.01;

/// Try to write a PWM value; if the driver rejects it (EINVAL, common for GPU fans
/// that enforce a minimum), try progressively higher values. Returns the PWM actually set.
fn is_einval(e: &anyhow::Error) -> bool {
    // Check the full error chain for EINVAL
    let msg = format!("{:#}", e);
    msg.contains("Invalid argument")
}

fn write_pwm_safe(fan: &mut Fan, target: u8) -> Result<u8> {
    // Try the requested value first
    match fan.write_pwm(target) {
        Ok(()) => return Ok(target),
        Err(e) if is_einval(&e) => {}
        Err(e) => return Err(e),
    }
    // Driver rejected value — try stepping up to find the minimum accepted
    for pwm in (target + 1)..=255 {
        match fan.write_pwm(pwm) {
            Ok(()) => {
                eprintln!("  (fan '{}' minimum PWM = {}, driver rejected {})", fan.name, pwm, target);
                return Ok(pwm);
            }
            Err(e) if is_einval(&e) => {}
            Err(e) => return Err(e),
        }
    }
    anyhow::bail!("fan '{}': driver rejected all PWM values 0-255", fan.name)
}

// ─── Topology YAML ──────────────────────────────────────────────────────────

/// User-provided fan topology.
#[derive(serde::Deserialize)]
pub struct Topology {
    pub fans: Vec<FanTopo>,
}

/// A single fan header — may drive fans in multiple physical locations.
#[derive(serde::Deserialize)]
pub struct FanTopo {
    /// Must match a detected fan's auto_name
    pub name: String,
    /// Physical locations of fans on this header
    pub positions: Vec<FanPlacement>,
    /// Group name for coordinated control (e.g. "case")
    pub group: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct FanPlacement {
    pub location: String,
    pub direction: String,
    /// Number of fans at this location (default 1)
    #[serde(default = "default_count")]
    pub count: u32,
}

fn default_count() -> u32 { 1 }

pub fn load_topology(path: &Path) -> Result<Topology> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read topology file: {}", path.display()))?;
    let topo: Topology = serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse topology YAML: {}", path.display()))?;
    anyhow::ensure!(!topo.fans.is_empty(), "topology file has no fan entries");
    Ok(topo)
}

fn parse_position(s: &str) -> Result<FanPosition> {
    match s {
        "front" => Ok(FanPosition::Front),
        "rear" => Ok(FanPosition::Rear),
        "top" => Ok(FanPosition::Top),
        "bottom" => Ok(FanPosition::Bottom),
        "side" => Ok(FanPosition::Side),
        "cpu_cooler" => Ok(FanPosition::CpuCooler),
        "gpu_cooler" => Ok(FanPosition::GpuCooler),
        _ => anyhow::bail!("unknown position: '{}' (expected: front, rear, top, bottom, side, cpu_cooler, gpu_cooler)", s),
    }
}

fn parse_direction(s: &str) -> Result<AirflowDirection> {
    match s {
        "intake" => Ok(AirflowDirection::Intake),
        "exhaust" => Ok(AirflowDirection::Exhaust),
        _ => anyhow::bail!("unknown direction: '{}' (expected: intake, exhaust)", s),
    }
}

// ─── Fan classification ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum FanRole {
    /// Directly cools one component (cpu_cooler, gpu_cooler)
    DirectCooler,
    /// Case airflow fan — representative of its group (or solo)
    CaseRepresentative,
    /// Case fan in a group — follows the representative's results
    CaseFollower { representative: String },
}

struct ClassifiedFan {
    name: String,
    role: FanRole,
    position: FanPosition,
    direction: AirflowDirection,
    /// Sensor names this fan is expected to primarily affect
    expected_sensors: Vec<String>,
}

/// Classify fans based on topology to determine benchmark strategy.
fn classify_fans(
    topo: &Topology,
    sensor_names: &[String],
) -> Result<Vec<ClassifiedFan>> {
    let mut classified = Vec::new();
    let mut group_reps: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    // First pass: identify group representatives (first fan in each group)
    for ft in &topo.fans {
        if let Some(ref group) = ft.group {
            group_reps.entry(group.clone()).or_insert_with(|| ft.name.clone());
        }
    }

    for ft in &topo.fans {
        anyhow::ensure!(!ft.positions.is_empty(), "fan '{}' has no positions", ft.name);

        let primary = ft.positions.iter().max_by_key(|p| p.count).unwrap();
        let position = parse_position(&primary.location)?;
        let direction = parse_direction(&primary.direction)?;

        let is_direct = matches!(position, FanPosition::CpuCooler | FanPosition::GpuCooler);

        let role = if is_direct {
            FanRole::DirectCooler
        } else if let Some(ref group) = ft.group {
            let rep = group_reps.get(group).unwrap();
            if *rep == ft.name {
                FanRole::CaseRepresentative
            } else {
                FanRole::CaseFollower { representative: rep.clone() }
            }
        } else {
            FanRole::CaseRepresentative
        };

        // Determine which sensors this fan is expected to affect
        let expected_sensors = match position {
            FanPosition::CpuCooler => {
                sensor_names.iter()
                    .filter(|s| s.starts_with("cpu"))
                    .cloned()
                    .collect()
            }
            FanPosition::GpuCooler => {
                sensor_names.iter()
                    .filter(|s| s.starts_with("gpu"))
                    .cloned()
                    .collect()
            }
            _ => {
                // Case fans can affect everything
                sensor_names.to_vec()
            }
        };

        classified.push(ClassifiedFan {
            name: ft.name.clone(),
            role,
            position,
            direction,
            expected_sensors,
        });
    }

    Ok(classified)
}

// ─── Prepare from detection ─────────────────────────────────────────────────

/// Match detected hardware to topology, resolve hwmon paths, build a Config.
pub fn prepare_from_detect(
    result: &DetectionResult,
    topo: &Topology,
) -> Result<(Vec<Fan>, Vec<Sensor>, Vec<String>, Config)> {
    // Build instance map for hwmon disambiguation (same logic as detect.rs)
    let instance_map = build_instance_map(result);

    let mut sensors = Vec::new();
    let mut sensor_names = Vec::new();
    let mut sensor_configs = Vec::new();

    for ds in &result.sensors {
        match Sensor::from_path(&ds.auto_name, &ds.hwmon_path, ds.index) {
            Ok(sensor) => {
                sensor_names.push(ds.auto_name.clone());
                sensors.push(sensor);
                sensor_configs.push(SensorConfig {
                    name: ds.auto_name.clone(),
                    hwmon: ds.hwmon_chip.clone(),
                    index: ds.index,
                    hwmon_instance: hwmon_instance_for(&instance_map, &ds.hwmon_chip, &ds.hwmon_path),
                });
            }
            Err(e) => eprintln!("skipping sensor '{}': {}", ds.auto_name, e),
        }
    }

    let mut fans = Vec::new();
    let mut fan_configs = Vec::new();

    for ft in &topo.fans {
        let detected = result.fans.iter().find(|df| df.auto_name == ft.name);
        let df = match detected {
            Some(df) => df,
            None => {
                let available: Vec<&str> = result.fans.iter().map(|f| f.auto_name.as_str()).collect();
                anyhow::bail!(
                    "topology fan '{}' not found in detected hardware. Available: {:?}",
                    ft.name, available
                );
            }
        };

        anyhow::ensure!(
            !ft.positions.is_empty(),
            "fan '{}' has no positions defined",
            ft.name
        );

        let primary = ft.positions.iter().max_by_key(|p| p.count).unwrap();
        let position = parse_position(&primary.location)?;
        let direction = parse_direction(&primary.direction)?;

        match Fan::from_path(&df.auto_name, &df.hwmon_path, df.pwm_index) {
            Ok(fan) => {
                fans.push(fan);
                fan_configs.push(FanConfig {
                    name: df.auto_name.clone(),
                    hwmon: df.hwmon_chip.clone(),
                    pwm_index: df.pwm_index,
                    hwmon_instance: hwmon_instance_for(&instance_map, &df.hwmon_chip, &df.hwmon_path),
                    topology: FanTopology {
                        position,
                        direction,
                        group: ft.group.clone(),
                    },
                    sensors: sensor_names.clone(),
                    curve: default_curve(),
                });
            }
            Err(e) => eprintln!("skipping fan '{}': {}", df.auto_name, e),
        }
    }

    if fans.is_empty() {
        anyhow::bail!("no fans resolved — cannot benchmark");
    }

    let config = Config {
        poll_interval_ms: 2000,
        derivative: DerivativeConfig {
            window_size: 10,
            boost_threshold: 2.0,
            decay_rate: 0.5,
        },
        sensors: sensor_configs,
        fans: fan_configs,
        tuning: Default::default(),
    };

    Ok((fans, sensors, sensor_names, config))
}

/// Build a map of chip_name -> sorted vec of unique hwmon paths for disambiguation.
fn build_instance_map(result: &DetectionResult) -> std::collections::HashMap<String, Vec<std::path::PathBuf>> {
    let mut map: std::collections::HashMap<String, Vec<std::path::PathBuf>> = std::collections::HashMap::new();
    for chip in &result.chips {
        let paths = map.entry(chip.name.clone()).or_default();
        if !paths.contains(&chip.path) {
            paths.push(chip.path.clone());
        }
    }
    for paths in map.values_mut() {
        paths.sort();
    }
    map
}

/// Return Some(index) when a chip name has multiple hwmon instances, None if unique.
fn hwmon_instance_for(
    map: &std::collections::HashMap<String, Vec<std::path::PathBuf>>,
    chip_name: &str,
    hwmon_path: &std::path::Path,
) -> Option<u32> {
    let paths = map.get(chip_name)?;
    if paths.len() <= 1 {
        return None;
    }
    paths.iter().position(|p| p == hwmon_path).map(|i| i as u32)
}

fn default_curve() -> Vec<CurvePoint> {
    vec![
        CurvePoint { temp: 40, pwm: 0 },
        CurvePoint { temp: 55, pwm: 60 },
        CurvePoint { temp: 65, pwm: 120 },
        CurvePoint { temp: 75, pwm: 180 },
        CurvePoint { temp: 85, pwm: 230 },
        CurvePoint { temp: 95, pwm: 255 },
    ]
}

// ─── Adaptive settle detection ──────────────────────────────────────────────

/// Wait for temperatures to stabilize using dual EWMA convergence.
/// Returns early once convergence is detected, with a minimum of 5s and max of 20s.
fn wait_for_settle(sensors: &[Sensor]) -> Vec<f64> {
    let min_secs = 5;
    let max_secs = 20;
    let alpha_fast = 0.3;
    let alpha_slow = 0.1;
    let convergence_threshold = 0.15; // °C

    let n = sensors.len();
    let mut ewma_fast = vec![0.0f64; n];
    let mut ewma_slow = vec![0.0f64; n];
    let mut initialized = false;

    let start = std::time::Instant::now();

    loop {
        let elapsed = start.elapsed().as_secs();

        // Read current temperatures
        let temps: Vec<f64> = sensors.iter().map(|s| s.read_temp_c().unwrap_or(0.0)).collect();

        if !initialized {
            ewma_fast = temps.clone();
            ewma_slow = temps.clone();
            initialized = true;
        } else {
            for i in 0..n {
                ewma_fast[i] = alpha_fast * temps[i] + (1.0 - alpha_fast) * ewma_fast[i];
                ewma_slow[i] = alpha_slow * temps[i] + (1.0 - alpha_slow) * ewma_slow[i];
            }
        }

        // Check convergence after minimum wait
        if elapsed >= min_secs {
            let max_diff = ewma_fast.iter().zip(ewma_slow.iter())
                .map(|(f, s)| (f - s).abs())
                .fold(0.0f64, f64::max);

            if max_diff < convergence_threshold {
                return temps;
            }
        }

        if elapsed >= max_secs {
            return temps;
        }

        thread::sleep(Duration::from_millis(POLL_MS));
    }
}

/// Sample temperatures over a window, return averages.
fn sample_temps(sensors: &[Sensor], duration_secs: u64) -> Vec<f64> {
    let n_samples = (duration_secs * 1000 / POLL_MS) as usize;
    let mut accumulators = vec![0.0f64; sensors.len()];

    for _ in 0..n_samples {
        for (j, sensor) in sensors.iter().enumerate() {
            if let Ok(temp) = sensor.read_temp_c() {
                accumulators[j] += temp;
            }
        }
        thread::sleep(Duration::from_millis(POLL_MS));
    }

    accumulators.iter().map(|acc| acc / n_samples as f64).collect()
}

// ─── Stall detection ────────────────────────────────────────────────────────

/// Find the minimum PWM where the fan starts spinning (RPM > 0).
/// Uses binary search between 0 and 80 PWM.
fn find_stall_threshold(fan: &mut Fan) -> Result<u8> {
    let mut lo: u8 = 0;
    let mut hi: u8 = 80;
    let mut stall_pwm: u8 = 0;

    // First check if fan spins at hi
    let actual_hi = write_pwm_safe(fan, hi)?;
    thread::sleep(Duration::from_secs(3));
    let rpm = fan.read_rpm().unwrap_or(0);
    if rpm == 0 {
        // Fan doesn't spin even at 80, skip stall detection
        return Ok(0);
    }

    // If driver enforced a minimum above lo, start from there
    lo = lo.max(actual_hi.saturating_sub(hi).max(0));

    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let actual = write_pwm_safe(fan, mid)?;
        thread::sleep(Duration::from_secs(2));
        let rpm = fan.read_rpm().unwrap_or(0);

        if rpm > 0 {
            stall_pwm = actual;
            hi = mid;
        } else {
            lo = mid + 1;
        }

        // Stop if range is small enough
        if hi - lo <= 3 {
            break;
        }
    }

    Ok(stall_pwm)
}

// ─── Benchmark ──────────────────────────────────────────────────────────────

/// Per-fan measurement data collected during benchmark.
struct FanMeasurement {
    fan_name: String,
    role: FanRole,
    stall_pwm: u8,
    /// For each PWM level: (pwm, per-sensor average temps)
    readings: Vec<(u8, Vec<f64>)>,
}

/// Run topology-adaptive thermal coupling benchmark.
///
/// Protocol:
/// 1. Discovery: global baseline (all fans off), stall detection
/// 2. Targeted sweeps: 4 PWM levels per fan, adaptive settle
/// 3. Skips: group followers use representative's data, direct coolers test fewer sensors
pub fn run_benchmark(
    fans: &mut [Fan],
    sensors: &[Sensor],
    sensor_names: &[String],
    topo: &Topology,
) -> Result<TuningResponse> {
    let classified = classify_fans(topo, sensor_names)?;
    let n_active = classified.iter().filter(|c| c.role != FanRole::DirectCooler || {
        // DirectCoolers are always benchmarked
        true
    }).filter(|c| !matches!(c.role, FanRole::CaseFollower { .. })).count();

    eprintln!(
        "Adaptive benchmark: {} fans ({} to sweep), {} sensors",
        fans.len(), n_active, sensors.len()
    );

    let est_secs = 30 + n_active as u64 * PWM_LEVELS.len() as u64 * 12;
    eprintln!("Estimated duration: ~{} minutes", est_secs / 60);

    // Set all fans to manual mode, tracking which ones are controllable
    let mut controllable: Vec<bool> = Vec::new();
    for fan in fans.iter_mut() {
        if let Err(e) = fan.set_manual() {
            eprintln!("  fan '{}': cannot set manual mode ({}), skipping", fan.name, e);
            controllable.push(false);
            continue;
        }
        // Verify we can actually write PWM
        match write_pwm_safe(fan, 128) {
            Ok(_) => controllable.push(true),
            Err(_) => {
                eprintln!("  fan '{}': driver rejects PWM writes, skipping (firmware-controlled?)", fan.name);
                if let Err(_) = fan.restore_original() {}
                controllable.push(false);
            }
        }
    }

    let n_controllable = controllable.iter().filter(|&&c| c).count();
    if n_controllable == 0 {
        anyhow::bail!("no fans are controllable — cannot benchmark");
    }
    eprintln!("{}/{} fans controllable", n_controllable, fans.len());

    // ── Phase 0: Global baseline ────────────────────────────────────────────

    eprintln!("\n=== Phase 0: Baseline (all fans minimum) ===");
    for (fan, &is_ctrl) in fans.iter_mut().zip(controllable.iter()) {
        if is_ctrl {
            write_pwm_safe(fan, 0)?;
        }
    }
    wait_for_settle(sensors);
    let baseline_avg = sample_temps(sensors, 3);
    eprintln!("Baseline temperatures:");
    for (i, name) in sensor_names.iter().enumerate() {
        eprintln!("  {} = {:.1}°C", name, baseline_avg[i]);
    }

    // ── Phase 0.5: Stall detection ──────────────────────────────────────────

    eprintln!("\n=== Stall detection ===");
    let mut stall_thresholds: std::collections::HashMap<String, u8> = std::collections::HashMap::new();
    for (fan, &is_ctrl) in fans.iter_mut().zip(controllable.iter()) {
        if !is_ctrl {
            eprintln!("  {} ... skipped (not controllable)", fan.name);
            continue;
        }
        eprint!("  {} ... ", fan.name);
        let stall = find_stall_threshold(fan)?;
        eprintln!("stall PWM = {}", stall);
        stall_thresholds.insert(fan.name.clone(), stall);
        write_pwm_safe(fan, 0)?;
    }

    // ── Phase 1: Targeted sweeps ────────────────────────────────────────────

    eprintln!("\n=== Phase 1: Targeted sweeps ===");
    let mut measurements: Vec<FanMeasurement> = Vec::new();

    for (fan_idx, cf) in classified.iter().enumerate() {
        // Skip uncontrollable fans
        if !controllable[fan_idx] {
            eprintln!("\n--- {} (not controllable, skipping) ---", cf.name);
            continue;
        }

        // Skip followers — they'll inherit their representative's data
        if let FanRole::CaseFollower { .. } = cf.role {
            eprintln!("\n--- {} (follower, skipping) ---", cf.name);
            continue;
        }

        eprintln!("\n--- Sweeping: {} ({:?}) ---", cf.name, cf.role);

        // Set all other controllable fans to midpoint
        for (i, fan) in fans.iter_mut().enumerate() {
            if i != fan_idx && controllable[i] {
                write_pwm_safe(fan, 128)?;
            }
        }

        let stall = *stall_thresholds.get(&cf.name).unwrap_or(&0);
        let mut readings = Vec::new();

        // Always include a reading at stall threshold (effective minimum)
        let effective_levels: Vec<u8> = std::iter::once(stall.max(1))
            .chain(PWM_LEVELS.iter().copied().filter(|&p| p > stall))
            .collect();

        for &pwm in &effective_levels {
            eprint!("  PWM={:>3} ... ", pwm);
            write_pwm_safe(&mut fans[fan_idx], pwm)?;

            // Adaptive settle
            wait_for_settle(sensors);

            // Sample
            let temps = sample_temps(sensors, 3);

            let temp_str: Vec<String> = sensor_names.iter().zip(temps.iter())
                .map(|(name, t)| format!("{}={:.1}°C", name, t))
                .collect();
            eprintln!("{}", temp_str.join(", "));

            readings.push((pwm, temps));
        }

        // Restore to midpoint
        write_pwm_safe(&mut fans[fan_idx], 128)?;

        measurements.push(FanMeasurement {
            fan_name: cf.name.clone(),
            role: cf.role.clone(),
            stall_pwm: stall,
            readings,
        });
    }

    // ── Restore all fans ────────────────────────────────────────────────────

    eprintln!("\nRestoring fan control modes...");
    for (fan, &is_ctrl) in fans.iter().zip(controllable.iter()) {
        if is_ctrl {
            if let Err(e) = fan.restore_original() {
                eprintln!("  warning: failed to restore {}: {}", fan.name, e);
            }
        }
    }

    // ── Phase 2: Compute coupling matrix ────────────────────────────────────

    let tuning = compute_tuning(&measurements, &classified, sensor_names, &baseline_avg);
    eprintln!(
        "\nBenchmark complete. {} coupling entries.",
        tuning.coupling_matrix.len()
    );

    Ok(tuning)
}

/// Compute thermal coupling from benchmark measurements.
///
/// For each fan×sensor pair, fits a linear regression of temperature vs PWM.
/// The slope (beta) represents °C change per +10 PWM.
/// Group followers inherit their representative's coupling data.
fn compute_tuning(
    measurements: &[FanMeasurement],
    classified: &[ClassifiedFan],
    sensor_names: &[String],
    _baseline: &[f64],
) -> TuningResponse {
    let mut coupling_matrix = Vec::new();
    let mut step_responses = Vec::new();

    // Build coupling from direct measurements
    for measurement in measurements {
        for (sensor_idx, sensor_name) in sensor_names.iter().enumerate() {
            let readings: Vec<(f64, f64)> = measurement.readings.iter()
                .map(|(pwm, temps)| (*pwm as f64, temps[sensor_idx]))
                .collect();

            if readings.len() < 2 {
                continue;
            }

            // Check if temperature actually varies
            let temps: Vec<f64> = readings.iter().map(|(_, t)| *t).collect();
            let t_mean = temps.iter().sum::<f64>() / temps.len() as f64;
            let variance = temps.iter().map(|t| (t - t_mean).powi(2)).sum::<f64>() / temps.len() as f64;
            if variance < MIN_TEMP_VARIANCE {
                continue;
            }

            // Linear regression: temp = a + b * pwm
            let (slope, _intercept, r_squared) = linear_regression(&readings);

            // beta = °C per +10 PWM
            let beta = slope * 10.0;

            coupling_matrix.push(CouplingEntry {
                fan: measurement.fan_name.clone(),
                sensor: sensor_name.clone(),
                beta,
                r_squared,
            });

            step_responses.push(StepResponseEntry {
                fan: measurement.fan_name.clone(),
                sensor: sensor_name.clone(),
                gain_k: slope,
                tau_ticks: 10.0, // estimated from adaptive settle
                n_events: readings.len(),
            });
        }
    }

    // Propagate coupling data to group followers
    for cf in classified {
        if let FanRole::CaseFollower { ref representative } = cf.role {
            // Copy representative's coupling entries for this follower
            let rep_entries: Vec<CouplingEntry> = coupling_matrix.iter()
                .filter(|e| e.fan == *representative)
                .map(|e| CouplingEntry {
                    fan: cf.name.clone(),
                    sensor: e.sensor.clone(),
                    beta: e.beta,
                    r_squared: e.r_squared,
                })
                .collect();
            coupling_matrix.extend(rep_entries);

            let rep_steps: Vec<StepResponseEntry> = step_responses.iter()
                .filter(|e| e.fan == *representative)
                .map(|e| StepResponseEntry {
                    fan: cf.name.clone(),
                    sensor: e.sensor.clone(),
                    gain_k: e.gain_k,
                    tau_ticks: e.tau_ticks,
                    n_events: e.n_events,
                })
                .collect();
            step_responses.extend(rep_steps);
        }
    }

    TuningResponse {
        coupling_matrix,
        step_responses,
        cross_correlations: Vec::new(),
        thermal_integrals: Vec::new(),
    }
}

/// Simple linear regression: y = a + b*x. Returns (slope, intercept, r²).
fn linear_regression(data: &[(f64, f64)]) -> (f64, f64, f64) {
    let n = data.len() as f64;
    let sum_x: f64 = data.iter().map(|(x, _)| x).sum();
    let sum_y: f64 = data.iter().map(|(_, y)| y).sum();
    let sum_xx: f64 = data.iter().map(|(x, _)| x * x).sum();
    let sum_xy: f64 = data.iter().map(|(x, y)| x * y).sum();

    let denom = n * sum_xx - sum_x * sum_x;
    if denom.abs() < 1e-12 {
        return (0.0, sum_y / n, 0.0);
    }

    let slope = (n * sum_xy - sum_x * sum_y) / denom;
    let intercept = (sum_y - slope * sum_x) / n;

    let y_mean = sum_y / n;
    let ss_tot: f64 = data.iter().map(|(_, y)| (y - y_mean).powi(2)).sum();
    let ss_res: f64 = data.iter()
        .map(|(x, y)| {
            let predicted = intercept + slope * x;
            (y - predicted).powi(2)
        })
        .sum();
    let r_squared = if ss_tot > 1e-12 { 1.0 - ss_res / ss_tot } else { 0.0 };

    (slope, intercept, r_squared)
}
