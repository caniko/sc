use std::collections::HashMap;

// ─── EWMA Online Statistics ───────────────────────────────────────────────────

/// Exponentially Weighted Moving Average with online variance tracking.
/// Smooths noisy sensor readings and provides volatility estimates.
pub struct EwmaStats {
    alpha: f64,
    mean: f64,
    variance: f64,
    initialized: bool,
}

impl EwmaStats {
    /// Create with a given span (α = 2 / (span + 1)).
    pub fn new(span: usize) -> Self {
        Self {
            alpha: 2.0 / (span as f64 + 1.0),
            mean: 0.0,
            variance: 0.0,
            initialized: false,
        }
    }

    pub fn update(&mut self, value: f64) {
        if !self.initialized {
            self.mean = value;
            self.variance = 0.0;
            self.initialized = true;
            return;
        }
        let delta = value - self.mean;
        self.mean += self.alpha * delta;
        // EWMA variance: Var_t = (1-α)(Var_{t-1} + α·δ²)
        self.variance = (1.0 - self.alpha) * (self.variance + self.alpha * delta * delta);
    }

    #[cfg(test)]
    fn smoothed(&self) -> f64 {
        self.mean
    }

    pub fn volatility(&self) -> f64 {
        self.variance.sqrt()
    }
}

// ─── Cross-Correlation Function ───────────────────────────────────────────────

/// Computes normalized cross-correlation between PWM changes and temperature
/// changes for a single (fan, sensor) pair to detect thermal propagation delay.
pub struct CrossCorrelator {
    pub fan_name: String,
    pub sensor_name: String,
    dpwm_buf: Vec<f64>,
    dtemp_buf: Vec<f64>,
    buf_size: usize,
    max_lag: usize,
    write_pos: usize,
    count: usize,
    prev_pwm: Option<f64>,
    prev_temp: Option<f64>,
    // Cached results
    peak_ccf: f64,
    optimal_lag: usize,
}

impl CrossCorrelator {
    pub fn new(fan_name: &str, sensor_name: &str, buf_size: usize, max_lag: usize) -> Self {
        Self {
            fan_name: fan_name.to_string(),
            sensor_name: sensor_name.to_string(),
            dpwm_buf: vec![0.0; buf_size],
            dtemp_buf: vec![0.0; buf_size],
            buf_size,
            max_lag: max_lag.min(buf_size / 2),
            write_pos: 0,
            count: 0,
            prev_pwm: None,
            prev_temp: None,
            peak_ccf: 0.0,
            optimal_lag: 0,
        }
    }

    pub fn update(&mut self, pwm: f64, temp: f64) {
        if let (Some(pp), Some(pt)) = (self.prev_pwm, self.prev_temp) {
            self.dpwm_buf[self.write_pos] = pwm - pp;
            self.dtemp_buf[self.write_pos] = temp - pt;
            self.write_pos = (self.write_pos + 1) % self.buf_size;
            self.count = (self.count + 1).min(self.buf_size);
        }
        self.prev_pwm = Some(pwm);
        self.prev_temp = Some(temp);
    }

    /// Recompute the CCF and find optimal lag. Call periodically, not every tick.
    pub fn recompute(&mut self) {
        let n = self.count;
        if n < self.max_lag + 10 {
            return;
        }

        // Compute means and stddevs of the filled portion
        let (mean_p, std_p) = mean_std(&self.dpwm_buf, n, self.write_pos, self.buf_size);
        let (mean_t, std_t) = mean_std(&self.dtemp_buf, n, self.write_pos, self.buf_size);

        if std_p < 1e-9 || std_t < 1e-9 {
            return;
        }

        let mut best_ccf = 0.0_f64;
        let mut best_lag = 0;

        for lag in 0..=self.max_lag {
            let mut sum = 0.0;
            let effective_n = n - lag;
            for i in 0..effective_n {
                let idx_p = ring_idx(self.write_pos, n, i, self.buf_size);
                let idx_t = ring_idx(self.write_pos, n, i + lag, self.buf_size);
                sum += (self.dpwm_buf[idx_p] - mean_p) * (self.dtemp_buf[idx_t] - mean_t);
            }
            let ccf = sum / (effective_n as f64 * std_p * std_t);
            if ccf.abs() > best_ccf.abs() {
                best_ccf = ccf;
                best_lag = lag;
            }
        }

        self.peak_ccf = best_ccf;
        self.optimal_lag = best_lag;
    }

