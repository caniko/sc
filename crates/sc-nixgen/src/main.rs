mod detect;
mod emit;
mod optimize;

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
        #[arg(long, default_value = "0.1")]
        coupling_threshold: f64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Detect { include_ec, output } => {
            let result = sc_detect::detect_hardware(include_ec)?;
            detect::print_summary(&result);
            let config = detect::build_config(&result);
            let text = emit::emit_nix_from_detected(&config);

            write_output(&text, output.as_deref())
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
                    let t: sc_core::ipc::TuningResponse = ron::from_str(&content)
                        .with_context(|| {
                            format!("failed to parse tuning RON from {}", path.display())
                        })?;
                    Some(t)
                }
                None => None,
            };

            let optimized =
                optimize::optimize_config(&config, tuning.as_ref(), coupling_threshold);
            let nix_output = emit::emit_nix(&config, &optimized, tuning.as_ref());

            write_output(&nix_output, output.as_deref())
        }
    }
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
