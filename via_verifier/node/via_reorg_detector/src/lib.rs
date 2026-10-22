use std::{collections::HashMap, sync::Arc};

use via_btc_client::{client::BitcoinClient, traits::BitcoinOps};
use via_reorg::{reorg_window_start, scan_for_reorg, ReorgScan};
use via_verifier_dal::{via_indexer_dal::BTC_WATCH_OWNER, Verifier, VerifierDal};
use zksync_config::configs::via_reorg_detector::ViaReorgDetectorConfig;
use zksync_dal::{Connection, ConnectionPool};

use crate::metrics::{ReorgInfo, METRICS};

mod metrics;

#[cfg(test)]
mod tests;

#[derive(Debug)]
enum ReorgKind {
    Soft,
    Hard,
}

// Adapt Graph Node's early reorg outcome and Chainlink's metrics-after-success ordering.
// Via defers reorg metrics until the outer commit because the transaction can still roll back.
// https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/chain/ethereum/src/polling_block_stream.rs#L181-L186
// https://github.com/smartcontractkit/chainlink-evm/blob/ea52d84d2f7aecf8b3dfb9cc88db1ab6e6cd6ba2/pkg/logpoller/observability.go#L60-L66
#[derive(Debug)]
struct DetectedReorg {
    kind: ReorgKind,
    start_block: i64,
    end_block: i64,
}

#[derive(Debug)]
pub struct ViaVerifierReorgDetector {
    config: ViaReorgDetectorConfig,
    pool: ConnectionPool<Verifier>,
    btc_client: Arc<BitcoinClient>,
}

