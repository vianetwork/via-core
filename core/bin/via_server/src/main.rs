use anyhow::Context as _;
use clap::Parser;
use zksync_config::{
    configs::{via_general, DatabaseSecrets, L1Secrets, Secrets},
    ContractsConfig, GenesisConfig, ViaGeneralConfig,
};
use zksync_env_config::FromEnv;

mod config;
mod node_builder;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[derive(Debug, Parser)]
#[command(author = "Via Protocol", version, about = "Via validator/sequencer node", long_about = None)]
struct Cli {
    /// Generate genesis block for the first contract deployment using temporary DB.
    #[arg(long)]
    genesis: bool,
    /// Path to the YAML config. If set, it will be used instead of env vars.
    #[arg(long)]
    config_path: Option<std::path::PathBuf>,

    /// Path to the YAML with secrets. If set, it will be used instead of env vars.
    #[arg(long)]
    secrets_path: Option<std::path::PathBuf>,

    /// Path to the yaml with contracts. If set, it will be used instead of env vars.
    #[arg(long)]
    contracts_config_path: Option<std::path::PathBuf>,

    /// Path to the wallets config. If set, it will be used instead of env vars.
    #[arg(long)]
    wallets_path: Option<std::path::PathBuf>,

    /// Path to the YAML with genesis configuration. If set, it will be used instead of env vars.
    #[arg(long)]
    genesis_path: Option<std::path::PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let opt = Cli::parse();

    // Load env config
    let tmp_config = config::load_env_config()?;

    // Load configurations
    let configs = match opt.config_path {
        Some(_path) => {
            todo!("Load config from file")
        }
        None => {
            let general = tmp_config.general();
            let mut via_general = ViaGeneralConfig::from(general);

            // Load the rest of the configs
            let via_configs = config::via_load_env_config()?;
            via_general.via_btc_watch_config = Some(via_configs.0);
            via_general.via_btc_sender_config = Some(via_configs.1);
            via_general.via_celestia_config = Some(via_configs.2);
            via_general
        }
    };

    let secrets = match opt.secrets_path {
        Some(_path) => {
            todo!("Load secrets from file")
        }
        None => Secrets {
            consensus: config::read_consensus_secrets().context("read_consensus_secrets()")?,
            database: DatabaseSecrets::from_env().ok(),
            l1: L1Secrets::from_env().ok(),
        },
    };

    let genesis = match opt.genesis_path {
        Some(_path) => {
            todo!("Load genesis from file")
        }
        None => GenesisConfig::from_env().context("Failed to load genesis from env")?,
    };

    let wallets = match opt.wallets_path {
        Some(_path) => {
            todo!("Load config from file");
        }
        None => tmp_config.wallets(),
    };

    let mut contracts_config = match opt.contracts_config_path {
        Some(_path) => {
            todo!("Load contracts from file")
        }
        None => ContractsConfig::from_env().context("contracts_config")?,
    };

    // Disable ecosystem contracts for now
    contracts_config.ecosystem_contracts = None;

    let observability_config = configs
        .observability
        .clone()
        .context("Observability config missing")?;

    let node_builder =
        node_builder::ViaNodeBuilder::new(configs, wallets, secrets, genesis, contracts_config)?;

    let observability_guard = {
        // Observability initialization should be performed within tokio context.
        let _context_guard = node_builder.runtime_handle().enter();
        observability_config.install()?
    };

    // Build the node

    if opt.genesis {
        let node = node_builder.only_genesis()?;
        node.run(observability_guard)?;
        return Ok(());
    }

    let node = node_builder.build()?;
    node.run(observability_guard)?;

    Ok(())
}
