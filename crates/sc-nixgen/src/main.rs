mod benchmark;
mod detect;
mod emit;
mod interrupt;
mod optimize;
mod pkl_format;
mod stress;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "sc-nixgen", about = "Generate NixOS configs for SmartCool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Auto-detect hardware sensors and fans, output a starter NixOS config
    Detect {
        /// Include motherboard EC sensors (not just CPU/GPU)
        #[arg(long)]
        include_ec: bool,

        /// Output file path (defaults to stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Generate a topology Pkl template for the benchmark command
        #[arg(long)]
        topology_template: Option<PathBuf>,
    },
    /// Run thermal benchmark with built-in stress testing
    Benchmark {
        /// Pkl file describing fan topology (name, position, direction per fan)
        #[arg(short, long)]
        topology: PathBuf,

        /// Include motherboard EC sensors (not just CPU/GPU)
        #[arg(long)]
        include_ec: bool,

        /// Output tuning data file (Pkl format)
        #[arg(short, long, default_value = "tuning.pkl")]
        output: PathBuf,

        /// Also generate optimized NixOS config from benchmark results
        #[arg(long)]
        generate_nix: Option<PathBuf>,

        /// Minimum coupling |beta| to assign a sensor to a fan (C per +10 PWM)
        #[arg(long, default_value = "0.02")]
        coupling_threshold: f64,

        /// Actually write PWM values and run stress testing. Without this
        /// flag the command only prints the planned transaction.
        #[arg(long)]
        apply: bool,
    },
    /// Inspect one detected fan, optionally running a short reversible PWM test.
    Identify {
        /// Auto-detected fan name (for example fan1 or gpu_fan)
        #[arg(long)]
        fan: String,

        /// Seconds to hold manual control when --apply is used
        #[arg(long, default_value = "5")]
        duration: u64,

        /// Actually take manual control and measure RPM; state is restored on exit.
        #[arg(long)]
        apply: bool,
    },
    /// Generate optimized NixOS config from an existing config + optional tuning data
    Generate {
        /// Path to current SmartCool config (Pkl format)
        #[arg(short, long)]
        config: PathBuf,

        /// Path to tuning data (Pkl format, exported by `sc-nixgen benchmark`)
        #[arg(short, long)]
        tuning: Option<PathBuf>,

        /// Output file path (defaults to stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Minimum coupling |beta| to assign a sensor to a fan (C per +10 PWM)
        #[arg(long, default_value = "0.02")]
        coupling_threshold: f64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Detect {
            include_ec,
            output,
            topology_template,
        } => {
            let result = sc_detect::detect_hardware(include_ec)?;
            detect::print_summary(&result);
            let config = detect::build_config(&result);
            let text = emit::emit_nix_from_detected(&config);

            write_output(&text, output.as_deref())?;

            // Generate topology template if requested
            if let Some(topo_path) = topology_template {
                let template = benchmark::generate_topology_template(&result);
                std::fs::write(&topo_path, &template)
                    .with_context(|| format!("failed to write {}", topo_path.display()))?;
                eprintln!("Topology template written to {}", topo_path.display());
                eprintln!("Edit it to match your case layout, then run:");
                eprintln!("  sc-nixgen benchmark --topology {}", topo_path.display());
            }

            Ok(())
        }
        Command::Benchmark {
            topology: topology_path,
            include_ec,
            output,
            generate_nix,
            coupling_threshold,
            apply,
        } => {
            // Load topology
            let topo = benchmark::load_topology(&topology_path)?;

            // Auto-detect hardware
            let result = sc_detect::detect_hardware(include_ec)?;
            detect::print_summary(&result);

            // Match detected fans to topology entries and resolve hwmon
            let (mut fans, sensors, sensor_names, config) =
                benchmark::prepare_from_detect(&result, &topo)?;

            if !apply {
                eprintln!(
                    "Dry run: resolved {} fan(s) and {} sensor(s); no PWM or stress writes were performed.",
                    fans.len(),
                    sensors.len()
                );
                eprintln!(
                    "Re-run with --apply only after reviewing the topology and having a recovery path."
                );
                return Ok(());
            }

            // Run benchmark with built-in stress
            let tuning = benchmark::run_benchmark(&mut fans, &sensors, &sensor_names, &topo)?;

            let tuning_pkl =
                pkl_format::to_pkl(&tuning).context("failed to serialize tuning data")?;
            std::fs::write(&output, &tuning_pkl)
                .with_context(|| format!("failed to write {}", output.display()))?;
            eprintln!("Tuning data written to {}", output.display());

            // Optionally generate Nix config
            if let Some(nix_path) = generate_nix {
                // Filter out uncontrollable fans (those with no coupling data)
                let controllable_config = filter_uncontrollable_fans(&config, &tuning);

                let optimized = optimize::optimize_config(
                    &controllable_config,
                    Some(&tuning),
                    coupling_threshold,
                );
                let nix_output = emit::emit_nix(&controllable_config, &optimized, Some(&tuning));
                std::fs::write(&nix_path, &nix_output)
                    .with_context(|| format!("failed to write {}", nix_path.display()))?;
                eprintln!("NixOS config written to {}", nix_path.display());
            }

            Ok(())
        }
        Command::Identify {
            fan,
            duration,
            apply,
        } => identify(&fan, duration, apply),
        Command::Generate {
            config: config_path,
            tuning: tuning_path,
            output,
            coupling_threshold,
        } => {
            let config = sc_core::config::load(&config_path)
                .with_context(|| format!("failed to load config from {}", config_path.display()))?;

            let tuning = match tuning_path {
                Some(path) => {
                    let tuning = pkl_format::load(&path, "tuning data").with_context(|| {
                        format!("failed to load tuning data from {}", path.display())
                    })?;
                    Some(tuning)
                }
                None => None,
            };

            let optimized = optimize::optimize_config(&config, tuning.as_ref(), coupling_threshold);
            let nix_output = emit::emit_nix(&config, &optimized, tuning.as_ref());

            write_output(&nix_output, output.as_deref())
        }
    }
}

