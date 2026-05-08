use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// A running stress workload that auto-stops on drop.
pub struct StressProcess {
    inner: StressInner,
    label: String,
}

enum StressInner {
    /// Pure-Rust CPU stress: threads doing heavy FP computation.
    Cpu {
        running: Arc<AtomicBool>,
        handles: Vec<JoinHandle<()>>,
    },
    /// GPU stress via sysfs clock forcing.
    Gpu {
        /// (sysfs_path, original_value) for each forced GPU.
        originals: Vec<(PathBuf, String)>,
    },
}

impl StressProcess {
    /// Start CPU stress: spawn one thread per core doing matrix multiplication.
    pub fn start_cpu() -> Result<Self> {
        let n_cpus = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4);

        eprintln!("Starting built-in CPU stress ({} threads)...", n_cpus);

        let running = Arc::new(AtomicBool::new(true));
        let mut handles = Vec::with_capacity(n_cpus);

        for _ in 0..n_cpus {
            let running = running.clone();
            handles.push(std::thread::spawn(move || {
                cpu_burn_loop(&running);
            }));
        }

        Ok(Self {
            inner: StressInner::Cpu { running, handles },
            label: format!("CPU stress ({} threads)", n_cpus),
        })
    }

    /// Start GPU stress: force all AMD GPUs to max clocks via sysfs.
    /// This pushes a modern dGPU from ~15W idle to ~80-120W, generating
    /// enough thermal load for coupling measurement.
    pub fn start_gpu() -> Result<Self> {
        let gpu_cards = find_amdgpu_cards()?;

        if gpu_cards.is_empty() {
            anyhow::bail!(
                "no AMD GPU DRM cards found for clock forcing.\n\
                 GPU stress currently supports AMD GPUs via sysfs power management."
            );
        }

        let mut originals = Vec::new();

        for card_path in &gpu_cards {
            let perf_path = card_path.join("device/power_dpm_force_performance_level");

            // Save original value
            let original = std::fs::read_to_string(&perf_path)
                .with_context(|| format!("failed to read {}", perf_path.display()))?
                .trim()
                .to_string();

            // Force max clocks
            std::fs::write(&perf_path, "high")
                .with_context(|| format!("failed to write 'high' to {}", perf_path.display()))?;

            let card_name = card_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            eprintln!(
                "  {} forced to max clocks (was: {})",
                card_name, original
            );

            originals.push((perf_path, original));
        }

        eprintln!(
            "GPU stress active: {} card(s) at max clocks",
            originals.len()
        );

        Ok(Self {
            inner: StressInner::Gpu { originals },
            label: format!("GPU stress ({} cards)", gpu_cards.len()),
        })
    }

    /// Stop the stress workload.
    pub fn stop(&mut self) {
        match &mut self.inner {
            StressInner::Cpu { running, handles } => {
                running.store(false, Ordering::Relaxed);
                for handle in handles.drain(..) {
                    let _ = handle.join();
                }
            }
            StressInner::Gpu { originals } => {
                for (path, original) in originals.drain(..) {
                    if let Err(e) = std::fs::write(&path, &original) {
                        eprintln!(
                            "  warning: failed to restore {}: {}",
                            path.display(),
                            e
                        );
                    }
                }
            }
        }
        eprintln!("Stopped {}", self.label);
    }
}

impl Drop for StressProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

// ─── CPU burn ────────────────────────────────────────────────────────────────

/// Heavy FP computation loop: 4×4 matrix multiply, fits in registers/L1.
/// Maximizes FP throughput and power draw.
fn cpu_burn_loop(running: &AtomicBool) {
    // Initialize two 4x4 matrices with non-trivial values
    let mut a = [0.0f64; 16];
    let mut b = [0.0f64; 16];
    for i in 0..16 {
        a[i] = (i as f64 + 1.0) * 0.01;
        b[i] = (16 - i) as f64 * 0.01;
    }

    let mut result = [0.0f64; 16];
    let mut iteration = 0u64;

    while running.load(Ordering::Relaxed) {
        // 4x4 matrix multiply: 64 FMA operations per iteration
        for i in 0..4 {
            for j in 0..4 {
                let mut sum = 0.0f64;
                for k in 0..4 {
                    sum += a[i * 4 + k] * b[k * 4 + j];
                }
                result[i * 4 + j] = sum;
            }
        }

        // Feed result back to prevent optimizer from eliding the work
        // and keep values from growing unbounded
        iteration = iteration.wrapping_add(1);
        if iteration % 1024 == 0 {
            for i in 0..16 {
                a[i] = result[i].fract() * 0.5 + 0.25;
            }
        }
    }

    // Prevent the compiler from optimizing away the result
    std::hint::black_box(result);
}

// ─── GPU card discovery ──────────────────────────────────────────────────────

/// Find all DRM card paths that use the amdgpu driver.
fn find_amdgpu_cards() -> Result<Vec<PathBuf>> {
    let mut cards = Vec::new();
    let drm_dir = PathBuf::from("/sys/class/drm");

    let entries = std::fs::read_dir(&drm_dir).context("failed to read /sys/class/drm")?;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Only cardN, not cardN-DP-1 etc.
        if !name_str.starts_with("card") || name_str.contains('-') {
            continue;
        }

        let card_path = entry.path();
        let driver_link = card_path.join("device/driver");

        if let Ok(target) = std::fs::read_link(&driver_link) {
            if let Some(driver_name) = target.file_name() {
                if driver_name == "amdgpu" {
                    // Verify perf level file exists
                    let perf_path = card_path.join("device/power_dpm_force_performance_level");
                    if perf_path.exists() {
                        cards.push(card_path);
                    }
                }
            }
        }
    }

    cards.sort();
    Ok(cards)
}
