use anyhow::Context as _;
use clap::Parser;
use zksync_config::configs::{PostgresConfig, Secrets};
use zksync_env_config::FromEnv;

mod node_builder;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[derive(Debug, Parser)]
#[command(author = "Matter Labs", version, about = "Via validator/sequencer node", long_about = None)]
struct Cli {
    /// Path to the yaml config. If set, it will be used instead of env vars.
    #[arg(long)]
    config_path: Option<std::path::PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let _opt = Cli::parse();

    let postgres_config = PostgresConfig::from_env().context("PostgresConfig")?;
    let secrets = Secrets {
        consensus: None,
        database: None,
        l1: None,
    };

    let node = node_builder::NodeBuilder::new(postgres_config, secrets)?;
    node.build()?
        .run(zksync_vlog::ObservabilityBuilder::new().build())?;

    Ok(())
}
