mod asusd;
mod controller;
mod display;
mod ipc_server;
mod thermal;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "sc", about = "SmartCool — intelligent fan control daemon")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the fan control daemon
    Daemon {
        /// Path to configuration file (Pkl format)
        #[arg(short, long, default_value = "/etc/smartcool/config.pkl")]
        config: PathBuf,
    },
    /// Query current sensor and fan status
    Status,
    /// Show cooling effectiveness analytics
    Analytics,
    /// Show advanced thermal tuning data (coupling matrix, step responses, CCF)
    Tuning,
    /// Validate a configuration file
    Config {
        /// Path to configuration file to validate
        #[arg(long)]
        validate: PathBuf,
    },
    /// Inspect or manage firmware fan curves through asusd
    Asusd {
        #[command(subcommand)]
        command: asusd::Command,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Daemon {
            config: config_path,
        } => controller::run_daemon(&config_path),
        Command::Status => {
            let response = sc_core::ipc::query(sc_core::ipc::Request::Status)?;
            display::print_status(&response);
            Ok(())
        }
        Command::Analytics => {
            let response = sc_core::ipc::query(sc_core::ipc::Request::Analytics)?;
            display::print_analytics(&response);
            Ok(())
        }
        Command::Tuning => {
            let response = sc_core::ipc::query(sc_core::ipc::Request::Tuning)?;
            display::print_tuning(&response);
            Ok(())
        }
        Command::Config { validate } => {
            let cfg = sc_core::config::load(&validate)?;
            println!(
                "Configuration valid: {} sensors, {} fans",
                cfg.sensors.len(),
                cfg.fans.len()
            );
            Ok(())
        }
        Command::Asusd { command } => asusd::run(command),
    }
}
