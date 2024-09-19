use anyhow::Context;
use zksync_config::{
    configs::{PostgresConfig, Secrets},
    GenesisConfig, ViaBtcWatchConfig, ViaGeneralConfig,
};
use zksync_node_framework::{
    implementations::layers::{pools_layer::PoolsLayerBuilder, via_btc_watch::BtcWatchLayer},
    service::{ZkStackService, ZkStackServiceBuilder},
};

pub struct NodeBuilder {
    node: ZkStackServiceBuilder,
    postgres_config: PostgresConfig,
    secrets: Secrets,
}

impl NodeBuilder {
    pub fn new(
        via_general_config: ViaGeneralConfig,
        secrets: Secrets,
        genesis_config: GenesisConfig,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            node: ZkStackServiceBuilder::new().context("Cannot create ZkStackServiceBuilder")?,
            postgres_config: via_general_config.postgres_config.unwrap(),
            secrets,
        })
    }

    fn add_pools_layer(mut self) -> anyhow::Result<Self> {
        let pools_layer = PoolsLayerBuilder::empty(
            self.postgres_config.clone(),
            self.secrets
                .database
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Database secrets are not provided"))?,
        )
        .with_master(true)
        .with_replica(true)
        .build();
        self.node.add_layer(pools_layer);
        Ok(self)
    }

    fn add_btc_watcher_layer(mut self) -> anyhow::Result<Self> {
        let btc_watch_config = ViaBtcWatchConfig::for_tests();
        self.node.add_layer(BtcWatchLayer::new(btc_watch_config));
        Ok(self)
    }

    pub fn build(self) -> anyhow::Result<ZkStackService> {
        Ok(self
            .add_pools_layer()?
            .add_btc_watcher_layer()?
            .node
            .build())
    }
}
