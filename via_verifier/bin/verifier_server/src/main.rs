use anyhow::Context;
use clap::Parser;
use zksync_config::{
    configs::{
        api::HealthCheckConfig,
        chain::CircuitBreakerConfig,
        via_bridge::ViaBridgeConfig,
        via_btc_client::ViaBtcClientConfig,
        via_consensus::ViaGenesisConfig,
        via_secrets::{ViaDASecrets, ViaL1Secrets, ViaSecrets},
        via_verifier::ViaGeneralVerifierConfig,
        via_wallets::ViaWallets,
        DatabaseSecrets, L1Secrets, ObservabilityConfig, PrometheusConfig, Secrets,
    },
    GenesisConfig, PostgresConfig, ViaBtcSenderConfig, ViaBtcWatchConfig, ViaCelestiaConfig,
    ViaVerifierConfig,
};
use zksync_core_leftovers::temp_config_store::decode_yaml_repr;
use zksync_env_config::FromEnv;

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

    let via_general_verifier_config = ViaGeneralVerifierConfig {
        genesis_config: GenesisConfig::from_env()?,
        health_check: HealthCheckConfig::from_env()?,
        postgres_config: PostgresConfig::from_env()?,
        prometheus_config: PrometheusConfig::from_env()?,
        via_btc_client_config: ViaBtcClientConfig::from_env()?,
        via_btc_watch_config: ViaBtcWatchConfig::from_env()?,
        via_genesis_config: ViaGenesisConfig::from_env()?,
        via_bridge_config: ViaBridgeConfig::from_env()?,
        via_btc_sender_config: ViaBtcSenderConfig::from_env()?,
        via_celestia_config: ViaCelestiaConfig::from_env()?,
        via_verifier_config: ViaVerifierConfig::from_env()?,
        observability_config: ObservabilityConfig::from_env()?,
        circuit_breaker_config: CircuitBreakerConfig::from_env()?,
        secrets,
        wallets,
    };

    let node_builder = node_builder::ViaNodeBuilder::new(via_general_verifier_config.clone())?;

    let observability_guard = {
        // Observability initialization should be performed within tokio context.
        let _context_guard = node_builder.runtime_handle().enter();
        via_general_verifier_config.observability_config.install()?
    };

    // Build the node

    let node = node_builder.build()?;
    node.run(observability_guard)?;

    Ok(())
}
