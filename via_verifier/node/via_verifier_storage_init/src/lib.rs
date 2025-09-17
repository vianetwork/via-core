mod genesis;
mod indexer;
pub mod wallets;

use std::sync::Arc;

use genesis::VerifierGenesis;
use via_btc_client::{bootstrap::ViaBootstrap, client::BitcoinClient};
use via_verifier_dal::{ConnectionPool, Verifier};
use wallets::ViaWalletsInitializer;
use zksync_config::{configs::via_consensus::ViaGenesisConfig, GenesisConfig, ViaBtcWatchConfig};

use crate::indexer::ViaIndexerInitializer;

#[derive(Debug, Clone)]
pub struct ViaVerifierStorageInitializer {}

impl ViaVerifierStorageInitializer {
    pub async fn new(
        pool: ConnectionPool<Verifier>,
        client: Arc<BitcoinClient>,
        via_genesis_config: ViaGenesisConfig,
        genesis_config: GenesisConfig,
        btc_watch_config: ViaBtcWatchConfig,
    ) -> anyhow::Result<Self> {
        let genesis = Arc::new(VerifierGenesis {
            genesis_config,
            pool: pool.clone(),
        });

        let bootstrap = ViaBootstrap::new(client, via_genesis_config);
        let indexer = ViaIndexerInitializer::new(pool.clone(), bootstrap.clone(), btc_watch_config);
        let wallets = ViaWalletsInitializer::new(pool, bootstrap);

        genesis.initialize_storage().await?;
        wallets.initialize_storage().await?;
        indexer.initialize_storage().await?;

        Ok(Self {})
    }
}
