use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Context;
use bitcoin::Block;
use tokio::time::sleep;
use via_btc_client::{client::BitcoinClient, traits::BitcoinOps};
use via_reorg::{reorg_window_start, scan_for_reorg, ReorgScan};
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
    pub fn new(config: ViaReorgDetectorConfig, pool: ConnectionPool<Core>, btc_client: Arc<BitcoinClient>, is_main_node: bool) -> Self {
        Self { config, pool, btc_client, is_main_node }
    }

    pub async fn run(mut self, mut stop_receiver: tokio::sync::watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.config.poll_interval());
        let pool = self.pool.clone();

        self.init().await?;

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            let mut storage = pool.connection_tagged("via_main_node_reorg_detector").await?;
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

    /// Fetches canonical Bitcoin blocks and preserves each requested height with its block.
    /// Order is preserved via `buffered` (not `buffer_unordered`), so callers can safely
    /// compare blocks by explicit height instead of Vec position.
    async fn fetch_blocks(&self, from_block_height: i64, to_block_height: i64) -> anyhow::Result<Vec<(i64, Block)>> {
        use futures::stream::{self, StreamExt};

        let heights: Vec<i64> = (from_block_height..=to_block_height).collect();

        let results = stream::iter(heights)
            .map(|height| async move {
                self.btc_client
                    .fetch_block(height as u128)
                    .await
                    .with_context(|| format!("Failed to fetch canonical block at height {height}"))
                    .map(|block| (height, block))
            })
            .buffered(self.config.max_concurrent_fetches())
            .collect::<Vec<_>>()
            .await;

        results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("Failed to fetch canonical L1 block window {from_block_height}..={to_block_height}"))
    }

    async fn init(&mut self) -> anyhow::Result<()> {
        let mut storage = self.pool.connection_tagged("via_main_node_reorg_detector").await?;

        match storage.via_l1_block_dal().get_last_l1_block().await? {
            Some(_) => {}
            None => {
                let block_height = storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await? as i64;

                // `get_last_processed_l1_block` returns 0 before `via_btc_watch`
                // is initialized. Leave `via_l1_blocks` empty until the watch
                // cursor advances to a real Bitcoin height.
                if block_height <= 0 {
                    tracing::warn!(
                        "Skipping via_l1_blocks seed: via_btc_watch has not been \
                         initialized (last_processed_l1_block = {block_height})"
                    );
                } else {
                    let block = self.btc_client.fetch_block(block_height as u128).await?;

                    storage.via_l1_block_dal().insert_l1_block(block_height, block.block_hash().to_string()).await?;
                }
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

    /// Seeds `via_l1_blocks` once `via_btc_watch` has advanced to a real
    /// Bitcoin height. Returns `None` while the watch cursor is uninitialized.
    async fn lazy_bootstrap_first_l1_block(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<Option<(i64, String)>> {
        let cursor = storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await? as i64;
        if cursor <= 0 {
            tracing::debug!("via_l1_blocks empty and via_btc_watch not yet initialized; waiting");
            return Ok(None);
        }
        let block = self.btc_client.fetch_block(cursor as u128).await?;
        let hash = block.block_hash().to_string();
        storage.via_l1_block_dal().insert_l1_block(cursor, hash.clone()).await?;
        tracing::info!("Seeded via_l1_blocks bootstrap row at height {cursor}");
        Ok(Some((cursor, hash)))
    }

    async fn sync_l1_blocks(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
        let (block_height, hash) = match storage.via_l1_block_dal().get_last_l1_block().await? {
            Some(row) => row,
            None => match self.lazy_bootstrap_first_l1_block(storage).await? {
                Some(row) => row,
                None => return Ok(()),
            },
        };

        if !self.is_canonical_chain(block_height, hash).await? {
            tracing::warn!("Last known block {block_height} is not on canonical chain, reorg detection will handle this");
            return Ok(());
        }

        let last_block_height = self.btc_client.fetch_block_height().await? as i64;

        let from_block_height = block_height + 1;
        let to_block_height = std::cmp::min(last_block_height, from_block_height + self.config.block_limit());

        if from_block_height > to_block_height {
            tracing::debug!("No new blocks to sync");
            return Ok(());
        }

        let blocks = self.fetch_blocks(from_block_height, to_block_height).await?;

        let mut transaction = storage.start_transaction().await?;

        for (height, block) in blocks {
            tracing::debug!("Fetched block {height} with hash {}", block.block_hash().to_string());

            transaction.via_l1_block_dal().insert_l1_block(height, block.block_hash().to_string()).await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    async fn detect_reorg(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
        let Some((block_height, _)) = storage.via_l1_block_dal().get_last_l1_block().await? else {
            // Nothing to compare until `via_l1_blocks` is bootstrapped.
            tracing::debug!("Skipping reorg check: via_l1_blocks not yet bootstrapped");
            return Ok(());
        };

        let window = self.config.reorg_window().max(1);
        let start_height = reorg_window_start(block_height, window);

        // `list_l1_blocks` only returns what exists locally — the window can be sparse.
        // We must compare by height, not by position in the result.
        let db_blocks = storage.via_l1_block_dal().list_l1_blocks(start_height, window).await?;

        // A failed canonical fetch is inconclusive; it must not trigger reorg
        // handling.
        let chain_blocks = match self.fetch_blocks(start_height, block_height).await {
            Ok(blocks) => blocks,
            Err(err) => {
                tracing::warn!(
                    "Skipping reorg check ({start_height}..={block_height}): \
                     failed to fetch canonical L1 window: {err:#}",
                );
                return Ok(());
            }
        };

        if chain_blocks.is_empty() {
            return Ok(());
        }

        let canonical_by_height: HashMap<i64, String> =
            chain_blocks.into_iter().map(|(height, block)| (height, block.block_hash().to_string())).collect();

        let reorg_start_block_height = match scan_for_reorg(&db_blocks, &canonical_by_height) {
            ReorgScan::NoReorg => return Ok(()),
            ReorgScan::ReorgAt(height) => {
                tracing::warn!("Reorg detected at block {}", height);
                height
            }
            ReorgScan::SparseAt(height) => {
                // Missing canonical data for a DB-known height is inconclusive.
                tracing::warn!(
                    "Skipping reorg check: canonical chain fetch is missing block {height} \
                     for height-keyed comparison ({start_height}..={block_height} window)"
                );
                return Ok(());
            }
        };

        // `reorg_start_block_height` is the canonical divergence point and drives
        // metrics, reorg metadata, and the affected-batches query. The
        // `via_btc_watch` cursor is only used to bound the rewind target so we
        // never move the cursor forward.
        let last_processed_l1_block = storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await? as i64;

        let l1_batch_number_opt = storage.via_l1_block_dal().list_l1_batches_with_priority_txs(reorg_start_block_height as i32).await?;

        if l1_batch_number_opt.is_none() || !self.is_main_node {
            tracing::info!("Soft reorg detected: no l1 batch affected or not main node");

            storage.via_l1_block_dal().insert_reorg_metadata(reorg_start_block_height, 0).await?;

            sleep(Duration::from_secs(30)).await;

            let mut transaction = storage.start_transaction().await?;

            // Cursor 0 means `via_btc_watch` is uninitialized; fall back to the
            // pre-divergence height. Otherwise cap the keep-target at the cursor
            // so a reorg above the cursor never demotes it forward.
            let l1_block_number_to_keep = if last_processed_l1_block > 0 {
                (reorg_start_block_height - 1).min(last_processed_l1_block)
            } else {
                reorg_start_block_height - 1
            };

            transaction.via_indexer_dal().update_last_processed_l1_block("via_btc_watch", l1_block_number_to_keep as u32).await?;

            transaction.via_l1_block_dal().delete_l1_reorg(l1_block_number_to_keep).await?;

            transaction.via_l1_block_dal().delete_l1_blocks(l1_block_number_to_keep).await?;

            transaction.commit().await?;

            METRICS.reorg_type[&ReorgType::Soft].set(reorg_start_block_height as usize);

            tracing::info!("Soft reorg handled successfully");

            return Ok(());
        };

        if self.is_main_node {
            let l1_batch_number = l1_batch_number_opt.unwrap();
            tracing::warn!("Hard reorg detected: affects L1 batch {} from block {}", l1_batch_number, reorg_start_block_height);

            storage.via_l1_block_dal().insert_reorg_metadata(reorg_start_block_height, l1_batch_number).await?;

            METRICS.reorg_type[&ReorgType::Hard].set(reorg_start_block_height as usize);
        }

        Ok(())
    }

    async fn check_if_reorg_in_progress(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<bool> {
        if let Some((l1_block_number, l1_batch_number)) = storage.via_l1_block_dal().has_reorg_in_progress().await? {
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
