use std::sync::Arc;

use via_btc_client::{bootstrap::ViaBootstrap, client::BitcoinClient};
use via_indexer_dal::{Indexer, IndexerDal};
use zksync_config::configs::via_consensus::ViaGenesisConfig;
use zksync_dal::ConnectionPool;
use zksync_types::via_wallet::{SystemWallets, SystemWalletsDetails};

#[derive(Debug)]
pub struct ViaIndexerStorageInitializer {
    pool: ConnectionPool<Indexer>,
    bootstrap: ViaBootstrap,
}

impl ViaIndexerStorageInitializer {
    pub fn new(
        pool: ConnectionPool<Indexer>,
        client: Arc<BitcoinClient>,
        config: ViaGenesisConfig,
    ) -> Self {
        let bootstrap = ViaBootstrap::new(client, config);

        Self { pool, bootstrap }
    }

    pub async fn indexer_wallets(&self) -> anyhow::Result<SystemWallets> {
        if let Some(system_wallets) = self.fetch_indexer_wallets_from_db().await? {
            return Ok(system_wallets);
        }
        Ok(self.init_indexer_wallets().await?)
    }

    pub async fn init_indexer_wallets(&self) -> anyhow::Result<SystemWallets> {
        let state = self.bootstrap.process_bootstrap_messages().await?;

        let indexer_wallets_details = SystemWalletsDetails::try_from(&state)?;

        self.pool
            .connection()
            .await?
            .via_wallet_dal()
            .insert_wallets(&indexer_wallets_details)
            .await?;

        let wallets = state
            .wallets
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Wallets missing when init_indexer_wallets"))?;

        tracing::info!("Loaded the indexer wallets from bootstrap inscriptions");

        Ok(wallets)
    }

    async fn fetch_indexer_wallets_from_db(&self) -> anyhow::Result<Option<SystemWallets>> {
        let system_wallet_raw_opt = self
            .pool
            .connection()
            .await?
            .via_wallet_dal()
            .get_system_wallets_raw()
            .await?;

        let Some(system_wallet_raw) = system_wallet_raw_opt else {
            return Ok(None);
        };

        let parsed_system_wallets = SystemWallets::try_from(system_wallet_raw)?;
        tracing::info!("Loaded the indexer wallets from DB");

        Ok(Some(parsed_system_wallets))
    }
}
