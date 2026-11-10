use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::thermal::{derivative::DerivativeTracker, tuning::ThermalSystem};
use sc_core::config::Config;
use sc_core::ipc;
use sc_hwmon::{
    fan::{Fan, FanControlGuard},
    sensor::Sensor,
};

/// Resolved runtime state for a single managed fan.
struct ManagedFan {
    config_idx: usize,
    derivative_boost: f64,
}

pub fn run_daemon(config_path: &Path) -> Result<()> {
    init_tracing();

    tracing::info!(config = %config_path.display(), "starting SmartCool daemon");

    let config = sc_core::config::load(config_path)?;

    // Resolve sensors
    // (sensor, derivative window, hysteresis-held temperature)
    let mut sensors: Vec<(Sensor, DerivativeTracker, Option<f64>)> = Vec::new();
    for sc in &config.sensors {
        let sensor = Sensor::from_config(&sc.name, &sc.hwmon, sc.index, sc.hwmon_instance)
            .with_context(|| format!("failed to initialize sensor '{}'", sc.name))?;
        let tracker = DerivativeTracker::new(config.derivative.window_size);
        tracing::info!(
            sensor = %sc.name,
            hwmon = %sc.hwmon,
            index = sc.index,
            "initialized sensor"
        );
        sensors.push((sensor, tracker, None));
    }

    // NVML fans: validated against the hardware before any are touched. The
    // controllers restore driver control on drop, so declare them after `nvml`.
    let nvml = (!config.nvidia.is_empty())
        .then(|| nvml_wrapper::Nvml::init().context("failed to initialize NVML"))
        .transpose()?;
    let mut gpus = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    if let Some(nvml) = &nvml {
        for gpu in &config.nvidia {
            let ctl = sc_hwmon::nvidia::GpuController::new(nvml, gpu.clone())?;
            let id = ctl.bus_id()?;
            anyhow::ensure!(
                !seen.contains(&id),
                "nvidia device '{}' resolves to {id}, already configured",
                gpu.device
            );
            seen.push(id);
            gpus.push(ctl);
        }
    }

    // Resolve every fan before taking control of any of them.
    let mut fans = Vec::new();
    for fc in &config.fans {
        let fan = Fan::from_config(&fc.name, &fc.hwmon, fc.pwm_index, fc.hwmon_instance)
            .with_context(|| format!("failed to initialize fan '{}'", fc.name))?;
        fans.push(fan);
    }

    // The guard restores every acquired channel on all ordinary error, panic,
    // signal, and shutdown paths.
    let mut fan_control = FanControlGuard::new(&mut fans);
    let mut managed_fans: Vec<ManagedFan> = Vec::new();
    for (i, (fan, fc)) in fan_control
        .fans_mut()
        .iter_mut()
        .zip(config.fans.iter())
        .enumerate()
    {
        fan.set_manual()
            .with_context(|| format!("failed to set fan '{}' to manual mode", fc.name))?;
        tracing::info!(
            fan = %fc.name,
            hwmon = %fc.hwmon,
            pwm_index = fc.pwm_index,
            "initialized fan (manual mode)"
        );
        managed_fans.push(ManagedFan {
            config_idx: i,
            derivative_boost: 0.0,
        });
    }

    // Initialize thermal integral tracking
    let sensor_names: Vec<String> = config.sensors.iter().map(|s| s.name.clone()).collect();
    let mut thermal_system =
        ThermalSystem::new(config.poll_interval_ms as f64 / 1000.0, &sensor_names);
    tracing::info!("initialized thermal integral tracking");

    // Set up shared state for IPC
    let shared_state = Arc::new(Mutex::new(crate::ipc_server::DaemonState {
        status: ipc::StatusResponse {
            sensors: Vec::new(),
            fans: Vec::new(),
        },
        tuning: ipc::TuningResponse {
            coupling_matrix: Vec::new(),
            step_responses: Vec::new(),
            load_profiles: Vec::new(),
            cross_correlations: Vec::new(),
            thermal_integrals: Vec::new(),
        },
    }));

    // Start IPC server
    crate::ipc_server::start(Arc::clone(&shared_state))?;

    // Set up signal handling
    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&shutdown))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&shutdown))?;

    let poll_interval = Duration::from_millis(config.poll_interval_ms);
    let mut ready = false;

    // Main control loop
    while !shutdown.load(Ordering::Relaxed) {
        let tick_start = Instant::now();

        for gpu in &mut gpus {
            gpu.tick();
        }
        control_tick(
            &config,
            &mut sensors,
            fan_control.fans_mut(),
            &mut managed_fans,
            &mut thermal_system,
            &shared_state,
        )
        .context("control tick failed")?;

        if ready {
            sd_notify::notify(&[sd_notify::NotifyState::Watchdog])
                .context("notify systemd watchdog")?;
        } else {
            sd_notify::notify(&[sd_notify::NotifyState::Ready])
                .context("notify systemd readiness")?;
            ready = true;
            tracing::info!(
                poll_ms = config.poll_interval_ms,
                sensors = sensors.len(),
                fans = managed_fans.len(),
                "daemon ready"
            );
        }

        let elapsed = tick_start.elapsed();
        if elapsed < poll_interval {
            sleep_interruptibly(&shutdown, poll_interval - elapsed);
        }
    }

    tracing::info!("shutting down, restoring fan control modes");
    let _ = sd_notify::notify(&[sd_notify::NotifyState::Stopping]);
    fan_control
        .restore()
        .context("failed to restore fan control modes")?;
    drop(fan_control);
    drop(gpus);

    let _ = std::fs::remove_file(ipc::SOCKET_PATH);

    tracing::info!("shutdown complete");
    Ok(())
}

