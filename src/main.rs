use anyhow::Result;
use clap::Parser;

use mdshelf::cli::{self, Cli};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    cli::install_tracing(cli.verbosity());
    cli.run().await
}
