use std::{sync::Arc, time::Duration};

use anyhow::Context;
use bitcoin::Block;
use futures::future::try_join_all;
use tokio::time::sleep;
use via_btc_client::{client::BitcoinClient, traits::BitcoinOps};
use via_verifier_dal::{Verifier, VerifierDal};
use zksync_config::configs::via_reorg_detector::ViaReorgDetectorConfig;
use zksync_dal::{Connection, ConnectionPool};

use crate::metrics::{ReorgInfo, METRICS};

mod metrics;

#[derive(Debug)]
pub struct ViaVerifierReorgDetector {
    config: ViaReorgDetectorConfig,
    pool: ConnectionPool<Verifier>,
    btc_client: Arc<BitcoinClient>,
}

impl ViaVerifierReorgDetector {
    pub fn new(
        config: ViaReorgDetectorConfig,
        pool: ConnectionPool<Verifier>,
        btc_client: Arc<BitcoinClient>,
    ) -> Self {
        Self {
            config,
            pool,
            btc_client,
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
                .connection_tagged("via main node reorg detector")
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

    async fn loop_iteration(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<()> {
        if self.check_if_reorg_in_progress(storage).await? {
            return Ok(());
        }

        if !self.detect_reorg(storage).await? {
            self.sync_l1_blocks(storage).await?;
        }

        Ok(())
    }

    async fn init(&mut self) -> anyhow::Result<()> {
        let mut storage = self
            .pool
            .connection_tagged("via main node reorg detector")
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

        METRICS.reorg_data[&ReorgInfo::StartBlock].set(0);
        METRICS.reorg_data[&ReorgInfo::EndBlock].set(0);
        METRICS.soft_reorg.inc_by(0);
        METRICS.hard_reorg.inc_by(0);

        Ok(())
    }

    async fn fetch_blocks(
        &self,
        from_block_height: i64,
        to_block_height: i64,
    ) -> anyhow::Result<Vec<Block>> {
        let calls = (from_block_height..=to_block_height)
            .map(|height| self.btc_client.fetch_block(height as u128))
            .collect::<Vec<_>>();

        let results = try_join_all(calls).await?;

        Ok(results)
    }

    async fn is_canonical_chain(&self, block_height: i64, hash: String) -> anyhow::Result<bool> {
        let blocks = self.fetch_blocks(block_height, block_height).await?;

        let Some(block) = blocks.first() else {
            anyhow::bail!("Cannot fetch the block {}", block_height);
        };

        Ok(block.block_hash().to_string() == hash)
    }

    async fn sync_l1_blocks(&self, storage: &mut Connection<'_, Verifier>) -> anyhow::Result<()> {
        let Some((block_height, hash)) = storage.via_l1_block_dal().get_last_l1_block().await?
        else {
            anyhow::bail!("No blocks found to sync blocks")
        };

        if !self.is_canonical_chain(block_height, hash).await? {
            tracing::warn!("No canonical chain found from block height {block_height}");
            return Ok(());
        }

        let last_block_height = self.btc_client.fetch_block_height().await? as i64;

        let from_block_height = block_height + 1;
        let to_block_height = std::cmp::min(
            last_block_height,
            from_block_height + self.config.block_limit(),
        );

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

    async fn detect_reorg(&self, storage: &mut Connection<'_, Verifier>) -> anyhow::Result<bool> {
        let Some((block_height, hash)) = storage.via_l1_block_dal().get_last_l1_block().await?
        else {
            anyhow::bail!("No blocks found to detect reorg")
        };

        let block = self
            .btc_client
            .fetch_block(block_height as u128)
            .await
            .with_context(|| format!("failed to fetch block at height {}", block_height))?;

        if hash == block.block_hash().to_string() {
            return Ok(false);
        }

        tracing::warn!(
            "Reorg detected, find the block affected block number {block_height} hash {hash}"
        );

        // Todo: compare with other nodes if the hash is different to confirm the reorg

        let mut i = 1;
        let checkpoint_block = loop {
            let candidate_height = block_height.saturating_sub(i * self.config.reorg_checkpoint());

            if candidate_height == 0 {
                anyhow::bail!("No checkpoint found while searching for reorg checkpoint");
            }

            let Some(hash) = storage
                .via_l1_block_dal()
                .get_l1_block_hash(candidate_height)
                .await?
            else {
                anyhow::bail!("L1 block hash not found at height {candidate_height}");
            };

            let block = self
                .btc_client
                .fetch_block(candidate_height as u128)
                .await?;

            if hash == block.block_hash().to_string() {
                break candidate_height;
            }

            tracing::info!("Reorg checkpoint not found at block: {candidate_height}");

            i += 1;
        };

        tracing::info!("Reorg checkpoint found at block: {checkpoint_block}");

        let from_block_height = checkpoint_block + 1;
        let to_block_height = checkpoint_block + self.config.reorg_checkpoint();

        let blocks = self
            .fetch_blocks(from_block_height, to_block_height)
            .await?;

        let db_blocks = storage
            .via_l1_block_dal()
            .list_l1_blocks(from_block_height, self.config.reorg_checkpoint())
            .await?;

        if blocks.len() != db_blocks.len() {
            anyhow::bail!("Mismatch client and DB blocks");
        }

        let mut reorg_start_block_height_opt = None;

        for (i, (db_number, db_hash)) in db_blocks.iter().enumerate() {
            let client_block = &blocks[i];
            let expected_number = checkpoint_block + 1 + i as i64;

            if *db_number != expected_number {
                anyhow::bail!(
                    "DB returned unexpected block number: got {}, expected {}",
                    db_number,
                    expected_number
                );
            }

            if client_block.block_hash().to_string() != *db_hash {
                reorg_start_block_height_opt = Some(*db_number);
                break;
            }
        }

        let Some(reorg_start_block_height) = reorg_start_block_height_opt else {
            anyhow::bail!("Reorg start block height not found");
        };

        let l1_block_number_to_keep = reorg_start_block_height - 1;

        let l1_batch_number_opt = storage
            .via_transactions_dal()
            .get_l1_batch_number_affected_by_reorg(l1_block_number_to_keep)
            .await?;

        let transactions_count = storage
            .via_transactions_dal()
            .get_not_finalized_transactions(l1_block_number_to_keep)
            .await?;

        METRICS.reorg_data[&ReorgInfo::StartBlock].set(reorg_start_block_height as usize);
        METRICS.reorg_data[&ReorgInfo::EndBlock].set(block_height as usize);

        if l1_batch_number_opt.is_none() && transactions_count == 0 {
            tracing::info!("There is no transactions affected by the reorg, no action is required");

            // Restart the reorg detector from the last processed block range
            let l1_block_number_to_keep = reorg_start_block_height - self.config.block_limit();

            // Insert a reorg in the DB to stop all the other components from processing
            storage
                .via_l1_block_dal()
                .insert_reorg_metadata(l1_block_number_to_keep, 0)
                .await?;

            // Sleep and wait for the reorg event is received by all components
            sleep(Duration::from_secs(30)).await;

            METRICS.soft_reorg.inc();

            tracing::warn!("Soft Reorg found");

            return Ok(false);
        };

        let l1_batch_number = l1_batch_number_opt.unwrap_or_default();

        storage
            .via_l1_block_dal()
            .insert_reorg_metadata(reorg_start_block_height, l1_batch_number)
            .await?;

        METRICS.hard_reorg.inc();
        tracing::warn!("Hard Reorg found");

        Ok(true)
    }

    async fn check_if_reorg_in_progress(
        &self,
        storage: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<bool> {
        if let Some((l1_block_number, l1_batch_number)) =
            storage.via_l1_block_dal().has_reorg_in_progress().await?
        {
            tracing::warn!(
                "Found reorg in progress at l1 block number: {} and l1 batch number: {}",
                l1_block_number,
                l1_batch_number
            );
            return Ok(true);
        }

        Ok(false)
    }
}
