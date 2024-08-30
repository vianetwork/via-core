use anyhow::Context as _;
use clap::Parser;
use zksync_config::configs::{PostgresConfig, Secrets, ObservabilityConfig, DatabaseSecrets};
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

    let default_db_secrets = DatabaseSecrets {
        server_url: None,
        prover_url: None,
        server_replica_url: None,
    };

    let secrets = Secrets {
        consensus: None,
        database: Some(default_db_secrets),
        l1: None,
    };

    let observability_config = ObservabilityConfig {
        sentry_url: None,
        sentry_environment: None,
        opentelemetry: None,
        log_format: "plain".to_string(),
        log_directives: None,
    };

    let observability_guard = observability_config.install()?;

    let node_builder = node_builder::NodeBuilder::new(postgres_config, secrets)?;
    let node = node_builder.build()?;

    node.run(observability_guard)?;

    Ok(())
}