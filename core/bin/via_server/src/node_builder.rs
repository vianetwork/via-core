use anyhow::Context;
use zksync_config::{
    configs::{PostgresConfig, Secrets},
    BtcWatchConfig,
};
use zksync_node_framework::{
    implementations::layers::{btc_watch::BtcWatchLayer, pools_layer::PoolsLayerBuilder},
    service::{ZkStackService, ZkStackServiceBuilder},
};

pub struct NodeBuilder {
    node: ZkStackServiceBuilder,
    postgres_config: PostgresConfig,
    secrets: Secrets,
}

impl NodeBuilder {
    pub fn new(postgres_config: PostgresConfig, secrets: Secrets) -> anyhow::Result<Self> {
        Ok(Self {
            node: ZkStackServiceBuilder::new().context("Cannot create ZkStackServiceBuilder")?,
            postgres_config,
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
        let btc_watch_config = BtcWatchConfig::for_tests();
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
