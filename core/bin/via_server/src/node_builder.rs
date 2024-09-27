use anyhow::Context;
use zksync_config::{
    configs::{wallets::Wallets, GeneralConfig, PostgresConfig, Secrets},
    ContractsConfig, GenesisConfig, ViaBtcWatchConfig, ViaGeneralConfig,
};
use zksync_node_framework::{
    implementations::layers::{
        object_store::ObjectStoreLayer,
        pools_layer::PoolsLayerBuilder,
        postgres_metrics::PostgresMetricsLayer,
        prometheus_exporter::PrometheusExporterLayer,
        sigint::SigintHandlerLayer,
        via_btc_sender::{
            aggregator::ViaBtcInscriptionAggregatorLayer, manager::ViaInscriptionManagerLayer,
        },
        via_btc_watch::BtcWatchLayer,
    },
    service::{ZkStackService, ZkStackServiceBuilder},
};
use zksync_vlog::prometheus::PrometheusExporterConfig;

/// Macro that looks into a path to fetch an optional config,
/// and clones it into a variable.
macro_rules! try_load_config {
    ($path:expr) => {
        $path.as_ref().context(stringify!($path))?.clone()
    };
}

// TODO: list of upcoming layers
// - prometheus_exporter
//

pub struct ViaNodeBuilder {
    node: ZkStackServiceBuilder,
    configs: ViaGeneralConfig,
    // wallets: Wallets,
    genesis_config: GenesisConfig,
    // contracts_config: ContractsConfig,
    secrets: Secrets,
}

impl ViaNodeBuilder {
    pub fn new(
        via_general_config: ViaGeneralConfig,
        secrets: Secrets,
        genesis_config: GenesisConfig,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            node: ZkStackServiceBuilder::new().context("Cannot create ZkStackServiceBuilder")?,
            configs: via_general_config,
            genesis_config,
            secrets,
        })
    }

    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.node.runtime_handle()
    }

    fn add_sigint_handler_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(SigintHandlerLayer);
        Ok(self)
    }

    fn add_pools_layer(mut self) -> anyhow::Result<Self> {
        let config = try_load_config!(self.configs.postgres_config);
        let secrets = try_load_config!(self.secrets.database);
        let pools_layer = PoolsLayerBuilder::empty(config, secrets)
            .with_master(true)
            .with_replica(true)
            .with_prover(true) // Used by house keeper.
            .build();
        self.node.add_layer(pools_layer);
        Ok(self)
    }

    fn add_postgres_metrics_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(PostgresMetricsLayer);
        Ok(self)
    }

    fn add_object_store_layer(mut self) -> anyhow::Result<Self> {
        let object_store_config = try_load_config!(self.configs.core_object_store);
        self.node
            .add_layer(ObjectStoreLayer::new(object_store_config));
        Ok(self)
    }

    // VIA related layers
    fn add_btc_watcher_layer(mut self) -> anyhow::Result<Self> {
        let btc_watch_config = try_load_config!(self.configs.via_btc_watch_config);
        self.node.add_layer(BtcWatchLayer::new(btc_watch_config));
        Ok(self)
    }

    fn add_btc_sender_layer(mut self) -> anyhow::Result<Self> {
        let btc_sender_config = try_load_config!(self.configs.via_btc_sender_config);
        self.node.add_layer(ViaBtcInscriptionAggregatorLayer::new(
            btc_sender_config.clone(),
        ));
        self.node
            .add_layer(ViaInscriptionManagerLayer::new(btc_sender_config));
        Ok(self)
    }

    pub fn build(self) -> anyhow::Result<ZkStackService> {
        Ok(self
            .add_pools_layer()?
            .add_btc_watcher_layer()?
            .add_btc_sender_layer()?
            .node
            .build())
    }
}
