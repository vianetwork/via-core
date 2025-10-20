mod message_processors;
mod metrics;

use std::sync::Arc;

use message_processors::GovernanceUpgradesEventProcessor;
use tokio::sync::watch;
// re-export via_btc_client types
pub use via_btc_client::types::BitcoinNetwork;
use via_btc_client::{client::BitcoinClient, indexer::BitcoinInscriptionIndexer};
use zksync_config::{configs::via_btc_watch::L1_BLOCKS_CHUNK, ViaBtcWatchConfig};
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};

#[cfg(test)]
mod test;

use self::message_processors::{
    L1ToL2MessageProcessor, MessageProcessor, MessageProcessorError, VotableMessageProcessor,
};
use crate::message_processors::SystemWalletProcessor;

#[derive(Debug)]
pub struct BtcWatch {
    btc_watch_config: ViaBtcWatchConfig,
    indexer: BitcoinInscriptionIndexer,
    pool: ConnectionPool<Core>,
    system_wallet_processor: Box<dyn MessageProcessor>,
    message_processors: Vec<Box<dyn MessageProcessor>>,
}

impl BtcWatch {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        btc_watch_config: ViaBtcWatchConfig,
        indexer: BitcoinInscriptionIndexer,
        btc_client: Arc<BitcoinClient>,
        pool: ConnectionPool<Core>,
        is_main_node: bool,
    ) -> anyhow::Result<Self> {
        let system_wallet_processor = Box::new(SystemWalletProcessor::new(btc_client.clone()));

        // Only build message processors that match the actor role:
        let mut message_processors: Vec<Box<dyn MessageProcessor>> = vec![
            Box::new(L1ToL2MessageProcessor::default()),
            Box::new(VotableMessageProcessor::default()),
        ];

        if is_main_node {
            let mut storage = pool.connection_tagged(BtcWatch::module_name()).await?;

            let protocol_semantic_version = storage
                .protocol_versions_dal()
                .latest_semantic_version()
                .await
                .expect("Failed to load the latest protocol semantic version")
                .ok_or_else(|| anyhow::anyhow!("Protocol version is missing"))?;

            message_processors.push(Box::new(GovernanceUpgradesEventProcessor::new(
                btc_client,
                protocol_semantic_version,
            )));
        }

        Ok(Self {
            btc_watch_config,
            indexer,
            pool,
            message_processors,
            system_wallet_processor,
        })
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.btc_watch_config.poll_interval());
        let pool = self.pool.clone();

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            let mut storage = pool.connection_tagged(BtcWatch::module_name()).await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => { /* everything went fine */ }
                Err(MessageProcessorError::Internal(err)) => {
                    tracing::error!("Internal error processing new blocks: {err:?}");
                    return Err(err);
                }
                Err(err) => {
                    tracing::error!("Failed to process new blocks: {err}");
                }
            }
        }

        tracing::info!("Stop signal received, via_btc_watch is shutting down");
        Ok(())
    }

    async fn loop_iteration(
        &mut self,
        storage: &mut Connection<'_, Core>,
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
            .get_last_processed_l1_block(BtcWatch::module_name())
            .await? as u32;

        if last_processed_bitcoin_block == 0 {
            return Err(MessageProcessorError::Internal(anyhow::anyhow!(
                "The indexer is not initialized".to_string()
            )));
        }

        let current_l1_block_number =
            self.indexer
                .fetch_block_height()
                .await
                .map_err(|e| MessageProcessorError::Internal(anyhow::anyhow!(e.to_string())))?
                .saturating_sub(self.btc_watch_config.block_confirmations) as u32;
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

        let mut messages = self
            .indexer
            .process_blocks(from_block, to_block)
            .await
            .map_err(|e| MessageProcessorError::Internal(e.into()))?;

        // Re-process blocks if system wallets were updated, since the new wallet state
        // may change how subsequent messages are interpreted.
        if self
            .system_wallet_processor
            .process_messages(storage, messages.clone(), &mut self.indexer)
            .await
            .map_err(|e| MessageProcessorError::Internal(e.into()))?
        {
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

        storage
            .via_indexer_dal()
            .update_last_processed_l1_block(BtcWatch::module_name(), to_block)
            .await
            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

        tracing::info!(
            "The btc_watch processed {} blocks, from {} to {}",
            L1_BLOCKS_CHUNK,
            from_block,
            to_block,
        );

        Ok(())
    }

    fn module_name() -> &'static str {
        "via_btc_watch"
    }
}
