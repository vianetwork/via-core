use std::ops::Mul;

use anyhow::Context;

use via_verifier_dal::{Verifier, VerifierDal};
use zksync_config::configs::via_reorg_detector::ViaReorgDetectorConfig;
use zksync_dal::{Connection, ConnectionPool};

use crate::metrics::METRICS;

mod metrics;

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
        if storage
            .via_l1_block_dal()
            .has_reorg_in_progress()
            .await?
            .is_none()
        {
            return Ok(());
        }
        let mut transaction = storage.start_transaction().await?;
        transaction.via_indexer_dal().lock_btc_watch().await?;
        transaction
            .via_indexer_dal()
            .lock_withdrawal_invalidation()
            .await?;
        let Some((l1_block_number, l1_batch_number)) = transaction
            .via_l1_block_dal()
            .has_reorg_in_progress()
            .await?
        else {
            return Ok(());
        };

        tracing::info!("Reorg found for l1_block_number {}, l1_batch_number {}, verifier network reverting process started...", l1_block_number, l1_batch_number);

        let l1_block_number_to_keep = l1_block_number
            .checked_sub(1)
            .filter(|height| *height >= 0)
            .context("Reorg must retain a nonnegative Bitcoin height")?;
        let consumer_progress = transaction
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await
            .context("Reading completed inscription progress under the recovery owner")?;
        let retained_height = u32::try_from(l1_block_number_to_keep)
            .context("Reorg retained Bitcoin height is outside the supported range")?;
        // Via adapts Bitcoin Core's `CWallet::blockDisconnected` consumer-owned processed position.
        // Via caps recovery at completed inscription work because its header scanner can be ahead.
        // https://github.com/bitcoin/bitcoin/blob/f13ae519de94394314cd63a27924b0998ac65d45/src/wallet/wallet.cpp
        let checkpoint_to_keep = consumer_progress.min(u64::from(retained_height)) as u32;

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
            .update_last_processed_l1_block("via_btc_watch", checkpoint_to_keep)
            .await?;

        if l1_batch_number != 0 {
            transaction
                .via_transactions_dal()
                .delete_transactions(l1_block_number_to_keep)
                .await?;
            transaction
                .via_votes_dal()
                .delete_votable_transactions(l1_batch_number)
                .await?;
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
