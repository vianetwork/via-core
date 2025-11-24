mod message_processors;
mod metrics;

use std::sync::Arc;

use message_processors::{GovernanceUpgradesEventProcessor, WithdrawalProcessor};
use tokio::sync::watch;
// re-export via_btc_client types
pub use via_btc_client::types::BitcoinNetwork;
use via_btc_client::{client::BitcoinClient, indexer::BitcoinInscriptionIndexer};
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use via_verifier_types::protocol_version::check_if_supported_sequencer_version;
use zksync_config::{configs::via_btc_watch::L1_BLOCKS_CHUNK, ViaBtcWatchConfig};
use zksync_types::via_wallet::SystemWallets;

use self::message_processors::{MessageProcessor, MessageProcessorError};
use crate::{
    message_processors::{L1ToL2MessageProcessor, SystemWalletProcessor, VerifierMessageProcessor},
    metrics::METRICS,
};

#[cfg(test)]
mod test;

#[derive(Debug)]
pub struct VerifierBtcWatch {
    config: ViaBtcWatchConfig,
    indexer: BitcoinInscriptionIndexer,
    pool: ConnectionPool<Verifier>,
    system_wallet_processor: Box<dyn MessageProcessor>,
    message_processors: Vec<Box<dyn MessageProcessor>>,
}

impl VerifierBtcWatch {
    pub async fn new(
        config: ViaBtcWatchConfig,
        indexer: BitcoinInscriptionIndexer,
        btc_client: Arc<BitcoinClient>,
        pool: ConnectionPool<Verifier>,
    ) -> anyhow::Result<Self> {
        let system_wallet_processor = Box::new(SystemWalletProcessor::new(btc_client.clone()));

        let message_processors: Vec<Box<dyn MessageProcessor>> = vec![
            Box::new(GovernanceUpgradesEventProcessor::new(btc_client)),
            Box::new(L1ToL2MessageProcessor::default()),
            Box::new(VerifierMessageProcessor::default()),
            Box::new(WithdrawalProcessor::default()),
        ];

        Ok(Self {
            config,
            indexer,
            pool,
            system_wallet_processor,
            message_processors,
        })
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.config.poll_interval());
        let pool = self.pool.clone();

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            let mut storage = pool
                .connection_tagged(VerifierBtcWatch::module_name())
                .await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => { /* everything went fine */ }
                Err(err) => {
                    METRICS.errors.inc();
                    tracing::error!("Error processing new blocks: {err:?}");
                }
            }
        }

        tracing::info!("Stop signal received, via_btc_watch is shutting down");
        Ok(())
    }

    async fn loop_iteration(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> Result<(), MessageProcessorError> {
        if storage
            .via_l1_block_dal()
            .has_reorg_in_progress()
            .await?
            .is_some()
        {
            return Ok(());
        }

        let last_processed_bitcoin_block = storage
            .via_indexer_dal()
            .get_last_processed_l1_block(VerifierBtcWatch::module_name())
            .await? as u32;

        if last_processed_bitcoin_block == 0 {
            return Err(MessageProcessorError::Internal(anyhow::anyhow!(
                "The indexer was not initialized".to_string()
            )));
        }

        if let Some(last_protocol_version) = storage
            .via_protocol_versions_dal()
            .latest_protocol_semantic_version()
            .await
            .expect("Error load the protocol version")
        {
            check_if_supported_sequencer_version(last_protocol_version)
                .map_err(|e| MessageProcessorError::Internal(anyhow::anyhow!(e.to_string())))?;
        }

        let current_l1_block_number =
            self.indexer
                .fetch_block_height()
                .await
                .map_err(|e| MessageProcessorError::Internal(anyhow::anyhow!(e.to_string())))?
                .saturating_sub(self.config.block_confirmations) as u32;
        if current_l1_block_number <= last_processed_bitcoin_block {
            return Ok(());
        }

        let Some((last_l1_block_number, _)) =
            storage.via_l1_block_dal().get_last_l1_block().await?
        else {
            tracing::warn!("Reorg did not start yet");
            return Ok(());
        };

        let mut to_block = last_processed_bitcoin_block + L1_BLOCKS_CHUNK;
        if to_block > current_l1_block_number {
            to_block = current_l1_block_number;
        }

        // Clamp the to_batch to the last valid block number validated by the reorg detector
        if to_block > last_l1_block_number as u32 {
            to_block = last_l1_block_number as u32;
        }

        let from_block = last_processed_bitcoin_block + 1;

        let system_wallets_map = match storage
            .via_wallet_dal()
            .get_system_wallets_raw(last_processed_bitcoin_block as i64)
            .await?
        {
            Some(map) => map,
            None => {
                tracing::info!("Wait for storage init, block number {}", from_block);
                return Ok(());
            }
        };

        let system_wallets = SystemWallets::try_from(system_wallets_map)?;

        self.indexer.update_system_wallets(
            Some(system_wallets.sequencer),
            Some(system_wallets.bridge),
            Some(system_wallets.verifiers),
            Some(system_wallets.governance),
        );

        let mut messages = self
            .indexer
            .process_blocks(from_block, to_block)
            .await
            .map_err(|e| MessageProcessorError::Internal(e.into()))?;

        // Re-process blocks if system wallets were updated, since the new wallet state
        // may change how subsequent messages are interpreted.
        if let Some(block_number) = self
            .system_wallet_processor
            .process_messages(storage, messages.clone(), &mut self.indexer)
            .await
            .map_err(|e| MessageProcessorError::Internal(e.into()))?
        {
            // Process the blocks until where the update wallets block.
            to_block = block_number;

            messages = self
                .indexer
                .process_blocks(from_block, to_block)
                .await
                .map_err(|e| MessageProcessorError::Internal(e.into()))?;
        }

        for processor in self.message_processors.iter_mut() {
            processor
                .process_messages(storage, messages.clone(), &mut self.indexer)
                .await
                .map_err(|e| MessageProcessorError::Internal(e.into()))?;
        }

        // Check if the last processed block was updated by another thread. This could happen when a reorg is detected.
        let current_last_processed_bitcoin_block = storage
            .via_indexer_dal()
            .get_last_processed_l1_block(VerifierBtcWatch::module_name())
            .await? as u32;

        if current_last_processed_bitcoin_block != last_processed_bitcoin_block {
            tracing::info!(
                "The btc_watch last processed block was updated by another thread, skipping the block processing"
            );
            return Ok(());
        }

        storage
            .via_indexer_dal()
            .update_last_processed_l1_block(VerifierBtcWatch::module_name(), to_block)
            .await
            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

        tracing::info!(
            "The btc_watch processed blocks, from {} to {}",
            from_block,
            to_block,
        );

        Ok(())
    }

    fn module_name() -> &'static str {
        "via_btc_watch"
    }
}
