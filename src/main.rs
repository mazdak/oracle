mod cli;
mod service;

use clap::Parser;
use cli::{Cli, Command, run_cli_call};
use service::run_server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Call(args)) => run_cli_call(args).await?,
        _ => run_server().await?,
    }

    Ok(())
}
