//! Thin kernel binary that boots the wattetheria node runtime.

use anyhow::Result;
use clap::Parser;
use wattetheria_node_core::{Cli, init_tracing, run};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    run(Cli::parse()).await
}
