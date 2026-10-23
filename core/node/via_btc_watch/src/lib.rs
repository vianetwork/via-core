mod message_processors;
mod metrics;

use std::sync::Arc;

use message_processors::GovernanceUpgradesEventProcessor;
use tokio::sync::watch;
// re-export via_btc_client types
pub use via_btc_client::types::BitcoinNetwork;
use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, WithdrawalScanMode},
};
use zksync_config::{configs::via_btc_watch::L1_BLOCKS_CHUNK, ViaBtcWatchConfig};
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_types::via_wallet::SystemWallets;

#[cfg(test)]
mod test;

use self::message_processors::{
    L1ToL2MessageProcessor, MessageProcessor, MessageProcessorError, VotableMessageProcessor,
};
use crate::{message_processors::SystemWalletProcessor, metrics::METRICS};

#[derive(Debug)]
pub struct BtcWatch {
    btc_watch_config: ViaBtcWatchConfig,
    indexer: BitcoinInscriptionIndexer,
    btc_client: Arc<BitcoinClient>,
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
        let mut message_processors: Vec<Box<dyn MessageProcessor>> =
            vec![Box::new(L1ToL2MessageProcessor::default()), Box::new(VotableMessageProcessor::default())];

        if is_main_node {
            let mut storage = pool.connection_tagged(BtcWatch::module_name()).await?;

            storage
                .protocol_versions_dal()
                .latest_via_semantic_version()
                .await
                .expect("Failed to load the latest protocol semantic version")
                .ok_or_else(|| anyhow::anyhow!("Protocol version is missing"))?;

            message_processors.push(Box::new(GovernanceUpgradesEventProcessor::new(btc_client.clone())));
        }

        Ok(Self {
            btc_watch_config,
            btc_client,
            indexer: indexer.with_withdrawal_mode(WithdrawalScanMode::Ignore),
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
                Err(err) => {
                    METRICS.errors.inc();
                    tracing::error!("Failed to process new blocks: {err}");
                }
            }
        }

        tracing::info!("Stop signal received, via_btc_watch is shutting down");
        Ok(())
    }

    async fn loop_iteration(&mut self, storage: &mut Connection<'_, Core>) -> Result<(), MessageProcessorError> {
        let mut transaction = storage.start_transaction().await?;
        transaction.via_indexer_dal().lock_btc_watch().await?;
        self.process_scan(&mut transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    // jscpd:ignore-start Concrete node-role transactions retain their separate processors, source locks and queue effects.
    async fn process_scan(
        &mut self,
        storage: &mut Connection<'_, Core>,
    ) -> Result<(), MessageProcessorError> {
        if storage.via_l1_block_dal().has_reorg_in_progress().await?.is_some() {
            return Ok(());
        }

        let last_processed_bitcoin_block =
            storage.via_indexer_dal().get_last_processed_l1_block(BtcWatch::module_name()).await? as u32;

        if last_processed_bitcoin_block == 0 {
            return Err(MessageProcessorError::Internal(anyhow::anyhow!("The indexer is not initialized".to_string())));
        }

        let current_l1_block_number = self
            .indexer
            .fetch_block_height()
            .await
            .map_err(|e| MessageProcessorError::Internal(anyhow::anyhow!(e.to_string())))?
            .saturating_sub(self.btc_watch_config.block_confirmations) as u32;
        if current_l1_block_number <= last_processed_bitcoin_block {
            return Ok(());
        }

        let Some((last_l1_block_number, _)) = storage.via_l1_block_dal().get_last_l1_block().await? else {
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

        if to_block < from_block {
            return Ok(());
        }

        let system_wallets_map =
            match storage.via_wallet_dal().get_system_wallets_raw(last_processed_bitcoin_block as i64).await? {
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

        for height in from_block..=to_block {
            let wallets = storage
                .via_wallet_dal()
                .get_system_wallets_raw(i64::from(height) - 1)
                .await?
                .ok_or_else(|| {
                    anyhow::anyhow!("configuration history missing before Bitcoin block {height}")
                })?;
            let wallets = SystemWallets::try_from(wallets)?;
            self.indexer.update_system_wallets(
                Some(wallets.sequencer),
                Some(wallets.bridge),
                Some(wallets.verifiers),
                Some(wallets.governance),
            );
            let hash = storage
                .via_l1_block_dal()
                .get_l1_block_hash(i64::from(height))
                .await?
                .ok_or_else(|| anyhow::anyhow!("canonical block is missing at {height}"))?;
            let hash = hash.parse().map_err(anyhow::Error::from)?;
            let header = self
                .btc_client
                .canonical_header_at(height, hash)
                .await
                .map_err(anyhow::Error::from)?;
            let body = self.btc_client.checked_block(&header).await.map_err(anyhow::Error::from)?;
            let messages = self
                .indexer
                .process_checked_block(height, &body)
                .await
                .map_err(anyhow::Error::from)?;
            // Record the whole block before rotation admission; its query uses an exclusive source-coordinate prefix.
            for processor in &mut self.message_processors {
                processor.process_messages(storage, messages.clone(), &mut self.indexer).await?;
            }
            self.system_wallet_processor
                .process_messages(storage, messages, &mut self.indexer)
                .await?;
            storage
                .via_votes_dal()
                .recompute_reader_finalization(
                    height,
                    via_consensus::consensus::BATCH_FINALIZATION_THRESHOLD,
                )
                .await?;
        }

        storage
            .via_indexer_dal()
            .update_last_processed_l1_block(BtcWatch::module_name(), to_block)
            .await
            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

        tracing::info!("The btc_watch processed blocks, from {} to {}", from_block, to_block,);

        Ok(())
    }
    // jscpd:ignore-end

    fn module_name() -> &'static str {
        "via_btc_watch"
    }
}
