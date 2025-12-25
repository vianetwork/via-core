use std::{sync::Arc, time::Duration};

use anyhow::Context;
use bitcoin::Block;
use tokio::time::sleep;
use via_btc_client::{client::BitcoinClient, traits::BitcoinOps};
use zksync_config::configs::via_reorg_detector::ViaReorgDetectorConfig;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};

use crate::metrics::{ReorgType, METRICS};

mod metrics;

#[derive(Debug)]
pub struct ViaMainNodeReorgDetector {
    config: ViaReorgDetectorConfig,
    pool: ConnectionPool<Core>,
    btc_client: Arc<BitcoinClient>,
    is_main_node: bool,
}

impl ViaMainNodeReorgDetector {
    pub fn new(
        config: ViaReorgDetectorConfig,
        pool: ConnectionPool<Core>,
        btc_client: Arc<BitcoinClient>,
        is_main_node: bool,
    ) -> Self {
        Self {
            config,
            pool,
            btc_client,
            is_main_node,
        }
    }

    pub async fn run(
        mut self,
        mut stop_receiver: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.config.poll_interval());
        let pool = self.pool.clone();

        self.init().await?;

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            let mut storage = pool
                .connection_tagged("via_main_node_reorg_detector")
                .await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => { /* everything went fine */ }
                Err(err) => {
                    METRICS.errors.inc();
                    tracing::error!("Reorg detector failed to fetch new blocks: {err}");
                }
            }
        }

        tracing::info!("Stop signal received, via_main_node_reorg_detector is shutting down");
        Ok(())
    }

    async fn loop_iteration(&mut self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
        if self.check_if_reorg_in_progress(storage).await? {
            return Ok(());
        }

        self.detect_reorg(storage).await?;
        self.sync_l1_blocks(storage).await?;

        Ok(())
    }

    async fn fetch_blocks(
        &self,
        from_block_height: i64,
        to_block_height: i64,
    ) -> anyhow::Result<Vec<Block>> {
        use futures::stream::{self, StreamExt};

        let heights: Vec<i64> = (from_block_height..=to_block_height).collect();

        let results = stream::iter(heights)
            .map(|height| async move { self.btc_client.fetch_block(height as u128).await })
            .buffer_unordered(self.config.max_concurrent_fetches())
            .collect::<Vec<_>>()
            .await;

        let blocks: Result<Vec<_>, _> = results.into_iter().collect();
        blocks.context("Failed to fetch blocks")
    }

    async fn init(&mut self) -> anyhow::Result<()> {
        let mut storage = self
            .pool
            .connection_tagged("via_main_node_reorg_detector")
            .await?;

        match storage.via_l1_block_dal().get_last_l1_block().await? {
            Some(_) => {}
            None => {
                let block_height = storage
                    .via_indexer_dal()
                    .get_last_processed_l1_block("via_btc_watch")
                    .await? as i64;

                let block = self.btc_client.fetch_block(block_height as u128).await?;

                storage
                    .via_l1_block_dal()
                    .insert_l1_block(block_height, block.block_hash().to_string())
                    .await?;
            }
        };

        METRICS.errors.inc_by(0);
        METRICS.reorg_type[&ReorgType::Hard].inc_by(0);
        METRICS.reorg_type[&ReorgType::Soft].inc_by(0);

        Ok(())
    }

    async fn is_canonical_chain(&self, block_height: i64, hash: String) -> anyhow::Result<bool> {
        let blocks = self.fetch_blocks(block_height, block_height).await?;

        let Some(block) = blocks.first() else {
            anyhow::bail!("Cannot fetch the block {}", block_height);
        };

        Ok(block.block_hash().to_string() == hash)
    }

    async fn sync_l1_blocks(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
        let Some((block_height, hash)) = storage.via_l1_block_dal().get_last_l1_block().await?
        else {
            anyhow::bail!("No blocks found to sync blocks")
        };

        if !self.is_canonical_chain(block_height, hash).await? {
            tracing::warn!("Last known block {block_height} is not on canonical chain, reorg detection will handle this");
            return Ok(());
        }

        let last_block_height = self.btc_client.fetch_block_height().await? as i64;

        let from_block_height = block_height + 1;
        let to_block_height = std::cmp::min(
            last_block_height,
            from_block_height + self.config.block_limit(),
        );

        if from_block_height > to_block_height {
            tracing::debug!("No new blocks to sync");
            return Ok(());
        }

        let blocks = self
            .fetch_blocks(from_block_height, to_block_height)
            .await?;

        let mut transaction = storage.start_transaction().await?;

        for (height, block) in (from_block_height..=to_block_height).zip(blocks) {
            tracing::debug!(
                "Fetched block {height} with hash {}",
                block.block_hash().to_string()
            );

            transaction
                .via_l1_block_dal()
                .insert_l1_block(height, block.block_hash().to_string())
                .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    async fn detect_reorg(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
        let Some((block_height, _)) = storage.via_l1_block_dal().get_last_l1_block().await? else {
            anyhow::bail!("No blocks found to detect reorg")
        };

        let window = self.config.reorg_window();
        let start_height = block_height.saturating_sub(window - 1).max(1);

        // Fetch DB blocks in batch
        let db_blocks = storage
            .via_l1_block_dal()
            .list_l1_blocks(start_height, window)
            .await?;

        // Fetch chain blocks in batch
        let chain_blocks = self.fetch_blocks(start_height, block_height).await?;

        let mut reorg_start_block_height_opt = None;

        for ((db_number, db_hash), chain_block) in db_blocks.iter().zip(chain_blocks.iter()) {
            if chain_block.block_hash().to_string() != *db_hash {
                tracing::warn!("Reorg detected at block {}", db_number);
                reorg_start_block_height_opt = Some(*db_number);
                break;
            }
        }

        if reorg_start_block_height_opt.is_none() {
            return Ok(());
        }

        let Some(mut reorg_start_block_height) = reorg_start_block_height_opt else {
            anyhow::bail!("Reorg start block height not found");
        };

        let last_processed_l1_block = storage
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await? as i64;

        if last_processed_l1_block < reorg_start_block_height {
            reorg_start_block_height = last_processed_l1_block;
        }

        METRICS.reorg_type[&ReorgType::Soft]
            .set((block_height - reorg_start_block_height) as usize);

        let l1_batch_number_opt = storage
            .via_l1_block_dal()
            .list_l1_batches_with_priority_txs(reorg_start_block_height as i32)
            .await?;

        if l1_batch_number_opt.is_none() || !self.is_main_node {
            tracing::info!("Soft reorg detected: no l1 batch affected or not main node");

            // Insert reorg metadata to signal other components
            storage
                .via_l1_block_dal()
                .insert_reorg_metadata(reorg_start_block_height, 0)
                .await?;

            // Sleep to allow other components to process the reorg event
            sleep(Duration::from_secs(30)).await;

            let mut transaction = storage.start_transaction().await?;

            let l1_block_number_to_keep = reorg_start_block_height - 1;

            // Reset the BtcWatch indexer to the last valid block
            transaction
                .via_indexer_dal()
                .update_last_processed_l1_block("via_btc_watch", l1_block_number_to_keep as u32)
                .await?;

            // Delete the reorg metadata
            transaction
                .via_l1_block_dal()
                .delete_l1_reorg(l1_block_number_to_keep)
                .await?;

            // Delete the affected l1 blocks
            transaction
                .via_l1_block_dal()
                .delete_l1_blocks(l1_block_number_to_keep)
                .await?;

            transaction.commit().await?;

            METRICS.reorg_type[&ReorgType::Soft].set(reorg_start_block_height as usize);

            tracing::info!("Soft reorg handled successfully");

            return Ok(());
        };

        if self.is_main_node {
            let l1_batch_number = l1_batch_number_opt.unwrap();
            tracing::warn!(
                "Hard reorg detected: affects L1 batch {} from block {}",
                l1_batch_number,
                reorg_start_block_height
            );

            storage
                .via_l1_block_dal()
                .insert_reorg_metadata(reorg_start_block_height, l1_batch_number)
                .await?;

            METRICS.reorg_type[&ReorgType::Hard].set(reorg_start_block_height as usize);
        }

        Ok(())
    }

    async fn check_if_reorg_in_progress(
        &self,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<bool> {
        if let Some((l1_block_number, l1_batch_number)) =
            storage.via_l1_block_dal().has_reorg_in_progress().await?
        {
            tracing::debug!(
                "Reorg in progress at l1 block number: {} and l1 batch number: {} (waiting for external recovery service)",
                l1_block_number,
                l1_batch_number
            );
            return Ok(true);
        }

        Ok(false)
    }
}