fn sleep_interruptibly(shutdown: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while !shutdown.load(Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(
            Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

fn control_tick(
    config: &Config,
    sensors: &mut [(Sensor, DerivativeTracker, Option<f64>)],
    fans: &mut [Fan],
    managed_fans: &mut [ManagedFan],
    thermal_system: &mut ThermalSystem,
    shared_state: &Arc<Mutex<crate::ipc_server::DaemonState>>,
) -> Result<()> {
    anyhow::ensure!(
        fans.len() == managed_fans.len(),
        "fan controller state is inconsistent"
    );
    let mut sensor_temps: HashMap<String, f64> = HashMap::new();
    let mut sensor_derivatives: HashMap<String, f64> = HashMap::new();
    let mut parked: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (sensor, tracker, held) in sensors.iter_mut() {
        if sensor.is_parked() {
            // Restart the derivative window so a resumed GPU is not compared
            // against readings from before it slept.
            *tracker = DerivativeTracker::new(config.derivative.window_size);
            *held = None;
            parked.insert(sensor.name.clone());
            continue;
        }
        match sensor.read_temp_c() {
            Ok(temp) if temp > 0.0 && temp <= 150.0 => {
                tracker.push(temp);
                let dt = tracker.dt_per_second();
                *held = Some(hold(*held, temp, config.hysteresis));
                sensor_temps.insert(sensor.name.clone(), held.unwrap_or(temp));
                sensor_derivatives.insert(sensor.name.clone(), dt);
            }
            Ok(temp) => {
                tracing::warn!(sensor = %sensor.name, temp, "sensor reading is outside 0<temp<=150C");
            }
            Err(e) => {
                tracing::warn!(sensor = %sensor.name, error = %e, "failed to read sensor");
            }
        }
    }

    let mut status_fans = Vec::new();

    for (fan, mf) in fans.iter_mut().zip(managed_fans.iter_mut()) {
        let fan_config = &config.fans[mf.config_idx];
        let readings = linked_sensor_maxima(
            &fan_config.sensors,
            &sensor_temps,
            &sensor_derivatives,
            &parked,
        );

        let (base_pwm, boost, target_pwm) = if let Some((max_temp, max_dt)) = readings {
            let base_pwm = base_pwm(fan_config, &sensor_temps, &parked);
            mf.derivative_boost = next_boost(
                mf.derivative_boost,
                max_temp,
                max_dt,
                base_pwm,
                &config.derivative,
            );
            let boost = mf.derivative_boost.round() as i16;
            let target_pwm = (base_pwm as i16 + boost).clamp(0, 255) as u8;
            (base_pwm, boost, target_pwm)
        } else {
            mf.derivative_boost = 0.0;
            tracing::error!(
                fan = %fan.name,
                "linked sensor unavailable; commanding fail-safe PWM"
            );
            (u8::MAX, 0, u8::MAX)
        };

        fan.write_pwm(target_pwm)
            .with_context(|| format!("failed to write PWM for fan '{}'", fan.name))?;
        let actual_pwm = fan
            .read_pwm()
            .with_context(|| format!("failed to verify PWM for fan '{}'", fan.name))?;
        anyhow::ensure!(
            actual_pwm == target_pwm,
            "fan '{}': requested PWM {}, read back {}",
            fan.name,
            target_pwm,
            actual_pwm
        );

        let rpm = fan.read_rpm().unwrap_or(0);

        status_fans.push(ipc::FanStatus {
            name: fan.name.clone(),
            pwm: actual_pwm,
            rpm,
            base_pwm,
            boost_applied: boost,
        });
    }

    // Update thermal integral tracking
    thermal_system.update(&sensor_temps);

    // Build IPC state
    let status_sensors: Vec<ipc::SensorStatus> = sensors
        .iter()
        .map(|(sensor, tracker, _)| ipc::SensorStatus {
            name: sensor.name.clone(),
            temp_c: tracker.latest_temp().unwrap_or(0.0),
            dt_per_sec: tracker.dt_per_second(),
        })
        .collect();

    let tuning_response = ipc::TuningResponse {
        coupling_matrix: Vec::new(),
        step_responses: Vec::new(),
        cross_correlations: Vec::new(),
        thermal_integrals: thermal_system
            .thermal_integrals()
            .into_iter()
            .map(|i| ipc::IntegralEntry {
                sensor: i.sensor,
                integral: i.integral,
                baseline: i.baseline,
            })
            .collect(),
        load_profiles: Vec::new(),
    };

    if let Ok(mut state) = shared_state.lock() {
        state.status = ipc::StatusResponse {
            sensors: status_sensors,
            fans: status_fans,
        };
        state.tuning = tuning_response;
    }

    Ok(())
}

/// Temperature the curves see: a falling reading holds the previous value
/// until it drops more than `hysteresis` °C below it.
fn hold(held: Option<f64>, temp: f64, hysteresis: u32) -> f64 {
    match held {
        Some(h) if temp < h && h - temp <= f64::from(hysteresis) => h,
        _ => temp,
    }
}

/// Highest PWM any active linked sensor asks for through its own curve
/// (or the fan's default curve); the curve floor when all are parked.
fn base_pwm(
    fan: &sc_core::config::FanConfig,
    temps: &HashMap<String, f64>,
    parked: &std::collections::HashSet<String>,
) -> u8 {
    fan.sensors
        .iter()
        .filter(|s| !parked.contains(*s))
        .filter_map(|s| temps.get(s).map(|&t| fan.sensor_pwm(s, t)))
        .max()
        .unwrap_or_else(|| fan.interpolate_pwm(f64::NEG_INFINITY))
}

/// Derivative boost for the next tick.
///
/// Rises with dT/dt above the threshold and decays otherwise; it never
/// exceeds the headroom above `base_pwm`, and is cleared when every linked
/// sensor is parked (`max_temp` = −∞), so an idle GPU's fan drops straight
/// to its floor.
fn next_boost(
    boost: f64,
    max_temp: f64,
    max_dt: f64,
    base_pwm: u8,
    cfg: &sc_core::config::DerivativeConfig,
) -> f64 {
    if max_temp == f64::NEG_INFINITY {
        return 0.0;
    }
    let boost = if max_dt > cfg.boost_threshold {
        boost + max_dt * 5.0
    } else if max_dt < 0.0 {
        (boost - cfg.decay_rate).max(0.0)
    } else {
        (boost - cfg.decay_rate * 0.5).max(0.0)
    };
    boost.min(f64::from(u8::MAX - base_pwm))
}

/// Hottest temperature and fastest rise across a fan's linked sensors.
///
/// `None` (fail-safe) if any active linked sensor is unreadable. Parked
/// sensors (runtime-suspended GPUs) are skipped; if every linked sensor is
/// parked the fan idles at its curve floor (temperature −∞, no rise).
fn linked_sensor_maxima(
    sensor_names: &[String],
    temperatures: &HashMap<String, f64>,
    derivatives: &HashMap<String, f64>,
    parked: &std::collections::HashSet<String>,
) -> Option<(f64, f64)> {
    let mut max_temp = f64::NEG_INFINITY;
    let mut max_dt = f64::NEG_INFINITY;

    for name in sensor_names {
        if parked.contains(name) {
            continue;
        }
        let &temp = temperatures.get(name)?;
        let &dt = derivatives.get(name)?;
        max_temp = max_temp.max(temp);
        max_dt = max_dt.max(dt);
    }

    if max_temp.is_finite() {
        Some((max_temp, max_dt))
    } else if !sensor_names.is_empty() && sensor_names.iter().all(|n| parked.contains(n)) {
        Some((f64::NEG_INFINITY, 0.0))
    } else {
        None
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let registry = tracing_subscriber::registry().with(filter);

    if let Ok(journald) = tracing_journald::layer() {
        registry.with(journald).init();
    } else {
        registry
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .init();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_sleep_returns_promptly_after_shutdown() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&shutdown);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            signal.store(true, Ordering::Relaxed);
        });

        let started = Instant::now();
        sleep_interruptibly(&shutdown, Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn linked_sensor_maxima_requires_every_sensor() {
        let names = vec!["cpu".to_owned(), "gpu".to_owned()];
        let temperatures = HashMap::from([("cpu".to_owned(), 60.0)]);
        let derivatives = HashMap::from([("cpu".to_owned(), 1.0)]);

        assert_eq!(
            linked_sensor_maxima(&names, &temperatures, &derivatives, &Default::default()),
            None
        );
    }

    #[test]
    fn linked_sensor_maxima_selects_hottest_and_fastest() {
        let names = vec!["cpu".to_owned(), "gpu".to_owned()];
        let temperatures = HashMap::from([("cpu".to_owned(), 70.0), ("gpu".to_owned(), 80.0)]);
        let derivatives = HashMap::from([("cpu".to_owned(), -3.0), ("gpu".to_owned(), 2.0)]);

        // A cooling sensor must not mask one that is heating up.
        assert_eq!(
            linked_sensor_maxima(&names, &temperatures, &derivatives, &Default::default()),
            Some((80.0, 2.0))
        );
    }

    #[test]
    fn hysteresis_holds_small_drops_only() {
        assert_eq!(hold(None, 80.0, 3), 80.0);
        assert_eq!(hold(Some(80.0), 78.0, 3), 80.0);
        assert_eq!(hold(Some(80.0), 76.0, 3), 76.0);
        assert_eq!(hold(Some(80.0), 85.0, 3), 85.0);
        assert_eq!(hold(Some(80.0), 79.0, 0), 79.0);
    }

    #[test]
    fn each_sensor_drives_its_own_curve() {
        let point = |temp, pwm| sc_core::config::CurvePoint { temp, pwm };
        let fan = sc_core::config::FanConfig {
            name: "front".into(),
            hwmon: "nct6799".into(),
            pwm_index: 1,
            hwmon_instance: None,
            topology: sc_core::config::FanTopology {
                position: sc_core::config::FanPosition::Front,
                direction: sc_core::config::AirflowDirection::Intake,
                group: None,
            },
            sensors: vec!["cpu".into(), "gpu_junction".into()],
            curve: vec![point(45, 0), point(95, 255)],
            sensor_curves: vec![sc_core::config::SensorCurve {
                sensor: "gpu_junction".into(),
                curve: vec![point(50, 0), point(110, 255)],
            }],
        };
        let temps = HashMap::from([("cpu".to_owned(), 45.0), ("gpu_junction".to_owned(), 95.0)]);
        // 95 °C junction is ~75% of its own range, not full speed.
        assert_eq!(base_pwm(&fan, &temps, &Default::default()), 191);
        let parked = std::collections::HashSet::from(["cpu".to_owned(), "gpu_junction".to_owned()]);
        assert_eq!(base_pwm(&fan, &HashMap::new(), &parked), 0);
    }

    #[test]
    fn boost_is_capped_and_cleared_when_parked() {
        let cfg = sc_core::config::DerivativeConfig {
            window_size: 10,
            boost_threshold: 2.0,
            decay_rate: 0.0,
        };
        let boost = next_boost(0.0, 70.0, 40.0, 100, &cfg);
        assert_eq!(boost, 155.0);
        assert_eq!(next_boost(boost, f64::NEG_INFINITY, 0.0, 0, &cfg), 0.0);
    }

    #[test]
    fn parked_gpu_sensors_idle_instead_of_fail_safe() {
        let names = vec!["cpu".to_owned(), "gpu".to_owned()];
        let temperatures = HashMap::from([("cpu".to_owned(), 60.0)]);
        let derivatives = HashMap::from([("cpu".to_owned(), 0.5)]);
        let gpu_parked = std::collections::HashSet::from(["gpu".to_owned()]);

        assert_eq!(
            linked_sensor_maxima(&names, &temperatures, &derivatives, &gpu_parked),
            Some((60.0, 0.5))
        );
        let gpu_only = vec!["gpu".to_owned()];
        let (temp, dt) =
            linked_sensor_maxima(&gpu_only, &HashMap::new(), &HashMap::new(), &gpu_parked).unwrap();
        assert!(temp == f64::NEG_INFINITY && dt == 0.0);
    }
}
