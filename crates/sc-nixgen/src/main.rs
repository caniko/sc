mod benchmark;
mod detect;
mod emit;
mod optimize;
mod stress;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

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

        /// Output tuning data file (RON format)
        #[arg(short, long, default_value = "tuning.ron")]
        output: PathBuf,

        /// Also generate optimized NixOS config from benchmark results
        #[arg(long)]
        generate_nix: Option<PathBuf>,

        /// Minimum coupling |beta| to assign a sensor to a fan (C per +10 PWM)
        #[arg(long, default_value = "0.02")]
        coupling_threshold: f64,
    },
    /// Generate optimized NixOS config from an existing config + optional tuning data
    Generate {
        /// Path to current SmartCool config (RON format)
        #[arg(short, long)]
        config: PathBuf,

        /// Path to tuning data (RON format, exported by `sc tuning --export`)
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
        } => {
            // Load topology
            let topo = benchmark::load_topology(&topology_path)?;

            // Auto-detect hardware
            let result = sc_detect::detect_hardware(include_ec)?;
            detect::print_summary(&result);

            // Match detected fans to topology entries and resolve hwmon
            let (mut fans, sensors, sensor_names, config) =
                benchmark::prepare_from_detect(&result, &topo)?;

            // Run benchmark with built-in stress
            let tuning = benchmark::run_benchmark(&mut fans, &sensors, &sensor_names, &topo)?;

            // Write tuning data as RON
            let tuning_ron = ron::ser::to_string_pretty(&tuning, ron::ser::PrettyConfig::default())
                .context("failed to serialize tuning data")?;
            std::fs::write(&output, &tuning_ron)
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
                    let content = std::fs::read_to_string(&path).with_context(|| {
                        format!("failed to read tuning data from {}", path.display())
                    })?;
                    let t: sc_core::ipc::TuningResponse =
                        ron::from_str(&content).with_context(|| {
                            format!("failed to parse tuning RON from {}", path.display())
                        })?;
                    Some(t)
                }
                None => None,
            };

            let optimized = optimize::optimize_config(&config, tuning.as_ref(), coupling_threshold);
            let nix_output = emit::emit_nix(&config, &optimized, tuning.as_ref());

            write_output(&nix_output, output.as_deref())
        }
    }
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
