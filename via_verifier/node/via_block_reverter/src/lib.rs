use std::ops::Mul;

use anyhow::Context;
use via_consensus::consensus::BATCH_FINALIZATION_THRESHOLD;
use via_verifier_dal::{Verifier, VerifierDal};
use zksync_config::configs::via_reorg_detector::ViaReorgDetectorConfig;
use zksync_dal::{Connection, ConnectionPool};
use zksync_types::via_wallet::SystemWallets;

use crate::metrics::METRICS;

mod metrics;

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub struct ViaVerifierBlockReverter {
    config: ViaReorgDetectorConfig,
    pool: ConnectionPool<Verifier>,
}

impl ViaVerifierBlockReverter {
    pub fn new(pool: ConnectionPool<Verifier>, config: ViaReorgDetectorConfig) -> Self {
        METRICS.revert.inc_by(0);

        Self { config, pool }
    }

    pub async fn run(
        mut self,
        mut stop_receiver: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.config.poll_interval().mul(2));
        let pool = self.pool.clone();

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            let mut storage = pool
                .connection_tagged("via verifier block reverter")
                .await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => { /* everything went fine */ }
                Err(err) => {
                    METRICS.errors.inc();
                    tracing::error!("Verifier block reverter failed: {err}");
                }
            }
        }

        tracing::info!("Stop signal received, via_verifier_block_reverter is shutting down");
        Ok(())
    }

    async fn loop_iteration(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<()> {
        let Some((l1_block_number, l1_batch_number)) =
            storage.via_l1_block_dal().has_reorg_in_progress().await?
        else {
            return Ok(());
        };

        self.reorg(l1_block_number, l1_batch_number).await
    }

    pub(crate) async fn reorg(
        &self,
        l1_block_number: i64,
        l1_batch_number: i64,
    ) -> anyhow::Result<()> {
        tracing::info!("Reorg found for l1_block_number {}, l1_batch_number {}, verifier network reverting process started...", l1_block_number, l1_batch_number);

        let mut storage = self.pool.connection().await?;

        let mut transaction = storage.start_transaction().await?;

        let first_removed_height = u32::try_from(l1_block_number)
            .context("Reorg first removed Bitcoin height is outside the supported range")?;
        let l1_block_number_to_keep = l1_block_number
            .checked_sub(1)
            .filter(|height| *height >= 0)
            .context("Reorg must retain a nonnegative Bitcoin height")?;
        transaction
            .via_withdrawal_dal()
            .invalidate_withdrawals_from_l1_height(first_removed_height)
            .await
            .context("Invalidating withdrawal inclusions and acquiring the reorg gate")?;

        // Recompute while the exclusive invalidation gate is held and before
        // deleting deposits or source rows. Older events stored the greatest
        // affected batch, so their metadata is not a safe retained-parent boundary.
        let deposit_batch = transaction
            .via_transactions_dal()
            .get_l1_batch_number_affected_by_reorg(l1_block_number_to_keep)
            .await
            .with_context(|| {
                format!(
                    "Finding first affected batch after Bitcoin height {l1_block_number_to_keep} \
                     (persisted batch metadata {l1_batch_number})"
                )
            })?;
        let source_batch = transaction
            .via_votes_dal()
            .get_l1_batch_number_affected_by_source_reorg(l1_block_number_to_keep)
            .await
            .context("Finding first batch with reorged proof or vote provenance")?;
        let proof_batch = transaction
            .via_votes_dal()
            .get_l1_batch_number_affected_by_proof_reorg(l1_block_number_to_keep)
            .await
            .context("Finding first batch with reorged proof provenance")?;
        let first_deleted_batch = deposit_batch.into_iter().chain(proof_batch).min();
        let first_affected_batch = deposit_batch.into_iter().chain(source_batch).min();
        anyhow::ensure!(
            first_affected_batch.is_some() || l1_batch_number == 0,
            "Cannot recover first affected batch after Bitcoin height {l1_block_number_to_keep}: \
             nonzero persisted batch metadata {l1_batch_number} has no remaining deposit or source evidence"
        );
        if let Some(first_affected_batch) = first_affected_batch {
            transaction
                .via_withdrawal_dal()
                .invalidate_withdrawal_batches_from(
                    u32::try_from(first_affected_batch).context(
                        "First affected withdrawal batch is outside the supported range",
                    )?,
                )
                .await
                .context("Invalidating withdrawal source batches from the first affected batch")?;
        }

        transaction
            .via_l1_block_dal()
            .delete_l1_blocks(l1_block_number_to_keep)
            .await?;
        transaction
            .via_l1_block_dal()
            .delete_l1_reorg(l1_block_number_to_keep)
            .await?;
        transaction
            .via_indexer_dal()
            .update_last_processed_l1_block("via_btc_watch", l1_block_number_to_keep as u32)
            .await?;

        if first_affected_batch.is_some() {
            transaction
                .via_transactions_dal()
                .delete_transactions(l1_block_number_to_keep)
                .await?;
        }
        // A vote-only reorg revokes withdrawal authority, not the canonical
        // proof chain. Deposits and proof anchors determine deletion separately.
        if let Some(first_deleted_batch) = first_deleted_batch {
            transaction
                .via_transactions_dal()
                .reset_transactions(first_deleted_batch - 1)
                .await
                .context("Resetting retained deposits assigned to deleted proof batches")?;
            transaction
                .via_votes_dal()
                .delete_votable_transactions(first_deleted_batch - 1)
                .await
                .context("Deleting proof rows from the first invalid proof or deposit batch")?;
        }
        let retained_vote_parents = transaction
            .via_votes_dal()
            .revert_votes_after_l1_block(l1_block_number_to_keep)
            .await
            .context("Pruning reorged votes while retaining canonical proof and vote provenance")?;
        if !retained_vote_parents.is_empty() {
            let wallets = transaction
                .via_wallet_dal()
                .get_system_wallets_raw(l1_block_number_to_keep)
                .await?
                .context("Missing retained system wallets for canonical vote finalization")?;
            let wallets = SystemWallets::try_from(wallets)
                .context("Parsing retained system wallets for canonical vote finalization")?;
            anyhow::ensure!(
                !wallets.verifiers.is_empty(),
                "Retained verifier set is empty during canonical vote finalization"
            );
            // A removed vote may have been redundant: do not require another
            // inscription to finalize an already sufficient canonical quorum.
            for id in retained_vote_parents {
                transaction
                    .via_votes_dal()
                    .finalize_transaction_if_needed(
                        id,
                        BATCH_FINALIZATION_THRESHOLD,
                        wallets.verifiers.len(),
                    )
                    .await
                    .context("Recomputing finalization from retained canonical votes")?;
            }
        }
        if first_affected_batch.is_some() {
            transaction
                .via_wallet_dal()
                .delete_system_wallet(l1_block_number_to_keep)
                .await?;
        }

        transaction.commit().await?;

        tracing::info!("Verifier reverted successfully");

        Ok(())
    }
}
