use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use bitcoin::Block;
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
        let mut transaction = storage.start_transaction().await?;
        transaction.via_indexer_dal().lock_btc_watch().await?;
        let reorg = match transaction.via_l1_block_dal().has_reorg_in_progress().await? {
            Some(reorg) => Some(reorg),
            None => self.detect_reorg(&mut transaction).await?,
        };
        if reorg.is_none() {
            self.sync_l1_blocks(&mut transaction).await?;
        }
        transaction.commit().await?;

        // Committed detection remains visible if soft recovery is interrupted.
        // A fresh transaction rereads pending work under the owner, including after restart.
        if matches!(reorg, Some((_, 0))) {
            self.complete_soft_reorg(storage).await?;
        }
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
        let mut transaction = storage.start_transaction().await?;
        transaction.via_indexer_dal().lock_btc_watch().await?;
        let storage = &mut transaction;

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

        transaction.commit().await?;
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

    async fn detect_reorg(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<Option<(i64, i64)>> {
        let Some((block_height, _)) = storage.via_l1_block_dal().get_last_l1_block().await? else {
            // Nothing to compare until `via_l1_blocks` is bootstrapped.
            tracing::debug!("Skipping reorg check: via_l1_blocks not yet bootstrapped");
            return Ok(None);
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
                return Ok(None);
            }
        };

        if chain_blocks.is_empty() {
            return Ok(None);
        }

        let canonical_by_height: HashMap<i64, String> =
            chain_blocks.into_iter().map(|(height, block)| (height, block.block_hash().to_string())).collect();

        let reorg_start_block_height = match scan_for_reorg(&db_blocks, &canonical_by_height) {
            ReorgScan::NoReorg => return Ok(None),
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
                return Ok(None);
            }
        };

        let l1_batch_number_opt = storage.via_l1_block_dal().list_l1_batches_with_priority_txs(reorg_start_block_height as i32).await?;

        if l1_batch_number_opt.is_none() || !self.is_main_node {
            tracing::info!("Soft reorg detected: no l1 batch affected or not main node");

            storage.via_l1_block_dal().insert_reorg_metadata(reorg_start_block_height, 0).await?;

            return Ok(Some((reorg_start_block_height, 0)));
        };

        if self.is_main_node {
            let l1_batch_number = l1_batch_number_opt.unwrap();
            tracing::warn!("Hard reorg detected: affects L1 batch {} from block {}", l1_batch_number, reorg_start_block_height);

            storage.via_l1_block_dal().insert_reorg_metadata(reorg_start_block_height, l1_batch_number).await?;

            METRICS.reorg_type[&ReorgType::Hard].set(reorg_start_block_height as usize);
            return Ok(Some((reorg_start_block_height, l1_batch_number)));
        }

        Ok(None)
    }

    async fn complete_soft_reorg(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
        let mut transaction = storage.start_transaction().await?;
        transaction.via_indexer_dal().lock_btc_watch().await?;
        let Some((first_removed_height, 0)) = transaction.via_l1_block_dal().has_reorg_in_progress().await? else {
            return Ok(());
        };
        let reader_progress = transaction.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?;
        let canonical_boundary = first_removed_height
            .checked_sub(1)
            .filter(|height| *height >= 0)
            .context("Soft reorg must retain a nonnegative Bitcoin height")?;
        let keep_height = reader_progress.min(canonical_boundary as u64);
        if reader_progress != 0 {
            transaction
                .via_indexer_dal()
                .update_last_processed_l1_block(
                    "via_btc_watch",
                    u32::try_from(keep_height).context("Soft recovery checkpoint exceeds the supported height range")?,
                )
                .await?;
        }
        transaction.via_l1_block_dal().delete_l1_reorg(canonical_boundary).await?;
        transaction.via_l1_block_dal().delete_l1_blocks(canonical_boundary).await?;
        transaction.commit().await?;
        METRICS.reorg_type[&ReorgType::Soft].set(first_removed_height as usize);
        tracing::info!("Soft reorg handled successfully");
        Ok(())
    }
}
