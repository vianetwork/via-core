use anyhow::Context;
use zksync_config::configs::{via_l1_indexer::ViaIndexerConfig, via_wallets::ViaWallets};
use zksync_node_framework::{
    implementations::layers::{
        healtcheck_server::HealthCheckLayer, pools_layer::PoolsLayerBuilder,
        prometheus_exporter::PrometheusExporterLayer, sigint::SigintHandlerLayer,
        via_btc_client::BtcClientLayer, via_l1_indexer::L1IndexerLayer,
    },
    service::{ZkStackService, ZkStackServiceBuilder},
};
use zksync_vlog::prometheus::PrometheusExporterConfig;

pub struct ViaNodeBuilder {
    node: ZkStackServiceBuilder,
    via_indexer_config: ViaIndexerConfig,
}

impl ViaNodeBuilder {
    pub fn new(via_indexer_config: ViaIndexerConfig) -> anyhow::Result<Self> {
        Ok(Self {
            node: ZkStackServiceBuilder::new().context("Cannot create ZkStackServiceBuilder")?,
            via_indexer_config,
        })
    }

    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.node.runtime_handle()
    }

    fn add_sigint_handler_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(SigintHandlerLayer);
        Ok(self)
    }

    fn add_healthcheck_layer(mut self) -> anyhow::Result<Self> {
        let healthcheck_config = self.via_indexer_config.health_check.clone();
        self.node.add_layer(HealthCheckLayer(healthcheck_config));
        Ok(self)
    }

    fn add_prometheus_exporter_layer(mut self) -> anyhow::Result<Self> {
        let prom_config = self.via_indexer_config.prometheus_config.clone();
        let prom_config = PrometheusExporterConfig::pull(prom_config.listener_port);
        self.node.add_layer(PrometheusExporterLayer(prom_config));
        Ok(self)
    }

    fn add_pools_layer(mut self) -> anyhow::Result<Self> {
        let config = self.via_indexer_config.postgres_config.clone();
        let secrets = self
            .via_indexer_config
            .secrets
            .base_secrets
            .database
            .clone()
            .unwrap();
        let pools_layer = PoolsLayerBuilder::empty(config, secrets)
            .with_indexer(true)
            .build();
        self.node.add_layer(pools_layer);
        Ok(self)
    }

    fn add_btc_client_layer(mut self) -> anyhow::Result<Self> {
        let secrets = self.via_indexer_config.secrets.clone();
        let via_btc_client_config = self.via_indexer_config.via_btc_client_config.clone();
        let via_genesis_config = self.via_indexer_config.via_genesis_config.clone();
        self.node.add_layer(BtcClientLayer::new(
            via_btc_client_config,
            secrets.via_l1.unwrap(),
            ViaWallets::default(),
            Some(via_genesis_config.bridge_address.clone()),
        ));
        Ok(self)
    }

    fn add_l1_indexer_layer(mut self) -> anyhow::Result<Self> {
        let via_genesis_config = self.via_indexer_config.via_genesis_config.clone();
        let via_btc_client_config = self.via_indexer_config.via_btc_client_config.clone();
        let via_btc_watch_config = self.via_indexer_config.via_btc_watch_config.clone();
        let indexer_layer = L1IndexerLayer::new(
            via_genesis_config,
            via_btc_client_config,
            via_btc_watch_config,
        );
        self.node.add_layer(indexer_layer);
        Ok(self)
    }
    pub fn build(mut self) -> anyhow::Result<ZkStackService> {
        self = self
            .add_sigint_handler_layer()?
            .add_healthcheck_layer()?
            .add_prometheus_exporter_layer()?
            .add_pools_layer()?
            .add_btc_client_layer()?
            .add_l1_indexer_layer()?;

        Ok(self.node.build())
    }
}
