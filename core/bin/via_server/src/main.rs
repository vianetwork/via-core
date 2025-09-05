use std::str::FromStr;

use anyhow::Context as _;
use clap::Parser;
use zksync_config::{
    configs::{
        via_secrets::{ViaDASecrets, ViaL1Secrets, ViaSecrets},
        via_wallets::ViaWallets,
        DatabaseSecrets, L1Secrets, Secrets,
    },
    ContractsConfig, GenesisConfig,
};
use zksync_core_leftovers::{
    temp_config_store::{decode_yaml_repr, ViaTempConfigStore},
    ViaComponent, ViaComponents,
};
use zksync_env_config::FromEnv;

mod node_builder;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[derive(Debug, Parser)]
#[command(author = "Via Protocol", version, about = "Via sequencer node", long_about = None)]
struct Cli {
    /// Generate genesis block for the first contract deployment using temporary DB.
    #[arg(long)]
    genesis: bool,

    /// Comma-separated list of components to launch.
    #[arg(
        long,
        default_value = "api,btc,tree,tree_api,state_keeper,housekeeper,proof_data_handler,commitment_generator,celestia,da_dispatcher,vm_runner_protective_reads,vm_runner_bwip"
    )]
    components: ComponentsToRun,

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

#[derive(Debug, Clone)]
struct ComponentsToRun(Vec<ViaComponent>);

impl FromStr for ComponentsToRun {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let components = s.split(',').try_fold(vec![], |mut acc, component_str| {
            let components = ViaComponents::from_str(component_str.trim())?;
            acc.extend(components.0);
            Ok::<_, String>(acc)
        })?;
        Ok(Self(components))
    }
}

fn main() -> anyhow::Result<()> {
    let opt = Cli::parse();

    let wallets = match opt.wallets_path {
        None => ViaWallets::from_env()?,
        Some(path) => {
            let yaml =
                std::fs::read_to_string(&path).with_context(|| path.display().to_string())?;
            decode_yaml_repr::<zksync_protobuf_config::proto::via_wallets::ViaWallets>(&yaml)
                .context("failed decoding wallets YAML config")?
        }
    };

    let secrets: ViaSecrets = match opt.secrets_path {
        Some(path) => {
            let yaml =
                std::fs::read_to_string(&path).with_context(|| path.display().to_string())?;
            decode_yaml_repr::<zksync_protobuf_config::proto::via_secrets::ViaSecrets>(&yaml)
                .context("failed decoding secrets YAML config")?
        }
        None => ViaSecrets {
            base_secrets: Secrets {
                consensus: None,
                database: DatabaseSecrets::from_env().ok(),
                l1: L1Secrets::from_env().ok(),
            },
            via_l1: ViaL1Secrets::from_env().ok(),
            via_da: ViaDASecrets::from_env().ok(),
        },
    };

    // Load configurations
    let configs = match opt.config_path {
        Some(_path) => {
            return Err(anyhow::anyhow!(
                "The Via Server does not support configuration files at this point. Please use env variables."
            ));
        }
        None => {
            tracing::info!("Loading configs from env");
            ViaTempConfigStore::general()?
        }
    };

    let genesis = match opt.genesis_path {
        Some(_path) => {
            return Err(anyhow::anyhow!(
                "The Via Server does not support configuration files at this point. Please use env variables."
            ));
        }
        None => GenesisConfig::from_env().context("Failed to load genesis from env")?,
    };

    let mut contracts_config = match opt.contracts_config_path {
        Some(_path) => {
            return Err(anyhow::anyhow!(
                "The Via Server does not support configuration files at this point. Please use env variables."
            ));
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

    node_builder
        .build(opt.components.0)?
        .run(observability_guard)?;

    Ok(())
}