fn identify(name: &str, duration: u64, apply: bool) -> Result<()> {
    anyhow::ensure!(duration > 0, "duration must be greater than zero");
    let result = sc_detect::detect_hardware(true)?;
    let detected = result
        .fans
        .iter()
        .find(|candidate| candidate.auto_name == name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "fan '{}' was not detected; available fans: {}",
                name,
                result
                    .fans
                    .iter()
                    .map(|candidate| candidate.auto_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    let mut controller =
        sc_hwmon::fan::Fan::from_path(name, &detected.hwmon_path, detected.pwm_index)?;
    let original_pwm = controller.read_pwm()?;
    let original_rpm = controller.read_rpm().unwrap_or(0);

    println!(
        "{}: chip={} pwm{} current_pwm={} current_rpm={}",
        name, detected.hwmon_chip, detected.pwm_index, original_pwm, original_rpm
    );
    if !apply {
        println!(
            "Dry run: no fan control writes were performed. Re-run with --apply to test this fan."
        );
        return Ok(());
    }

    let interrupt = interrupt::InterruptGuard::new()?;
    let mut control = sc_hwmon::fan::FanControlGuard::new(std::slice::from_mut(&mut controller));
    let fan = &mut control.fans_mut()[0];
    fan.set_manual()?;
    fan.write_pwm(255)?;
    interrupt.sleep(Duration::from_secs(duration))?;
    let rpm = fan.read_rpm()?;
    println!("{}: measured_rpm={} at_pwm=255", name, rpm);
    Ok(())
}

/// Remove fans from config that have zero coupling entries in tuning data.
/// These are firmware-controlled or uncontrollable fans that shouldn't appear
/// in the generated NixOS config.
fn filter_uncontrollable_fans(
    config: &sc_core::config::Config,
    tuning: &sc_core::ipc::TuningResponse,
) -> sc_core::config::Config {
    let fans_with_data: std::collections::HashSet<&str> = tuning
        .coupling_matrix
        .iter()
        .map(|e| e.fan.as_str())
        .collect();

    let mut filtered = config.clone();
    let removed: Vec<String> = filtered
        .fans
        .iter()
        .filter(|f| !fans_with_data.contains(f.name.as_str()))
        .map(|f| f.name.clone())
        .collect();

    if !removed.is_empty() {
        eprintln!(
            "Excluding uncontrollable fans from config: {}",
            removed.join(", ")
        );
        filtered
            .fans
            .retain(|f| fans_with_data.contains(f.name.as_str()));
    }

    filtered
}

fn write_output(text: &str, output: Option<&Path>) -> Result<()> {
    match output {
        Some(path) => {
            std::fs::write(path, text)
                .with_context(|| format!("failed to write {}", path.display()))?;
            eprintln!("wrote output to {}", path.display());
        }
        None => {
            print!("{}", text);
        }
    }
    Ok(())
}
