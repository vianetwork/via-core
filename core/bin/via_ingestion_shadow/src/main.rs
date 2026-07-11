use clap::Parser;
use via_ingestion_shadow::{run_cli, Cli};

#[tokio::main]
async fn main() {
    if let Err(err) = run_cli(Cli::parse()).await {
        eprintln!("via_ingestion_shadow: {err:#}");
        std::process::exit(1);
    }
}
