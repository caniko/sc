use std::collections::VecDeque;
use std::time::Instant;

/// Tracks temperature readings over a rolling window and computes dT/dt
/// via least-squares linear regression.
pub struct DerivativeTracker {
    buffer: VecDeque<(Instant, f64)>,
    window_size: usize,
}

impl DerivativeTracker {
    pub fn new(window_size: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(window_size),
            window_size,
        }
    }

    /// Push a new temperature reading at the current time.
    pub fn push(&mut self, temp_c: f64) {
        self.push_at(Instant::now(), temp_c);
    }

    /// Push a new temperature reading at a specific time (useful for testing).
    pub fn push_at(&mut self, time: Instant, temp_c: f64) {
        if self.buffer.len() >= self.window_size {
            self.buffer.pop_front();
        }
        self.buffer.push_back((time, temp_c));
    }

    /// Compute the rate of temperature change in °C/s using least-squares regression.
    ///
    /// Returns 0.0 if fewer than 2 samples are available.
    pub fn dt_per_second(&self) -> f64 {
        if self.buffer.len() < 2 {
            return 0.0;
        }

        let t0 = self.buffer[0].0;

        // Convert to (seconds_since_first, temperature) pairs
        let n = self.buffer.len() as f64;
        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        let mut sum_xy = 0.0;
        let mut sum_xx = 0.0;

        for (instant, temp) in &self.buffer {
            let x = instant.duration_since(t0).as_secs_f64();
            let y = *temp;
            sum_x += x;
            sum_y += y;
            sum_xy += x * y;
            sum_xx += x * x;
        }

        let denominator = n * sum_xx - sum_x * sum_x;
        if denominator.abs() < f64::EPSILON {
            return 0.0;
        }

        // Slope = (n * Σxy - Σx * Σy) / (n * Σx² - (Σx)²)
        (n * sum_xy - sum_x * sum_y) / denominator
    }

    /// Latest temperature reading, if any.
    pub fn latest_temp(&self) -> Option<f64> {
        self.buffer.back().map(|(_, t)| *t)
    }

    /// Number of samples currently in the buffer.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn empty_tracker_returns_zero() {
        let tracker = DerivativeTracker::new(10);
        assert_eq!(tracker.dt_per_second(), 0.0);
    }

    #[test]
    fn single_sample_returns_zero() {
        let mut tracker = DerivativeTracker::new(10);
        tracker.push(50.0);
        assert_eq!(tracker.dt_per_second(), 0.0);
    }

    #[test]
    fn constant_temperature_returns_zero() {
        let mut tracker = DerivativeTracker::new(10);
        let start = Instant::now();
        for i in 0..5 {
            tracker.push_at(start + Duration::from_secs(i), 50.0);
        }
        assert!(tracker.dt_per_second().abs() < 0.001);
    }

    #[test]
    fn linear_rise_detected() {
        let mut tracker = DerivativeTracker::new(10);
        let start = Instant::now();
        // 2°C/s rise: 50, 52, 54, 56, 58 at 1s intervals
        for i in 0..5 {
            tracker.push_at(start + Duration::from_secs(i), 50.0 + 2.0 * i as f64);
        }
        let slope = tracker.dt_per_second();
        assert!((slope - 2.0).abs() < 0.01, "expected ~2.0, got {}", slope);
    }

    #[test]
    fn linear_drop_detected() {
        let mut tracker = DerivativeTracker::new(10);
        let start = Instant::now();
        // -1°C/s drop
        for i in 0..5 {
            tracker.push_at(start + Duration::from_secs(i), 80.0 - 1.0 * i as f64);
        }
        let slope = tracker.dt_per_second();
        assert!(
            (slope - (-1.0)).abs() < 0.01,
            "expected ~-1.0, got {}",
            slope
        );
    }

    #[test]
    fn window_evicts_old_samples() {
        let mut tracker = DerivativeTracker::new(3);
        let start = Instant::now();

        // Push 5 samples into a window of 3
        for i in 0..5 {
            tracker.push_at(start + Duration::from_secs(i), 50.0 + i as f64);
        }

        assert_eq!(tracker.len(), 3);
        // Latest should be 54.0
        assert_eq!(tracker.latest_temp(), Some(54.0));
    }
}