    pub fn peak_ccf(&self) -> f64 {
        self.peak_ccf
    }

    pub fn optimal_lag(&self) -> usize {
        self.optimal_lag
    }
}

/// Helper: mean and stddev from a ring buffer with `count` valid entries.
fn mean_std(buf: &[f64], count: usize, write_pos: usize, capacity: usize) -> (f64, f64) {
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    for i in 0..count {
        let idx = ring_idx(write_pos, count, i, capacity);
        sum += buf[idx];
        sum_sq += buf[idx] * buf[idx];
    }
    let mean = sum / count as f64;
    let var = (sum_sq / count as f64) - mean * mean;
    (mean, var.max(0.0).sqrt())
}

/// Map logical index i (0 = oldest) to physical ring buffer index.
fn ring_idx(write_pos: usize, count: usize, i: usize, capacity: usize) -> usize {
    // write_pos points to next write slot; oldest = write_pos - count
    (write_pos + capacity - count + i) % capacity
}

// ─── Step Response Detector ───────────────────────────────────────────────────

/// Detects PWM step changes and fits first-order exponential response.
pub struct StepDetector {
    step_threshold: f64,
    response_window: usize,
    /// Per-(fan, sensor) pair: list of completed step events
    completed: HashMap<(String, String), Vec<FittedStep>>,
    /// Active tracking: per-fan, current step event being recorded per sensor
    active: HashMap<String, ActiveStep>,
}

struct ActiveStep {
    delta_pwm: f64,
    tick: usize,
    initial_temps: HashMap<String, f64>,
    trajectories: HashMap<String, Vec<f64>>,
}

/// Result of fitting a first-order exponential to a step response.
#[derive(Debug, Clone)]
pub struct FittedStep {
    pub gain_k: f64,    // (T_∞ - T_0) / ΔPWM — °C per unit PWM
    pub tau_ticks: f64, // time constant in ticks (63% of response)
}

impl StepDetector {
    pub fn new(step_threshold: u8, response_window: usize) -> Self {
        Self {
            step_threshold: step_threshold as f64,
            response_window,
            completed: HashMap::new(),
            active: HashMap::new(),
        }
    }

    /// Called each tick. Pass previous and current PWM for each fan, plus sensor temps.
    pub fn update(
        &mut self,
        prev_pwms: &HashMap<String, u8>,
        curr_pwms: &HashMap<String, u8>,
        sensor_temps: &HashMap<String, f64>,
    ) {
        // Check for new step events
        for (fan, &curr) in curr_pwms {
            if let Some(&prev) = prev_pwms.get(fan) {
                let delta = curr as f64 - prev as f64;
                if delta.abs() >= self.step_threshold {
                    // Start tracking a new step event for this fan
                    self.active.insert(
                        fan.clone(),
                        ActiveStep {
                            delta_pwm: delta,
                            tick: 0,
                            initial_temps: sensor_temps.clone(),
                            trajectories: sensor_temps
                                .keys()
                                .map(|s| (s.clone(), vec![*sensor_temps.get(s).unwrap_or(&0.0)]))
                                .collect(),
                        },
                    );
                }
            }
        }

        // Advance active step recordings
        let mut finished = Vec::new();
        for (fan, step) in &mut self.active {
            step.tick += 1;
            for (sensor, temps) in &mut step.trajectories {
                if let Some(&t) = sensor_temps.get(sensor) {
                    temps.push(t);
                }
            }
            if step.tick >= self.response_window {
                finished.push(fan.clone());
            }
        }

        // Fit completed step events
        for fan in finished {
            if let Some(step) = self.active.remove(&fan) {
                for (sensor, trajectory) in &step.trajectories {
                    if trajectory.len() < 5 {
                        continue;
                    }
                    let t0 = step
                        .initial_temps
                        .get(sensor)
                        .copied()
                        .unwrap_or(trajectory[0]);
                    if let Some(fitted) = fit_first_order(t0, trajectory, step.delta_pwm) {
                        self.completed
                            .entry((fan.clone(), sensor.clone()))
                            .or_default()
                            .push(fitted);
                    }
                }
            }
        }
    }

