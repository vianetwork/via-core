mod indexer;
pub mod wallets;

use std::sync::Arc;

use indexer::ViaIndexerInitializer;
use via_btc_client::{bootstrap::ViaBootstrap, client::BitcoinClient};
use wallets::ViaWalletsInitializer;
use zksync_config::{configs::via_consensus::ViaGenesisConfig, ViaBtcWatchConfig};
use zksync_dal::{ConnectionPool, Core};

#[derive(Debug, Clone)]
pub struct ViaMainNodeStorageInitializer {}

impl ViaMainNodeStorageInitializer {
    pub async fn new(
        pool: ConnectionPool<Core>,
        client: Arc<BitcoinClient>,
        via_genesis_config: ViaGenesisConfig,
        btc_watch_config: ViaBtcWatchConfig,
    ) -> anyhow::Result<Self> {
        let bootstrap = ViaBootstrap::new(client, via_genesis_config);

        let wallets = ViaWalletsInitializer::new(pool.clone(), bootstrap.clone());
        let indexer = ViaIndexerInitializer::new(pool, bootstrap, btc_watch_config);

        wallets.initialize_storage().await?;
        indexer.initialize_storage().await?;

        Ok(Self {})
    }
}