impl ViaVerifierReorgDetector {
    pub fn new(config: ViaReorgDetectorConfig, pool: ConnectionPool<Verifier>, btc_client: Arc<BitcoinClient>) -> Self {
        Self { config, pool, btc_client }
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

            let mut storage = pool.connection_tagged("via_verifier_reorg_detector").await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => { /* everything went fine */ }
                Err(err) => {
                    METRICS.errors.inc();
                    tracing::error!("Reorg detector failed to fetch new blocks: {err}");
                }
            }
        }

        tracing::info!("Stop signal received, via_verifier_reorg_detector is shutting down");
        Ok(())
    }

    async fn loop_iteration(&mut self, storage: &mut Connection<'_, Verifier>) -> anyhow::Result<()> {
        let mut transaction = BTC_WATCH_OWNER.begin(storage).await?;
        let reorg = self.process_chain(&mut transaction).await?;
        transaction.commit().await?;
        if let Some(reorg) = reorg {
            METRICS.reorg_data[&ReorgInfo::StartBlock].set(reorg.start_block as usize);
            METRICS.reorg_data[&ReorgInfo::EndBlock].set(reorg.end_block as usize);
            match reorg.kind {
                ReorgKind::Soft => METRICS.soft_reorg.inc(),
                ReorgKind::Hard => METRICS.hard_reorg.inc(),
            };
        }
        Ok(())
    }

    async fn process_chain(&mut self, storage: &mut Connection<'_, Verifier>) -> anyhow::Result<Option<DetectedReorg>> {
        if self.check_if_reorg_in_progress(storage).await? {
            return Ok(None);
        }

        if let Some(reorg) = self.detect_reorg(storage).await? {
            return Ok(Some(reorg));
        }

        self.sync_l1_blocks(storage).await?;
        Ok(None)
    }

    async fn init(&mut self) -> anyhow::Result<()> {
        let mut storage = self.pool.connection_tagged("via_verifier_reorg_detector").await?;

        let mut transaction = BTC_WATCH_OWNER.begin(&mut storage).await?;
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
        METRICS.reorg_data[&ReorgInfo::StartBlock].set(0);
        METRICS.reorg_data[&ReorgInfo::EndBlock].set(0);
        METRICS.soft_reorg.inc_by(0);
        METRICS.hard_reorg.inc_by(0);

        Ok(())
    }

    // jscpd:ignore-start Separate DAL types and recovery per node role. Hash fetch lives in via_btc_client.
    async fn is_canonical_chain(&self, block_height: i64, hash: String) -> anyhow::Result<bool> {
        let blocks = self.btc_client.fetch_block_window(block_height, block_height, self.config.max_concurrent_fetches()).await?;

        let Some((_, block)) = blocks.first() else {
            anyhow::bail!("Cannot fetch the block {}", block_height);
        };

        Ok(block.to_string() == hash)
    }

    /// Seeds `via_l1_blocks` once `via_btc_watch` has advanced to a real
    /// Bitcoin height. Returns `None` while the watch cursor is uninitialized.
    async fn lazy_bootstrap_first_l1_block(&self, storage: &mut Connection<'_, Verifier>) -> anyhow::Result<Option<(i64, String)>> {
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

    async fn sync_l1_blocks(&self, storage: &mut Connection<'_, Verifier>) -> anyhow::Result<()> {
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

        let blocks = self.btc_client.fetch_block_window(from_block_height, to_block_height, self.config.max_concurrent_fetches()).await?;

        let mut transaction = storage.start_transaction().await?;

        for (height, hash) in blocks {
            tracing::debug!("Fetched block {height} with hash {hash}");

            transaction.via_l1_block_dal().insert_l1_block(height, hash.to_string()).await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    async fn detect_reorg(
        &self,
        storage: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<Option<DetectedReorg>> {
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
        let chain_blocks = match self.btc_client.fetch_block_window(start_height, block_height, self.config.max_concurrent_fetches()).await
        {
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
            chain_blocks.into_iter().map(|(height, hash)| (height, hash.to_string())).collect();

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

        // `reorg_start_block_height` is the canonical divergence point and drives
        // metrics, reorg metadata, and the affected-batches/transactions queries.
        // The `via_btc_watch` cursor is only used to bound the keep-target so a
        // reorg above the cursor never widens the affected range forward.
        let last_processed_l1_block = storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await? as i64;

        // Cursor 0 means `via_btc_watch` is uninitialized; fall back to the
        // pre-divergence height. Otherwise cap at the cursor so the affected
        // range cannot extend past blocks the indexer has not yet seen.
        let l1_block_number_to_keep = if last_processed_l1_block > 0 {
            (reorg_start_block_height - 1).min(last_processed_l1_block)
        } else {
            reorg_start_block_height - 1
        };

        let deposit_batch = storage.via_transactions_dal().get_l1_batch_number_affected_by_reorg(l1_block_number_to_keep).await?;
        let source_batch = storage.via_votes_dal().get_l1_batch_number_affected_by_source_reorg(l1_block_number_to_keep).await?;
        let l1_batch_number_opt = deposit_batch.into_iter().chain(source_batch).min();

        let transactions_count = storage.via_transactions_dal().get_not_finalized_transactions(l1_block_number_to_keep).await?;

        if l1_batch_number_opt.is_none() && transactions_count == 0 {
            tracing::info!("Soft reorg detected: no transactions affected");

            storage.via_l1_block_dal().insert_reorg_metadata(reorg_start_block_height, 0).await?;

            return Ok(Some(DetectedReorg { kind: ReorgKind::Soft, start_block: reorg_start_block_height, end_block: block_height }));
        };

        let l1_batch_number = l1_batch_number_opt.unwrap_or_default();

        tracing::warn!("Hard reorg detected: affects L1 batch {} and {} transactions", l1_batch_number, transactions_count);

        storage.via_l1_block_dal().insert_reorg_metadata(reorg_start_block_height, l1_batch_number).await?;

        Ok(Some(DetectedReorg { kind: ReorgKind::Hard, start_block: reorg_start_block_height, end_block: block_height }))
    }
    // jscpd:ignore-end

    async fn check_if_reorg_in_progress(&self, storage: &mut Connection<'_, Verifier>) -> anyhow::Result<bool> {
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
