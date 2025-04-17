use anyhow::Context as _;
use clap::Parser;
use zksync_config::{
    configs::{
        via_secrets::{ViaDASecrets, ViaL1Secrets, ViaSecrets},
        via_wallets::ViaWallets,
        DatabaseSecrets, L1Secrets, Secrets,
    },
    GenesisConfig, ViaVerifierConfig,
};
use zksync_core_leftovers::temp_config_store::ViaTempConfigStore;
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

    /// Path to the wallets config. If set, it will be used instead of env vars.
    #[arg(long)]
    wallets_path: Option<std::path::PathBuf>,

    /// Path to the YAML with genesis configuration. If set, it will be used instead of env vars.
    #[arg(long)]
    genesis_path: Option<std::path::PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let opt = Cli::parse();

    // Load configurations
    let configs = match opt.config_path {
        Some(_path) => {
            return Err(anyhow::anyhow!(
                "The Verifier Server does not support config files at this time. Please use env variables."
            ));
        }
        None => {
            let mut via_general_config = ViaTempConfigStore::general()?;
            via_general_config.via_verifier_config =
                Some(ViaVerifierConfig::from_env().context("Failed to load genesis config")?);
            via_general_config
        }
    };

    let secrets = match opt.secrets_path {
        Some(_path) => {
            return Err(anyhow::anyhow!(
                "The Verifier Server does not support config files at this time. Please use env variables."
            ));
        }
        None => ViaSecrets {
            base_secrets: Secrets {
                consensus: config::read_consensus_secrets().context("read_consensus_secrets()")?,
                database: DatabaseSecrets::from_env().ok(),
                l1: L1Secrets::from_env().ok(),
            },
            via_l1: ViaL1Secrets::from_env().ok(),
            via_da: ViaDASecrets::from_env().ok(),
        },
    };

    let wallets = match opt.wallets_path {
        Some(_path) => {
            return Err(anyhow::anyhow!(
                "The Via Server does not support configuration files at this point. Please use env variables."
            ));
        }
        None => ViaWallets::from_env()?,
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

    let node_builder = node_builder::ViaNodeBuilder::new(configs, genesis, secrets, wallets)?;

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
