use std::collections::HashMap;

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

/// Aggregates the daemon's thermal tracking. Updated each tick from the controller.
pub struct ThermalSystem {
    integrals: HashMap<String, ThermalIntegral>,
    poll_interval_secs: f64,
}

impl ThermalSystem {
    pub fn new(poll_interval_secs: f64, sensor_names: &[String]) -> Self {
        let integrals = sensor_names
            .iter()
            .map(|s| {
                // Default baseline: 40°C (reasonable for desktop motherboard sensors)
                (s.clone(), ThermalIntegral::new(40.0, 30))
            })
            .collect();

        Self {
            integrals,
            poll_interval_secs,
        }
    }

    /// Called each tick from the controller.
    pub fn update(&mut self, sensor_temps: &HashMap<String, f64>) {
        for (name, &temp) in sensor_temps {
            if let Some(integral) = self.integrals.get_mut(name) {
                integral.update(temp, self.poll_interval_secs);
            }
        }
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
