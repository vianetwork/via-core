use via_btc_client::bootstrap::ViaBootstrap;
use zksync_config::ViaBtcWatchConfig;
use zksync_dal::{ConnectionPool, Core, CoreDal};

#[derive(Debug, Clone)]
pub struct ViaIndexerInitializer {
    pool: ConnectionPool<Core>,
    bootstrap: ViaBootstrap,
    btc_watch_config: ViaBtcWatchConfig,
}

impl ViaIndexerInitializer {
    pub fn new(
        pool: ConnectionPool<Core>,
        bootstrap: ViaBootstrap,
        btc_watch_config: ViaBtcWatchConfig,
    ) -> Self {
        Self {
            pool,
            bootstrap,
            btc_watch_config,
        }
    }
}

impl ViaIndexerInitializer {
    pub async fn initialize_storage(&self) -> anyhow::Result<()> {
        let mut storage = self.pool.connection().await?;
        let mut transaction = storage.start_transaction().await?;
        transaction.via_indexer_dal().lock_btc_watch().await?;
        let is_initialized = transaction
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await?
            != 0;

        if !is_initialized {
            let state = self.bootstrap.process_bootstrap_messages().await?;
            transaction
                .via_indexer_dal()
                .init_indexer_metadata("via_btc_watch", state.starting_block_number)
                .await?;
            tracing::info!("Indexer storage initialized");
        } else if is_initialized && self.btc_watch_config.restart_indexing {
            transaction
                .via_indexer_dal()
                .update_last_processed_l1_block(
                    "via_btc_watch",
                    self.btc_watch_config.start_l1_block_number,
                )
                .await?;
            tracing::info!(
                "Update the indexer start block number {}",
                self.btc_watch_config.start_l1_block_number
            );
        }

        transaction.commit().await?;
        Ok(())
    }
}
