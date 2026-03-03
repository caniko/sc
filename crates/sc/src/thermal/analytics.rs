use std::collections::{HashMap, VecDeque};
use std::time::Instant;

/// A snapshot of fan state and sensor temperatures at a point in time.
#[derive(Debug, Clone)]
struct Snapshot {
    _time: Instant,
    pwm: u8,
    temps: HashMap<String, f64>,
}

/// Tracks cooling effectiveness for a single fan by correlating
/// PWM changes with temperature responses across linked sensors.
pub struct AnalyticsTracker {
    name: String,
    history: VecDeque<Snapshot>,
    max_history: usize,
    /// Computed effectiveness: sensor_name → estimated °C change per +10 PWM
    effectiveness: HashMap<String, f64>,
}

impl AnalyticsTracker {
    pub fn new(name: &str, max_history: usize) -> Self {
        Self {
            name: name.to_string(),
            history: VecDeque::with_capacity(max_history),
            max_history,
            effectiveness: HashMap::new(),
        }
    }

    /// Record a new data point.
    pub fn record(&mut self, pwm: u8, temps: HashMap<String, f64>) {
        if self.history.len() >= self.max_history {
            self.history.pop_front();
        }
        self.history.push_back(Snapshot {
            _time: Instant::now(),
            pwm,
            temps,
        });
    }

    /// Recompute effectiveness estimates from the history buffer.
    ///
    /// Uses simple linear correlation: for each sensor, compute the slope
    /// of (pwm, temp) pairs. Effectiveness = slope * 10 (°C per +10 PWM).
    pub fn recompute(&mut self) {
        if self.history.len() < 10 {
            return;
        }

        // Collect all sensor names from the latest snapshot
        let sensor_names: Vec<String> = self
            .history
            .back()
            .map(|s| s.temps.keys().cloned().collect())
            .unwrap_or_default();

        for sensor in &sensor_names {
            let pairs: Vec<(f64, f64)> = self
                .history
                .iter()
                .filter_map(|s| s.temps.get(sensor).map(|&t| (s.pwm as f64, t)))
                .collect();

            if pairs.len() < 10 {
                continue;
            }

            let slope = linear_slope(&pairs);
            // Effectiveness = slope * 10 → °C per +10 PWM
            self.effectiveness.insert(sensor.clone(), slope * 10.0);
        }
    }

    /// Get the current effectiveness estimates.
    pub fn effectiveness(&self) -> &HashMap<String, f64> {
        &self.effectiveness
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }
}

/// Compute the least-squares slope for a set of (x, y) pairs.
fn linear_slope(pairs: &[(f64, f64)]) -> f64 {
    let n = pairs.len() as f64;
    if n < 2.0 {
        return 0.0;
    }

    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xy = 0.0;
    let mut sum_xx = 0.0;

    for &(x, y) in pairs {
        sum_x += x;
        sum_y += y;
        sum_xy += x * y;
        sum_xx += x * x;
    }

    let denom = n * sum_xx - sum_x * sum_x;
    if denom.abs() < f64::EPSILON {
        return 0.0;
    }

    (n * sum_xy - sum_x * sum_y) / denom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_slope_positive() {
        let pairs: Vec<(f64, f64)> = (0..10).map(|i| (i as f64, i as f64 * 2.0)).collect();
        let slope = linear_slope(&pairs);
        assert!((slope - 2.0).abs() < 0.01);
    }

    #[test]
    fn linear_slope_negative() {
        let pairs: Vec<(f64, f64)> = (0..10).map(|i| (i as f64, 100.0 - i as f64)).collect();
        let slope = linear_slope(&pairs);
        assert!((slope - (-1.0)).abs() < 0.01);
    }

    #[test]
    fn effectiveness_needs_enough_data() {
        let mut tracker = AnalyticsTracker::new("test", 100);
        // Only 5 samples — not enough
        for i in 0..5 {
            let mut temps = HashMap::new();
            temps.insert("cpu".into(), 50.0 + i as f64);
            tracker.record(100 + i * 10, temps);
        }
        tracker.recompute();
        assert!(tracker.effectiveness().is_empty());
    }
}
