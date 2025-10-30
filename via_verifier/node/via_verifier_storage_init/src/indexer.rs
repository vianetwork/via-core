use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use zksync_config::ViaBtcWatchConfig;
use zksync_types::via_bootstrap::BootstrapState;

#[derive(Debug, Clone)]
pub struct ViaIndexerInitializer {
    pool: ConnectionPool<Verifier>,
    bootstrap_state: BootstrapState,
    btc_watch_config: ViaBtcWatchConfig,
}

impl ViaIndexerInitializer {
    pub fn new(
        pool: ConnectionPool<Verifier>,
        bootstrap_state: BootstrapState,
        btc_watch_config: ViaBtcWatchConfig,
    ) -> Self {
        Self {
            pool,
            bootstrap_state,
            btc_watch_config,
        }
    }
}

impl ViaIndexerInitializer {
    pub async fn is_initialized(&self) -> anyhow::Result<bool> {
        let last_processed_bitcoin_block = self
            .pool
            .connection()
            .await?
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await? as u32;

        Ok(last_processed_bitcoin_block != 0)
    }

    pub async fn initialize_storage(&self) -> anyhow::Result<()> {
        let is_initialized = self.is_initialized().await?;

        if !is_initialized {
            self.pool
                .connection()
                .await?
                .via_indexer_dal()
                .init_indexer_metadata("via_btc_watch", self.bootstrap_state.starting_block_number)
                .await?;
        } else if is_initialized && self.btc_watch_config.restart_indexing {
            self.pool
                .connection()
                .await?
                .via_indexer_dal()
                .update_last_processed_l1_block(
                    "via_btc_watch",
                    self.btc_watch_config.start_l1_block_number,
                )
                .await?;

            tracing::info!("Indexer storage initialized");
        }

        Ok(())
    }
}
