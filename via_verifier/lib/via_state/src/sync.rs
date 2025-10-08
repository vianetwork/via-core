use std::sync::Arc;

use via_btc_client::traits::BitcoinOps;
use via_verifier_dal::{Verifier, VerifierDal};
use zksync_config::ViaBtcWatchConfig;
use zksync_dal::ConnectionPool;

#[derive(Debug, Clone)]
pub struct ViaState {
    pool: ConnectionPool<Verifier>,
    btc_client: Arc<dyn BitcoinOps>,
    via_btc_watch_config: ViaBtcWatchConfig,
}

impl ViaState {
    pub fn new(
        pool: ConnectionPool<Verifier>,
        btc_client: Arc<dyn BitcoinOps>,
        via_btc_watch_config: ViaBtcWatchConfig,
    ) -> Self {
        Self {
            pool,
            btc_client,
            via_btc_watch_config,
        }
    }

    /// Check if the btc_watch is in sync with the current Bitcoin node
    pub async fn is_sync_in_progress(&self) -> anyhow::Result<bool> {
        let last_indexed_l1_block_number = self
            .pool
            .connection_tagged("verifier task")
            .await?
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await?;
        let current_l1_block_number = self.btc_client.fetch_block_height().await?;

        Ok(
            current_l1_block_number - self.via_btc_watch_config.block_confirmations
                > last_indexed_l1_block_number,
        )
    }

    /// Check if there is a reorg in progress
    pub async fn is_reorg_in_progress(&self) -> anyhow::Result<bool> {
        let is_reorg = self
            .pool
            .connection()
            .await?
            .via_l1_block_dal()
            .has_reorg_in_progress()
            .await?
            .is_some();

        Ok(is_reorg)
    }
}
