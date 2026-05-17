use std::{collections::HashMap, sync::Arc, time::Duration};

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
    ) -> anyhow::Result<Vec<(i64, Block)>> {
        use futures::stream::{self, StreamExt};

        let heights: Vec<i64> = (from_block_height..=to_block_height).collect();

        let results = stream::iter(heights)
            .map(|height| async move {
                self.btc_client
                    .fetch_block(height as u128)
                    .await
                    .map(|block| (height, block))
            })
            .buffer_unordered(self.config.max_concurrent_fetches())
            .collect::<Vec<_>>()
            .await;

        let mut blocks = results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to fetch blocks")?;
        blocks.sort_by_key(|(height, _)| *height);

        Ok(blocks)
    }

    async fn insert_l1_block_hashes(
        &self,
        storage: &mut Connection<'_, Core>,
        blocks: &[(i64, String)],
    ) -> anyhow::Result<()> {
        let mut transaction = storage.start_transaction().await?;
        for (height, hash) in blocks {
            transaction
                .via_l1_block_dal()
                .insert_l1_block(*height, hash.clone())
                .await?;
        }
        transaction.commit().await?;

        Ok(())
    }

    async fn insert_l1_block_window(
        &self,
        storage: &mut Connection<'_, Core>,
        block_height: i64,
    ) -> anyhow::Result<()> {
        let start_height = reorg_window_start(block_height, self.config.reorg_window());
        let blocks = self.fetch_blocks(start_height, block_height).await?;
        let blocks = blocks
            .iter()
            .map(|(height, block)| (*height, block.block_hash().to_string()))
            .collect::<Vec<_>>();
        self.insert_l1_block_hashes(storage, &blocks).await
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

                self.insert_l1_block_window(&mut storage, block_height)
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

        let Some((_, block)) = blocks.first() else {
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

        for (height, block) in blocks {
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
        let start_height = reorg_window_start(block_height, window);

        // Fetch DB blocks in batch
        let db_blocks = storage
            .via_l1_block_dal()
            .list_l1_blocks(start_height, window)
            .await?;

        // Fetch chain blocks in batch
        let chain_blocks = self.fetch_blocks(start_height, block_height).await?;
        let chain_blocks = chain_blocks
            .iter()
            .map(|(height, block)| (*height, block.block_hash().to_string()))
            .collect::<Vec<_>>();

        let reorg_start_block_height_opt =
            find_reorg_start_height(&db_blocks, &chain_blocks, start_height, block_height)?;

        if reorg_start_block_height_opt.is_none() {
            if db_blocks.len() != chain_blocks.len() {
                self.insert_l1_block_hashes(storage, &chain_blocks).await?;
            }
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

fn reorg_window_start(block_height: i64, window: i64) -> i64 {
    if block_height <= 1 {
        block_height
    } else {
        block_height.saturating_sub(window.saturating_sub(1)).max(1)
    }
}

fn find_reorg_start_height(
    db_blocks: &[(i64, String)],
    chain_blocks: &[(i64, String)],
    start_height: i64,
    end_height: i64,
) -> anyhow::Result<Option<i64>> {
    let expected_len = (end_height - start_height + 1) as usize;
    if chain_blocks.len() != expected_len {
        anyhow::bail!(
            "Fetched {} canonical L1 blocks for expected window {}..={}",
            chain_blocks.len(),
            start_height,
            end_height
        );
    }

    let chain_blocks_by_height = chain_blocks
        .iter()
        .map(|(height, hash)| (*height, hash.as_str()))
        .collect::<HashMap<_, _>>();

    for height in start_height..=end_height {
        if !chain_blocks_by_height.contains_key(&height) {
            anyhow::bail!("Canonical L1 block window is missing height {}", height);
        }
    }

    if db_blocks.len() != expected_len {
        tracing::warn!(
            "Sparse via_l1_blocks reorg window {}..={}: found {} of {} DB rows; comparing only stored heights",
            start_height,
            end_height,
            db_blocks.len(),
            expected_len
        );
    }

    for (db_number, db_hash) in db_blocks {
        let Some(chain_hash) = chain_blocks_by_height.get(db_number) else {
            anyhow::bail!(
                "DB L1 block {} is outside canonical reorg window",
                db_number
            );
        };

        if *chain_hash != db_hash {
            tracing::warn!("Reorg detected at block {}", db_number);
            return Ok(Some(*db_number));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain_window(start_height: i64, end_height: i64) -> Vec<(i64, String)> {
        (start_height..=end_height)
            .map(|height| (height, format!("hash-{height}")))
            .collect()
    }

    #[test]
    fn sparse_db_window_compares_by_height() {
        let chain_blocks = chain_window(100792, 100891);
        let db_blocks = vec![(100891, "hash-100891".to_string())];

        let reorg_start =
            find_reorg_start_height(&db_blocks, &chain_blocks, 100792, 100891).unwrap();

        assert_eq!(reorg_start, None);
    }

    #[test]
    fn sparse_db_window_detects_mismatch_at_stored_height() {
        let chain_blocks = chain_window(100792, 100891);
        let db_blocks = vec![(100891, "stale-hash".to_string())];

        let reorg_start =
            find_reorg_start_height(&db_blocks, &chain_blocks, 100792, 100891).unwrap();

        assert_eq!(reorg_start, Some(100891));
    }
}
