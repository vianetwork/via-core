use zksync_config::{
    configs::{
        api::HealthCheckConfig,
        via_bridge::ViaBridgeConfig,
        via_btc_client::ViaBtcClientConfig,
        via_consensus::ViaGenesisConfig,
        via_l1_indexer::ViaIndexerConfig,
        via_secrets::{ViaL1Secrets, ViaSecrets},
        DatabaseSecrets, ObservabilityConfig, PrometheusConfig, Secrets,
    },
    PostgresConfig, ViaBtcWatchConfig,
};
use zksync_env_config::FromEnv;

mod node_builder;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> anyhow::Result<()> {
    let via_indexer_config = ViaIndexerConfig {
        health_check: HealthCheckConfig::from_env()?,
        postgres_config: PostgresConfig::from_env()?,
        prometheus_config: PrometheusConfig::from_env()?,
        via_btc_client_config: ViaBtcClientConfig::from_env()?,
        via_btc_watch_config: ViaBtcWatchConfig::from_env()?,
        via_genesis_config: ViaGenesisConfig::from_env()?,
        observability_config: ObservabilityConfig::from_env()?,
        secrets: ViaSecrets {
            base_secrets: Secrets {
                consensus: None,
                database: DatabaseSecrets::from_env().ok(),
                l1: None,
                data_availability: None,
            },
            via_l1: ViaL1Secrets::from_env().ok(),
            via_l2: None,
            via_da: None,
        },
        via_bridge_config: ViaBridgeConfig::from_env()?,
    };

    let node_builder = node_builder::ViaNodeBuilder::new(via_indexer_config.clone())?;

    let observability_guard = {
        // Observability initialization should be performed within tokio context.
        let _context_guard = node_builder.runtime_handle().enter();
        via_indexer_config.observability_config.install()?
    };

    // Build the node

    let node = node_builder.build()?;
    node.run(observability_guard)?;

    Ok(())
}
