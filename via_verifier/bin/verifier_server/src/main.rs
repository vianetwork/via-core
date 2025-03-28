use anyhow::Context as _;
use clap::Parser;
use zksync_config::{
    configs::{DatabaseSecrets, L1Secrets, Secrets},
    GenesisConfig, ViaGeneralConfig,
};
use zksync_env_config::FromEnv;

mod config;
mod node_builder;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[derive(Debug, Parser)]
#[command(author = "Via verifer", version, about = "Via verifer node", long_about = None)]
struct Cli {
    /// Path to the YAML config. If set, it will be used instead of env vars.
    #[arg(long)]
    config_path: Option<std::path::PathBuf>,

    /// Path to the YAML with secrets. If set, it will be used instead of env vars.
    #[arg(long)]
    secrets_path: Option<std::path::PathBuf>,

    /// Path to the YAML with genesis configuration. If set, it will be used instead of env vars.
    #[arg(long)]
    genesis_path: Option<std::path::PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let opt = Cli::parse();

    // Load env config
    let env_config = config::load_env_config()?;

    // Load configurations
    let configs = match opt.config_path {
        Some(_path) => {
            return Err(anyhow::anyhow!(
                "The Verifier Server does not support config files at this time. Please use env variables."
            ));
        }
        None => {
            let general = env_config.general();
            let mut via_general = ViaGeneralConfig::from(general);

            // Load the rest of the configs
            let via_configs = config::via_load_env_config()?;
            via_general.via_btc_watch_config = Some(via_configs.0);
            via_general.via_btc_sender_config = Some(via_configs.1);
            via_general.via_celestia_config = Some(via_configs.2);
            via_general.via_verifier_config = Some(via_configs.3);
            via_general
        }
    };

    let secrets = match opt.secrets_path {
        Some(_path) => {
            return Err(anyhow::anyhow!(
                "The Verifier Server does not support config files at this time. Please use env variables."
            ));
        }
        None => Secrets {
            consensus: config::read_consensus_secrets().context("read_verifier_secrets()")?,
            database: DatabaseSecrets::from_env().ok(),
            l1: L1Secrets::from_env().ok(),
        },
    };

    let genesis = match opt.genesis_path {
        Some(_path) => {
            return Err(anyhow::anyhow!(
                "The Via Server does not support configuration files at this point. Please use env variables."
            ));
        }
        None => GenesisConfig::from_env().context("Failed to load genesis from env")?,
    };

    let observability_config = configs
        .observability
        .clone()
        .context("Observability config missing")?;

    let node_builder = node_builder::ViaNodeBuilder::new(configs, genesis, secrets)?;

    let observability_guard = {
        // Observability initialization should be performed within tokio context.
        let _context_guard = node_builder.runtime_handle().enter();
        observability_config.install()?
    };

    // Build the node

    let node = node_builder.build()?;
    node.run(observability_guard)?;

    Ok(())
}