    /// Get averaged step response parameters for each (fan, sensor) pair.
    pub fn results(&self) -> Vec<StepResponseResult> {
        self.completed
            .iter()
            .filter(|(_, fits)| !fits.is_empty())
            .map(|((fan, sensor), fits)| {
                let n = fits.len() as f64;
                let avg_k = fits.iter().map(|f| f.gain_k).sum::<f64>() / n;
                let avg_tau = fits.iter().map(|f| f.tau_ticks).sum::<f64>() / n;
                StepResponseResult {
                    fan: fan.clone(),
                    sensor: sensor.clone(),
                    gain_k: avg_k,
                    tau_ticks: avg_tau,
                    n_events: fits.len(),
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct StepResponseResult {
    pub fan: String,
    pub sensor: String,
    pub gain_k: f64,
    pub tau_ticks: f64,
    pub n_events: usize,
}

/// Fit T(t) = T_∞ + (T_0 - T_∞) · e^(-t/τ) to a trajectory.
/// Returns gain K = (T_∞ - T_0) / delta_pwm and τ.
/// Uses iterative least squares (simplified Gauss-Newton on τ).
fn fit_first_order(t0_temp: f64, trajectory: &[f64], delta_pwm: f64) -> Option<FittedStep> {
    if trajectory.len() < 5 || delta_pwm.abs() < 1.0 {
        return None;
    }

    let n = trajectory.len();
    // Estimate T_∞ as the mean of the last 20% of trajectory
    let tail_start = n * 4 / 5;
    let t_inf: f64 = trajectory[tail_start..].iter().sum::<f64>() / (n - tail_start) as f64;

    let delta_t = t_inf - t0_temp;
    if delta_t.abs() < 0.05 {
        // No significant temperature change
        return None;
    }

    // Search for τ that minimizes SSE using golden section search
    let mut tau_lo = 0.5_f64;
    let mut tau_hi = (n as f64) * 2.0;

    for _ in 0..50 {
        let tau_a = tau_lo + 0.382 * (tau_hi - tau_lo);
        let tau_b = tau_lo + 0.618 * (tau_hi - tau_lo);
        let sse_a = step_sse(t0_temp, t_inf, tau_a, trajectory);
        let sse_b = step_sse(t0_temp, t_inf, tau_b, trajectory);
        if sse_a < sse_b {
            tau_hi = tau_b;
        } else {
            tau_lo = tau_a;
        }
        if (tau_hi - tau_lo) < 0.01 {
            break;
        }
    }

    let tau = (tau_lo + tau_hi) / 2.0;
    let gain_k = delta_t / delta_pwm;

    Some(FittedStep {
        gain_k,
        tau_ticks: tau,
    })
}

/// Sum of squared errors for the first-order model.
fn step_sse(t0: f64, t_inf: f64, tau: f64, trajectory: &[f64]) -> f64 {
    trajectory
        .iter()
        .enumerate()
        .map(|(i, &actual)| {
            let predicted = t_inf + (t0 - t_inf) * (-(i as f64) / tau).exp();
            let err = actual - predicted;
            err * err
        })
        .sum()
}

// ─── Multivariate OLS Regression (Coupling Estimator) ─────────────────────────

/// Estimates the thermal coupling matrix β[sensor][fan] via online OLS.
/// Model: ΔT_sensor = Σ_fans β_fan · ΔPWM_fan + ε
///
/// Accumulates XᵀX and Xᵀy online, solves normal equations periodically.
pub struct CouplingEstimator {
    fan_names: Vec<String>,
    sensor_names: Vec<String>,
    n_fans: usize,
    /// Per-sensor: accumulated XᵀX (n_fans × n_fans, row-major)
    xtx: Vec<Vec<f64>>,
    /// Per-sensor: accumulated Xᵀy (n_fans)
    xty: Vec<Vec<f64>>,
    /// Sample count per sensor
    sample_count: Vec<usize>,
    min_samples: usize,
    /// Solved coupling coefficients: [sensor_idx][fan_idx]
    betas: Vec<Vec<f64>>,
    /// R² per sensor
    r_squared: Vec<f64>,
    /// Previous values for differencing
    prev_pwms: HashMap<String, f64>,
    prev_temps: HashMap<String, f64>,
    /// Per-sensor: sum of (ΔT)² for R² calculation
    ssy: Vec<f64>,
    /// Per-sensor: sum of residuals² for R² calculation
    sse: Vec<f64>,
}

impl CouplingEstimator {
    pub fn new(fan_names: &[String], sensor_names: &[String], min_samples: usize) -> Self {
        let nf = fan_names.len();
        let ns = sensor_names.len();
        Self {
            fan_names: fan_names.to_vec(),
            sensor_names: sensor_names.to_vec(),
            n_fans: nf,
            xtx: vec![vec![0.0; nf * nf]; ns],
            xty: vec![vec![0.0; nf]; ns],
            sample_count: vec![0; ns],
            min_samples,
            betas: vec![vec![0.0; nf]; ns],
            r_squared: vec![0.0; ns],
            prev_pwms: HashMap::new(),
            prev_temps: HashMap::new(),
            ssy: vec![0.0; ns],
            sse: vec![0.0; ns],
        }
    }

    /// Accumulate one observation.
    pub fn update(&mut self, fan_pwms: &HashMap<String, u8>, sensor_temps: &HashMap<String, f64>) {
        if self.prev_pwms.is_empty() {
            // First call — store and return
            for (name, &val) in fan_pwms {
                self.prev_pwms.insert(name.clone(), val as f64);
            }
            for (name, &val) in sensor_temps {
                self.prev_temps.insert(name.clone(), val);
            }
            return;
        }

        // Compute deltas
        let dpwm: Vec<f64> = self
            .fan_names
            .iter()
            .map(|f| {
                let curr = fan_pwms.get(f).map(|&v| v as f64).unwrap_or(0.0);
                let prev = self.prev_pwms.get(f).copied().unwrap_or(0.0);
                curr - prev
            })
            .collect();

        for (si, sensor) in self.sensor_names.iter().enumerate() {
            let curr_t = sensor_temps.get(sensor).copied().unwrap_or(0.0);
            let prev_t = self.prev_temps.get(sensor).copied().unwrap_or(0.0);
            let dt = curr_t - prev_t;

            // Accumulate XᵀX
            for i in 0..self.n_fans {
                for j in 0..self.n_fans {
                    self.xtx[si][i * self.n_fans + j] += dpwm[i] * dpwm[j];
                }
                // Accumulate Xᵀy
                self.xty[si][i] += dpwm[i] * dt;
            }
            self.ssy[si] += dt * dt;
            self.sample_count[si] += 1;
        }

        // Update previous values
        for (name, &val) in fan_pwms {
            self.prev_pwms.insert(name.clone(), val as f64);
        }
        for (name, &val) in sensor_temps {
            self.prev_temps.insert(name.clone(), val);
        }
    }

    /// Solve normal equations for all sensors. Call periodically.
    pub fn solve(&mut self) {
        for si in 0..self.sensor_names.len() {
            if self.sample_count[si] < self.min_samples {
                continue;
            }

            // Solve XᵀX · β = Xᵀy via Cholesky decomposition
            if let Some(beta) = cholesky_solve(&self.xtx[si], &self.xty[si], self.n_fans) {
                // Compute R²: 1 - SSE/SSY
                // SSE = SSY - βᵀ·Xᵀy
                let beta_xty: f64 = beta
                    .iter()
                    .zip(self.xty[si].iter())
                    .map(|(b, xy)| b * xy)
                    .sum();
                let sse = (self.ssy[si] - beta_xty).max(0.0);
                let r_sq = if self.ssy[si] > 1e-12 {
                    1.0 - sse / self.ssy[si]
                } else {
                    0.0
                };

                self.betas[si] = beta;
                self.r_squared[si] = r_sq;
                self.sse[si] = sse;
            }
        }
    }

    /// Get the coupling matrix as a flat list of entries.
    pub fn coupling_matrix(&self) -> Vec<CouplingResult> {
        let mut results = Vec::new();
        for (si, sensor) in self.sensor_names.iter().enumerate() {
            if self.sample_count[si] < self.min_samples {
                continue;
            }
            for (fi, fan) in self.fan_names.iter().enumerate() {
                results.push(CouplingResult {
                    fan: fan.clone(),
                    sensor: sensor.clone(),
                    // Scale to °C per +10 PWM for readability
                    beta: self.betas[si][fi] * 10.0,
                    r_squared: self.r_squared[si],
                });
            }
        }
        results
    }
}

#[derive(Debug, Clone)]
pub struct CouplingResult {
    pub fan: String,
    pub sensor: String,
    pub beta: f64,
    pub r_squared: f64,
}

/// Solve Ax = b via Cholesky decomposition (A must be symmetric positive semi-definite).
/// Returns None if decomposition fails (singular/near-singular matrix).
fn cholesky_solve(a_flat: &[f64], b: &[f64], n: usize) -> Option<Vec<f64>> {
    // Regularize: add small diagonal to handle collinearity
    let mut a = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in 0..n {
            a[i][j] = a_flat[i * n + j];
        }
        a[i][i] += 1e-8; // Tikhonov regularization
    }

    // Cholesky: A = LLᵀ
    let mut l = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in 0..=i {
            let sum: f64 = l[i][..j]
                .iter()
                .zip(l[j][..j].iter())
                .map(|(left, right)| left * right)
                .sum();
            if i == j {
                let diag = a[i][i] - sum;
                if diag <= 0.0 {
                    return None;
                }
                l[i][j] = diag.sqrt();
            } else {
                if l[j][j].abs() < 1e-15 {
                    return None;
                }
                l[i][j] = (a[i][j] - sum) / l[j][j];
            }
        }
    }

    // Forward substitution: Ly = b
    let mut y = vec![0.0; n];
    for i in 0..n {
        let mut sum = 0.0;
        for j in 0..i {
            sum += l[i][j] * y[j];
        }
        y[i] = (b[i] - sum) / l[i][i];
    }

    // Back substitution: Lᵀx = y
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let mut sum = 0.0;
        for j in (i + 1)..n {
            sum += l[j][i] * x[j]; // Lᵀ[i][j] = L[j][i]
        }
        x[i] = (y[i] - sum) / l[i][i];
    }

    Some(x)
}

// ─── Thermal Integral Tracker ─────────────────────────────────────────────────

/// Integrates temperature above a baseline using trapezoidal rule.
/// Provides cumulative "thermal energy" metric.
pub struct ThermalIntegral {
    baseline: f64,
    integral: f64,
    prev_excess: Option<f64>,
    cooldown_ticks: usize,
    cooldown_threshold: usize,
}

impl ThermalIntegral {
    pub fn new(baseline: f64, cooldown_threshold: usize) -> Self {
        Self {
            baseline,
            integral: 0.0,
            prev_excess: None,
            cooldown_ticks: 0,
            cooldown_threshold,
        }
    }

    /// Update with a new temperature reading. dt = time between samples.
    pub fn update(&mut self, temp: f64, dt_seconds: f64) {
        let excess = (temp - self.baseline).max(0.0);

        if let Some(prev) = self.prev_excess {
            // Trapezoidal rule: ∫ ≈ (f(t) + f(t+dt)) / 2 · dt
            self.integral += (prev + excess) / 2.0 * dt_seconds;
        }

        // Cooldown: reset integral if below baseline for long enough
        if excess <= 0.0 {
            self.cooldown_ticks += 1;
            if self.cooldown_ticks >= self.cooldown_threshold {
                self.integral = 0.0;
            }
        } else {
            self.cooldown_ticks = 0;
        }

        self.prev_excess = Some(excess);
    }

    pub fn integral(&self) -> f64 {
        self.integral
    }

    pub fn baseline(&self) -> f64 {
        self.baseline
    }
}

// ─── ThermalSystem Orchestrator ───────────────────────────────────────────────

/// Configuration for the tuning subsystem.
pub struct TuningParams {
    pub ewma_span: usize,
    pub ccf_buffer_size: usize,
    pub ccf_max_lag: usize,
    pub step_threshold: u8,
    pub response_window: usize,
    pub regression_min_samples: usize,
    pub poll_interval_secs: f64,
}

/// Aggregates all thermal tuning subsystems. Updated each tick from the controller.
pub struct ThermalSystem {
    ewma: HashMap<String, EwmaStats>,
    correlators: Vec<CrossCorrelator>,
    step_detector: StepDetector,
    coupling: CouplingEstimator,
    integrals: HashMap<String, ThermalIntegral>,
    poll_interval_secs: f64,
    prev_pwms: HashMap<String, u8>,
    tick: u32,
}

impl ThermalSystem {
    pub fn new(params: &TuningParams, fan_names: &[String], sensor_names: &[String]) -> Self {
        let mut ewma = HashMap::new();
        let mut integrals = HashMap::new();
        for s in sensor_names {
            ewma.insert(s.clone(), EwmaStats::new(params.ewma_span));
            // Default baseline: 40°C (reasonable for desktop motherboard sensors)
            integrals.insert(s.clone(), ThermalIntegral::new(40.0, 30));
        }

        let mut correlators = Vec::new();
        for fan in fan_names {
            for sensor in sensor_names {
                correlators.push(CrossCorrelator::new(
                    fan,
                    sensor,
                    params.ccf_buffer_size,
                    params.ccf_max_lag,
                ));
            }
        }

        let step_detector = StepDetector::new(params.step_threshold, params.response_window);

        let coupling =
            CouplingEstimator::new(fan_names, sensor_names, params.regression_min_samples);

        Self {
            ewma,
            correlators,
            step_detector,
            coupling,
            integrals,
            poll_interval_secs: params.poll_interval_secs,
            prev_pwms: HashMap::new(),
            tick: 0,
        }
    }

    /// Called each tick from the controller.
    pub fn update(&mut self, sensor_temps: &HashMap<String, f64>, fan_pwms: &HashMap<String, u8>) {
        // 1. Update EWMA smoothers
        for (name, &temp) in sensor_temps {
            if let Some(ewma) = self.ewma.get_mut(name) {
                ewma.update(temp);
            }
        }

        // 2. Update cross-correlators
        for corr in &mut self.correlators {
            let pwm = fan_pwms
                .get(&corr.fan_name)
                .map(|&v| v as f64)
                .unwrap_or(0.0);
            let temp = sensor_temps.get(&corr.sensor_name).copied().unwrap_or(0.0);
            corr.update(pwm, temp);
        }

        // 3. Update step detector
        self.step_detector
            .update(&self.prev_pwms, fan_pwms, sensor_temps);

        // 4. Update coupling estimator (OLS)
        self.coupling.update(fan_pwms, sensor_temps);

        // 5. Update thermal integrals
        for (name, &temp) in sensor_temps {
            if let Some(integral) = self.integrals.get_mut(name) {
                integral.update(temp, self.poll_interval_secs);
            }
        }

        // Periodic recomputation (every 30 ticks)
        if self.tick > 0 && self.tick.is_multiple_of(30) {
            for corr in &mut self.correlators {
                corr.recompute();
            }
            self.coupling.solve();
        }

        self.prev_pwms = fan_pwms.clone();
        self.tick += 1;
    }
    /// Get volatility for a sensor.
    #[allow(dead_code)]
    pub fn volatility(&self, sensor: &str) -> Option<f64> {
        self.ewma.get(sensor).map(|e| e.volatility())
    }

    /// Get the full coupling matrix.
    pub fn coupling_matrix(&self) -> Vec<CouplingResult> {
        self.coupling.coupling_matrix()
    }

    /// Get step response results.
    pub fn step_responses(&self) -> Vec<StepResponseResult> {
        self.step_detector.results()
    }

    /// Get cross-correlation results.
    pub fn cross_correlations(&self) -> Vec<CorrelationResult> {
        self.correlators
            .iter()
            .filter(|c| c.peak_ccf().abs() > 0.01)
            .map(|c| CorrelationResult {
                fan: c.fan_name.clone(),
                sensor: c.sensor_name.clone(),
                peak_ccf: c.peak_ccf(),
                optimal_lag: c.optimal_lag(),
            })
            .collect()
    }

    /// Get thermal integrals.
    pub fn thermal_integrals(&self) -> Vec<IntegralResult> {
        self.integrals
            .iter()
            .map(|(name, ti)| IntegralResult {
                sensor: name.clone(),
                integral: ti.integral(),
                baseline: ti.baseline(),
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct CorrelationResult {
    pub fan: String,
    pub sensor: String,
    pub peak_ccf: f64,
    pub optimal_lag: usize,
}

#[derive(Debug, Clone)]
pub struct IntegralResult {
    pub sensor: String,
    pub integral: f64,
    pub baseline: f64,
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewma_converges_to_constant() {
        let mut ewma = EwmaStats::new(10);
        for _ in 0..100 {
            ewma.update(50.0);
        }
        assert!((ewma.smoothed() - 50.0).abs() < 0.01);
        assert!(ewma.volatility() < 0.01);
    }

    #[test]
    fn ewma_tracks_step_change() {
        let mut ewma = EwmaStats::new(5);
        for _ in 0..50 {
            ewma.update(50.0);
        }
        for _ in 0..50 {
            ewma.update(60.0);
        }
        // After 50 samples at α=2/6≈0.33, should be very close to 60
        assert!((ewma.smoothed() - 60.0).abs() < 0.1);
    }

    #[test]
    fn ccf_detects_zero_lag_correlation() {
        let mut corr = CrossCorrelator::new("fan", "sensor", 200, 10);
        // PWM goes up, temp goes up simultaneously (lag 0)
        for i in 0..150 {
            let pwm = 100.0 + (i as f64 * 0.1).sin() * 30.0;
            let temp = 50.0 + (i as f64 * 0.1).sin() * 5.0; // same phase
            corr.update(pwm, temp);
        }
        corr.recompute();
        // Should find lag near 0 with positive correlation
        assert!(corr.peak_ccf() > 0.3, "peak_ccf={}", corr.peak_ccf());
        assert!(corr.optimal_lag() <= 2, "lag={}", corr.optimal_lag());
    }

    #[test]
    fn step_sse_zero_for_perfect_fit() {
        // T(t) = 60 + (50 - 60) * exp(-t/5) = 60 - 10*exp(-t/5)
        let trajectory: Vec<f64> = (0..30)
            .map(|i| 60.0 + (50.0 - 60.0) * (-(i as f64) / 5.0).exp())
            .collect();
        let sse = step_sse(50.0, 60.0, 5.0, &trajectory);
        assert!(sse < 1e-10, "sse={}", sse);
    }

    #[test]
    fn fit_first_order_recovers_tau() {
        // Generate trajectory with known parameters: T_0=50, T_∞=60, τ=10
        let trajectory: Vec<f64> = (0..60)
            .map(|i| 60.0 + (50.0 - 60.0) * (-(i as f64) / 10.0).exp())
            .collect();
        let fitted = fit_first_order(50.0, &trajectory, 20.0).unwrap();
        assert!(
            (fitted.tau_ticks - 10.0).abs() < 1.0,
            "tau={:.2}",
            fitted.tau_ticks
        );
        assert!(
            (fitted.gain_k - 0.5).abs() < 0.1,
            "gain_k={:.3}",
            fitted.gain_k
        );
    }

    #[test]
    fn cholesky_solve_2x2() {
        // A = [[4, 2], [2, 3]], b = [8, 7]
        // Solution: x = [1, 2] (verify: 4*1+2*2=8, 2*1+3*2=8... wait)
        // Actually: 2*1+3*2=8, not 7. Let me pick correct values.
        // A = [[2, 1], [1, 2]], b = [4, 5] → x = [1, 2]
        // Check: 2*1+1*2=4 ✓, 1*1+2*2=5 ✓
        let a = vec![2.0, 1.0, 1.0, 2.0];
        let b = vec![4.0, 5.0];
        let x = cholesky_solve(&a, &b, 2).unwrap();
        assert!((x[0] - 1.0).abs() < 0.01, "x[0]={}", x[0]);
        assert!((x[1] - 2.0).abs() < 0.01, "x[1]={}", x[1]);
    }

    #[test]
    fn coupling_estimator_recovers_coefficients() {
        let fans = vec!["fan_a".to_string(), "fan_b".to_string()];
        let sensors = vec!["sensor".to_string()];
        let mut est = CouplingEstimator::new(&fans, &sensors, 10);

        // Simulate: ΔT = -0.5 * ΔPWM_a + -0.3 * ΔPWM_b
        let mut pwm_a: u8 = 100;
        let mut pwm_b: u8 = 100;
        let mut temp = 60.0;

        for i in 0..200 {
            let da = if i % 3 == 0 { 5i16 } else { -2 };
            let db = if i % 5 == 0 { 3i16 } else { -1 };
            let new_a = (pwm_a as i16 + da).clamp(30, 230) as u8;
            let new_b = (pwm_b as i16 + db).clamp(30, 230) as u8;
            let dt = -0.05 * (new_a as f64 - pwm_a as f64) + -0.03 * (new_b as f64 - pwm_b as f64);
            temp += dt;
            pwm_a = new_a;
            pwm_b = new_b;

            let mut fan_map = HashMap::new();
            fan_map.insert("fan_a".to_string(), pwm_a);
            fan_map.insert("fan_b".to_string(), pwm_b);
            let mut sensor_map = HashMap::new();
            sensor_map.insert("sensor".to_string(), temp);
            est.update(&fan_map, &sensor_map);
        }

        est.solve();
        let matrix = est.coupling_matrix();
        // beta is scaled by 10 (per +10 PWM), so expect ~-0.5 and ~-0.3
        let beta_a = matrix.iter().find(|c| c.fan == "fan_a").map(|c| c.beta);
        let beta_b = matrix.iter().find(|c| c.fan == "fan_b").map(|c| c.beta);
        assert!(beta_a.is_some(), "fan_a coupling not found");
        assert!(beta_b.is_some(), "fan_b coupling not found");
        let ba = beta_a.unwrap();
        let bb = beta_b.unwrap();
        assert!(
            (ba - (-0.5)).abs() < 0.15,
            "fan_a beta={:.3}, expected ~-0.5",
            ba
        );
        assert!(
            (bb - (-0.3)).abs() < 0.15,
            "fan_b beta={:.3}, expected ~-0.3",
            bb
        );
    }

    #[test]
    fn thermal_integral_accumulates() {
        let mut ti = ThermalIntegral::new(40.0, 100);
        // 10 seconds at 50°C (excess = 10°C)
        for _ in 0..10 {
            ti.update(50.0, 1.0);
        }
        // Integral should be ~100 °C·s (10°C × 10s, trapezoidal)
        // First sample has no prev so contributes 0, then 9 intervals of 10°C·1s = 90
        assert!(
            (ti.integral() - 90.0).abs() < 1.0,
            "integral={}",
            ti.integral()
        );
    }

    #[test]
    fn thermal_integral_ignores_below_baseline() {
        let mut ti = ThermalIntegral::new(50.0, 100);
        for _ in 0..20 {
            ti.update(40.0, 1.0); // Below baseline
        }
        assert!(ti.integral() < 0.01, "integral={}", ti.integral());
    }

    #[test]
    fn thermal_integral_resets_on_cooldown() {
        let mut ti = ThermalIntegral::new(40.0, 5);
        // Build up some integral
        for _ in 0..10 {
            ti.update(60.0, 1.0);
        }
        assert!(ti.integral() > 0.0);
        // Cool down below baseline for 5+ ticks
        for _ in 0..10 {
            ti.update(30.0, 1.0);
        }
        assert!(ti.integral() < 0.01, "integral={}", ti.integral());
    }
}
