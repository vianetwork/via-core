use std::{sync::Arc, time::Duration};

use anyhow::Context;
use bitcoin::Block;
use futures::future::try_join_all;
use tokio::time::sleep;
use via_btc_client::{client::BitcoinClient, traits::BitcoinOps};
use zksync_config::configs::via_reorg_detector::ViaReorgDetectorConfig;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};

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

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            self.init().await?;

            let mut storage = pool
                .connection_tagged("via main node reorg detector")
                .await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => { /* everything went fine */ }
                Err(err) => {
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
        let calls = (from_block_height..=to_block_height)
            .map(|height| self.btc_client.fetch_block(height as u128))
            .collect::<Vec<_>>();

        let results = try_join_all(calls).await?;

        Ok(results)
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

    async fn detect_reorg(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
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
            return Ok(());
        }

        tracing::warn!("Reorg detected, find the block affected...");

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

        let l1_batch_number_opt = storage
            .via_l1_block_dal()
            .list_l1_batches_with_priority_txs(reorg_start_block_height as i32)
            .await?;

        if l1_batch_number_opt.is_none() || !self.is_main_node {
            tracing::info!("There is no l1 batch affected by the reorg, no action is required");
            // Reset the indexing because it's possible that verifier transactions are affected.
            let mut transaction = storage.start_transaction().await?;

            // Insert a reorg in the DB to stop all the other components from processing
            transaction
                .via_l1_block_dal()
                .insert_reorg_metadata(reorg_start_block_height, 0)
                .await?;

            // Sleep and wait for the reorg event is received by all components
            sleep(Duration::from_secs(30)).await;

            // Reset the BtcWatch last indexer to the last valid batch.
            transaction
                .via_indexer_dal()
                .update_last_processed_l1_block(
                    "via_btc_watch",
                    (reorg_start_block_height - 1) as u32,
                )
                .await?;

            // Delete the reorg
            transaction.via_l1_block_dal().delete_l1_reorg(0).await?;

            // Delete the affected l1 blocks
            transaction
                .via_l1_block_dal()
                .delete_l1_blocks(reorg_start_block_height - 1)
                .await?;

            transaction.commit().await?;

            return Ok(());
        };

        if self.is_main_node {
            let l1_batch_number = l1_batch_number_opt.unwrap();
            tracing::info!(
                "Reorg detected and affect the VIA network from l1_batch_number {l1_batch_number}"
            );

            storage
                .via_l1_block_dal()
                .insert_reorg_metadata(reorg_start_block_height, l1_batch_number)
                .await?;
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
