use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::thermal::{
    analytics::AnalyticsTracker,
    derivative::DerivativeTracker,
    tuning::{ThermalSystem, TuningParams},
};
use sc_core::config::Config;
use sc_core::ipc;
use sc_hwmon::{
    fan::{Fan, FanControlGuard},
    sensor::Sensor,
};

/// Number of analytics history snapshots to retain per fan.
const ANALYTICS_HISTORY_SIZE: usize = 300;
/// Recompute analytics every N ticks.
const ANALYTICS_RECOMPUTE_INTERVAL: u32 = 30;

/// Resolved runtime state for a single managed fan.
struct ManagedFan {
    config_idx: usize,
    derivative_boost: f64,
    analytics: AnalyticsTracker,
}

pub fn run_daemon(config_path: &Path) -> Result<()> {
    init_tracing();

    tracing::info!(config = %config_path.display(), "starting SmartCool daemon");

    let config = sc_core::config::load(config_path)?;

    // Resolve sensors
    let mut sensors: Vec<(Sensor, DerivativeTracker)> = Vec::new();
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
        sensors.push((sensor, tracker));
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
            analytics: AnalyticsTracker::new(&fc.name, ANALYTICS_HISTORY_SIZE),
        });
    }

    // Initialize thermal tuning system
    let fan_names: Vec<String> = config.fans.iter().map(|f| f.name.clone()).collect();
    let sensor_names: Vec<String> = config.sensors.iter().map(|s| s.name.clone()).collect();
    let tuning_params = TuningParams {
        ewma_span: config.tuning.ewma_span,
        ccf_buffer_size: config.tuning.ccf_buffer_size,
        ccf_max_lag: config.tuning.ccf_max_lag,
        step_threshold: config.tuning.step_threshold,
        response_window: config.tuning.response_window,
        regression_min_samples: config.tuning.regression_min_samples,
        poll_interval_secs: config.poll_interval_ms as f64 / 1000.0,
    };
    let mut thermal_system = ThermalSystem::new(&tuning_params, &fan_names, &sensor_names);
    tracing::info!("initialized thermal tuning system");

    // Set up shared state for IPC
    let shared_state = Arc::new(Mutex::new(crate::ipc_server::DaemonState {
        status: ipc::StatusResponse {
            sensors: Vec::new(),
            fans: Vec::new(),
        },
        analytics: ipc::AnalyticsResponse { fans: Vec::new() },
        tuning: ipc::TuningResponse {
            coupling_matrix: Vec::new(),
            step_responses: Vec::new(),
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
    let mut tick: u32 = 0;
    let mut ready = false;

    // Main control loop
    while !shutdown.load(Ordering::Relaxed) {
        let tick_start = Instant::now();

        control_tick(
            &config,
            &mut sensors,
            fan_control.fans_mut(),
            &mut managed_fans,
            &mut thermal_system,
            &shared_state,
            tick,
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

        tick = tick.wrapping_add(1);

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
    sensors: &mut [(Sensor, DerivativeTracker)],
    fans: &mut [Fan],
    managed_fans: &mut [ManagedFan],
    thermal_system: &mut ThermalSystem,
    shared_state: &Arc<Mutex<crate::ipc_server::DaemonState>>,
    tick: u32,
) -> Result<()> {
    anyhow::ensure!(
        fans.len() == managed_fans.len(),
        "fan controller state is inconsistent"
    );
    let mut sensor_temps: HashMap<String, f64> = HashMap::new();
    let mut sensor_derivatives: HashMap<String, f64> = HashMap::new();

    for (sensor, tracker) in sensors.iter_mut() {
        match sensor.read_temp_c() {
            Ok(temp) if temp > 0.0 && temp <= 150.0 => {
                tracker.push(temp);
                let dt = tracker.dt_per_second();
                sensor_temps.insert(sensor.name.clone(), temp);
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
        let readings =
            linked_sensor_maxima(&fan_config.sensors, &sensor_temps, &sensor_derivatives);

        let (base_pwm, boost, target_pwm) = if let Some((max_temp, max_dt)) = readings {
            if max_dt > config.derivative.boost_threshold {
                mf.derivative_boost += max_dt * 5.0;
            } else if max_dt < 0.0 {
                let decay = config.derivative.decay_rate;
                mf.derivative_boost = (mf.derivative_boost - decay).max(0.0);
            } else {
                mf.derivative_boost =
                    (mf.derivative_boost - config.derivative.decay_rate * 0.5).max(0.0);
            }

            let base_pwm = fan_config.interpolate_pwm(max_temp);
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

        let linked_temps: HashMap<String, f64> = fan_config
            .sensors
            .iter()
            .filter_map(|s| sensor_temps.get(s).map(|&t| (s.clone(), t)))
            .collect();
        mf.analytics.record(target_pwm, linked_temps);

        if tick.is_multiple_of(ANALYTICS_RECOMPUTE_INTERVAL) {
            mf.analytics.recompute();
        }

        status_fans.push(ipc::FanStatus {
            name: fan.name.clone(),
            pwm: actual_pwm,
            rpm,
            base_pwm,
            boost_applied: boost,
        });
    }

    // Update thermal tuning system
    let fan_pwms: HashMap<String, u8> = status_fans
        .iter()
        .map(|f| (f.name.clone(), f.pwm))
        .collect();
    thermal_system.update(&sensor_temps, &fan_pwms);

    // Build IPC state
    let status_sensors: Vec<ipc::SensorStatus> = sensors
        .iter()
        .map(|(sensor, tracker)| ipc::SensorStatus {
            name: sensor.name.clone(),
            temp_c: tracker.latest_temp().unwrap_or(0.0),
            dt_per_sec: tracker.dt_per_second(),
        })
        .collect();

    let analytics_fans: Vec<ipc::FanAnalyticsReport> = fans
        .iter()
        .zip(managed_fans.iter())
        .map(|(fan, mf)| {
            let rpm = fan.read_rpm().unwrap_or(0);
            let pwm = fan.read_pwm().unwrap_or(0);
            ipc::FanAnalyticsReport {
                name: mf.analytics.name().to_string(),
                pwm,
                rpm,
                history_samples: mf.analytics.history_len(),
                effectiveness: mf
                    .analytics
                    .effectiveness()
                    .iter()
                    .map(|(k, v)| (k.clone(), *v))
                    .collect(),
            }
        })
        .collect();

    let tuning_response = ipc::TuningResponse {
        coupling_matrix: thermal_system
            .coupling_matrix()
            .into_iter()
            .map(|c| ipc::CouplingEntry {
                fan: c.fan,
                sensor: c.sensor,
                beta: c.beta,
                r_squared: c.r_squared,
            })
            .collect(),
        step_responses: thermal_system
            .step_responses()
            .into_iter()
            .map(|s| ipc::StepResponseEntry {
                fan: s.fan,
                sensor: s.sensor,
                gain_k: s.gain_k,
                tau_ticks: s.tau_ticks,
                n_events: s.n_events,
            })
            .collect(),
        cross_correlations: thermal_system
            .cross_correlations()
            .into_iter()
            .map(|c| ipc::CorrelationEntry {
                fan: c.fan,
                sensor: c.sensor,
                peak_ccf: c.peak_ccf,
                optimal_lag: c.optimal_lag,
            })
            .collect(),
        thermal_integrals: thermal_system
            .thermal_integrals()
            .into_iter()
            .map(|i| ipc::IntegralEntry {
                sensor: i.sensor,
                integral: i.integral,
                baseline: i.baseline,
            })
            .collect(),
    };

    if let Ok(mut state) = shared_state.lock() {
        state.status = ipc::StatusResponse {
            sensors: status_sensors,
            fans: status_fans,
        };
        state.analytics = ipc::AnalyticsResponse {
            fans: analytics_fans,
        };
        state.tuning = tuning_response;
    }

    Ok(())
}

fn linked_sensor_maxima(
    sensor_names: &[String],
    temperatures: &HashMap<String, f64>,
    derivatives: &HashMap<String, f64>,
) -> Option<(f64, f64)> {
    let mut max_temp = f64::NEG_INFINITY;
    let mut max_dt: f64 = 0.0;

    for name in sensor_names {
        let &temp = temperatures.get(name)?;
        let &dt = derivatives.get(name)?;
        max_temp = max_temp.max(temp);
        if dt.abs() > max_dt.abs() {
            max_dt = dt;
        }
    }

    max_temp.is_finite().then_some((max_temp, max_dt))
}

fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;

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
            linked_sensor_maxima(&names, &temperatures, &derivatives),
            None
        );
    }

    #[test]
    fn linked_sensor_maxima_selects_hottest_and_fastest() {
        let names = vec!["cpu".to_owned(), "gpu".to_owned()];
        let temperatures = HashMap::from([("cpu".to_owned(), 70.0), ("gpu".to_owned(), 80.0)]);
        let derivatives = HashMap::from([("cpu".to_owned(), -3.0), ("gpu".to_owned(), 2.0)]);

        assert_eq!(
            linked_sensor_maxima(&names, &temperatures, &derivatives),
            Some((80.0, -3.0))
        );
    }
}
