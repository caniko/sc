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
use sc_hwmon::{fan::Fan, sensor::Sensor};

/// Number of analytics history snapshots to retain per fan.
const ANALYTICS_HISTORY_SIZE: usize = 300;
/// Recompute analytics every N ticks.
const ANALYTICS_RECOMPUTE_INTERVAL: u32 = 30;

/// Resolved runtime state for a single managed fan.
struct ManagedFan {
    fan: Fan,
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

    // Resolve fans and set to manual mode
    let mut managed_fans: Vec<ManagedFan> = Vec::new();
    for (i, fc) in config.fans.iter().enumerate() {
        let fan = Fan::from_config(&fc.name, &fc.hwmon, fc.pwm_index, fc.hwmon_instance)
            .with_context(|| format!("failed to initialize fan '{}'", fc.name))?;
        fan.set_manual()
            .with_context(|| format!("failed to set fan '{}' to manual mode", fc.name))?;
        tracing::info!(
            fan = %fc.name,
            hwmon = %fc.hwmon,
            pwm_index = fc.pwm_index,
            "initialized fan (manual mode)"
        );
        managed_fans.push(ManagedFan {
            fan,
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

    // Notify systemd we're ready
    let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

    let poll_interval = Duration::from_millis(config.poll_interval_ms);
    let mut tick: u32 = 0;

    tracing::info!(
        poll_ms = config.poll_interval_ms,
        sensors = sensors.len(),
        fans = managed_fans.len(),
        "daemon ready"
    );

    // Main control loop
    while !shutdown.load(Ordering::Relaxed) {
        let tick_start = Instant::now();

        if let Err(e) = control_tick(
            &config,
            &mut sensors,
            &mut managed_fans,
            &mut thermal_system,
            &shared_state,
            tick,
        ) {
            tracing::error!(error = %e, "control tick failed");
        }

        tick = tick.wrapping_add(1);

        let elapsed = tick_start.elapsed();
        if elapsed < poll_interval {
            std::thread::sleep(poll_interval - elapsed);
        }
    }

    tracing::info!("shutting down, restoring fan control modes");

    for mf in &managed_fans {
        if let Err(e) = mf.fan.restore_original() {
            tracing::error!(fan = %mf.fan.name, error = %e, "failed to restore fan mode");
        }
    }

    let _ = std::fs::remove_file(ipc::SOCKET_PATH);

    tracing::info!("shutdown complete");
    Ok(())
}

fn control_tick(
    config: &Config,
    sensors: &mut [(Sensor, DerivativeTracker)],
    managed_fans: &mut [ManagedFan],
    thermal_system: &mut ThermalSystem,
    shared_state: &Arc<Mutex<crate::ipc_server::DaemonState>>,
    tick: u32,
) -> Result<()> {
    let mut sensor_temps: HashMap<String, f64> = HashMap::new();
    let mut sensor_derivatives: HashMap<String, f64> = HashMap::new();

    for (sensor, tracker) in sensors.iter_mut() {
        match sensor.read_temp_c() {
            Ok(temp) => {
                tracker.push(temp);
                let dt = tracker.dt_per_second();
                sensor_temps.insert(sensor.name.clone(), temp);
                sensor_derivatives.insert(sensor.name.clone(), dt);
            }
            Err(e) => {
                tracing::warn!(sensor = %sensor.name, error = %e, "failed to read sensor");
            }
        }
    }

    let mut status_fans = Vec::new();

    for mf in managed_fans.iter_mut() {
        let fan_config = &config.fans[mf.config_idx];

        let mut max_temp: f64 = 0.0;
        let mut max_dt: f64 = 0.0;

        for sensor_name in &fan_config.sensors {
            if let Some(&temp) = sensor_temps.get(sensor_name) {
                max_temp = max_temp.max(temp);
            }
            if let Some(&dt) = sensor_derivatives.get(sensor_name) {
                if dt.abs() > max_dt.abs() {
                    max_dt = dt;
                }
            }
        }

        let base_pwm = fan_config.interpolate_pwm(max_temp);

        if max_dt > config.derivative.boost_threshold {
            mf.derivative_boost += max_dt * 5.0;
        } else if max_dt < 0.0 {
            let decay = config.derivative.decay_rate;
            mf.derivative_boost = (mf.derivative_boost - decay).max(0.0);
        } else {
            mf.derivative_boost =
                (mf.derivative_boost - config.derivative.decay_rate * 0.5).max(0.0);
        }

        let boost = mf.derivative_boost.round() as i16;
        let target_pwm = (base_pwm as i16 + boost).clamp(0, 255) as u8;

        if let Err(e) = mf.fan.write_pwm(target_pwm) {
            tracing::warn!(fan = %mf.fan.name, error = %e, "failed to write PWM");
        }

        let rpm = mf.fan.read_rpm().unwrap_or(0);

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
            name: mf.fan.name.clone(),
            pwm: target_pwm,
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

    let analytics_fans: Vec<ipc::FanAnalyticsReport> = managed_fans
        .iter()
        .map(|mf| {
            let rpm = mf.fan.read_rpm().unwrap_or(0);
            let pwm = mf.fan.read_pwm().unwrap_or(0);
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
