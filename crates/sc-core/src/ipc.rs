use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

pub const SOCKET_PATH: &str = "/run/smartcool/sc.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    Status,
    Analytics,
    Tuning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Status(StatusResponse),
    Analytics(AnalyticsResponse),
    Tuning(TuningResponse),
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub sensors: Vec<SensorStatus>,
    pub fans: Vec<FanStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorStatus {
    pub name: String,
    pub temp_c: f64,
    pub dt_per_sec: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanStatus {
    pub name: String,
    pub pwm: u8,
    pub rpm: u32,
    pub base_pwm: u8,
    pub boost_applied: i16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsResponse {
    pub fans: Vec<FanAnalyticsReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanAnalyticsReport {
    pub name: String,
    pub pwm: u8,
    pub rpm: u32,
    pub history_samples: usize,
    /// Per-sensor effectiveness: (sensor_name, °C change per +10 PWM)
    pub effectiveness: Vec<(String, f64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningResponse {
    pub coupling_matrix: Vec<CouplingEntry>,
    pub step_responses: Vec<StepResponseEntry>,
    pub cross_correlations: Vec<CorrelationEntry>,
    pub thermal_integrals: Vec<IntegralEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouplingEntry {
    pub fan: String,
    pub sensor: String,
    /// °C change per +10 PWM (from multivariate OLS)
    pub beta: f64,
    pub r_squared: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResponseEntry {
    pub fan: String,
    pub sensor: String,
    /// Static gain: °C per unit PWM
    pub gain_k: f64,
    /// Time constant in ticks (63% of response)
    pub tau_ticks: f64,
    pub n_events: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationEntry {
    pub fan: String,
    pub sensor: String,
    /// Peak normalized cross-correlation coefficient
    pub peak_ccf: f64,
    /// Lag in ticks at peak correlation
    pub optimal_lag: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegralEntry {
    pub sensor: String,
    /// Cumulative thermal energy (°C·s above baseline)
    pub integral: f64,
    pub baseline: f64,
}

/// Send a request to the daemon and return the response.
pub fn query(request: Request) -> Result<Response> {
    let mut stream = UnixStream::connect(SOCKET_PATH)
        .context("failed to connect to smartcool daemon (is it running?)")?;

    let mut request_json = serde_json::to_string(&request)?;
    request_json.push('\n');
    stream.write_all(request_json.as_bytes())?;
    stream.flush()?;

    let reader = BufReader::new(&stream);
    let line = reader
        .lines()
        .next()
        .context("no response from daemon")?
        .context("failed to read response")?;

    let response: Response = serde_json::from_str(&line)?;
    Ok(response)
}
