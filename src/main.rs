mod cli;
mod config;
mod content;
mod render;
mod server;
mod service;
mod theme;

use anyhow::Result;
use clap::Parser;

use crate::cli::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    cli::install_tracing(cli.verbosity());
    cli.run().await
}
