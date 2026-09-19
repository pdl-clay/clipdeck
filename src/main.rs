//! transferencia — histórico da área de transferência com Super+V para COSMIC.

mod app;
mod autostart;
mod clipboard;
mod cli;
mod config;
mod db;
mod paste;
mod selection;
mod shortcut;
mod util;
mod view;
mod watcher;

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    init_logging();

    let cli = cli::Cli::parse();
    let action = cli.action();
    let result = match cli.command {
        Some(command) => command.run(),
        None => app::run(action).map_err(|err| anyhow::anyhow!("{err}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("erro: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,transferencia=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}
