use anyhow::{bail, Context, Result};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::flag;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Converts termination signals into an ordinary error so transaction guards
/// can restore hardware before the process exits.
pub struct InterruptGuard {
    interrupted: Arc<AtomicBool>,
}

impl InterruptGuard {
    pub fn new() -> Result<Self> {
        let interrupted = Arc::new(AtomicBool::new(false));
        flag::register(SIGINT, Arc::clone(&interrupted)).context("register SIGINT handler")?;
        flag::register(SIGTERM, Arc::clone(&interrupted)).context("register SIGTERM handler")?;
        Ok(Self { interrupted })
    }

    pub fn check(&self) -> Result<()> {
        if self.interrupted.load(Ordering::Relaxed) {
            bail!("operation interrupted; restoring hardware state")
        }
        Ok(())
    }

    pub fn sleep(&self, duration: Duration) -> Result<()> {
        let deadline = std::time::Instant::now() + duration;
        while std::time::Instant::now() < deadline {
            self.check()?;
            thread::sleep(
                Duration::from_millis(100)
                    .min(deadline.saturating_duration_since(std::time::Instant::now())),
            );
        }
        self.check()
    }
}
