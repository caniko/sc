mod emit;
mod optimize;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "sc-nixgen", about = "Generate optimized NixOS configs from SmartCool tuning data")]
struct Cli {
    /// Path to current SmartCool config (RON format)
    #[arg(short, long, default_value = "/etc/smartcool/config.ron")]
    config: PathBuf,

    /// Output file path (defaults to stdout)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Minimum coupling |beta| to assign a sensor to a fan (C per +10 PWM)
    #[arg(long, default_value = "0.1")]
    coupling_threshold: f64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load current config for hardware details
    let config = sc_core::config::load(&cli.config)
        .with_context(|| format!("failed to load config from {}", cli.config.display()))?;

    // Query daemon for tuning data
    let tuning = match sc_core::ipc::query(sc_core::ipc::Request::Tuning) {
        Ok(sc_core::ipc::Response::Tuning(t)) => Some(t),
        Ok(_) => {
            eprintln!("warning: unexpected response from daemon, using original curves");
            None
        }
        Err(e) => {
            eprintln!("warning: could not connect to daemon ({}), using original curves", e);
            None
        }
    };

    // Optimize config based on tuning data
    let optimized = optimize::optimize_config(&config, tuning.as_ref(), cli.coupling_threshold);

    // Emit Nix
    let nix_output = emit::emit_nix(&config, &optimized, tuning.as_ref());

    match cli.output {
        Some(path) => {
            std::fs::write(&path, &nix_output)
                .with_context(|| format!("failed to write {}", path.display()))?;
            eprintln!("wrote optimized config to {}", path.display());
        }
        None => {
            print!("{}", nix_output);
        }
    }

    Ok(())
}
