mod message_processors;
mod dal_adapters;
mod storage;
mod metrics;

use std::sync::Arc;

use message_processors::{GovernanceUpgradesEventProcessor, WithdrawalProcessor};
use tokio::sync::watch;
pub use via_btc_client::types::BitcoinNetwork;
use via_btc_client::{client::BitcoinClient, indexer::BitcoinInscriptionIndexer};
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use via_verifier_types::protocol_version::check_if_supported_sequencer_version;
use zksync_config::{configs::via_btc_watch::L1_BLOCKS_CHUNK, ViaBtcWatchConfig};

use self::message_processors::{MessageProcessor, MessageProcessorError};
use crate::message_processors::{
    L1ToL2MessageProcessor, SystemWalletProcessor, VerifierMessageProcessor,
};
use via_btc_watch_common::orchestrator::WatchOrchestrator;
use std::sync::Arc as StdArc;
use crate::storage::VerifierStorage;

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
        zk_agreement_threshold: f64,
    ) -> anyhow::Result<Self> {
        let system_wallet_processor = Box::new(SystemWalletProcessor::new(btc_client.clone()));

        let message_processors: Vec<Box<dyn MessageProcessor>> = vec![
            Box::new(GovernanceUpgradesEventProcessor::new(btc_client)),
            Box::new(L1ToL2MessageProcessor::new(
                indexer.get_state().bridge.clone(),
            )),
            Box::new(VerifierMessageProcessor::new(zk_agreement_threshold)),
            Box::new(WithdrawalProcessor::new()),
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
        let pool = self.pool.clone();
        let mut storage_adapter = VerifierStorage { pool };
        let sys_proc = StdArc::new(tokio::sync::Mutex::new(self.system_wallet_processor));
        let module = Self::module_name();
        let processors = StdArc::new(tokio::sync::Mutex::new(self.message_processors));
        let cfg = self.config.clone();
        let mut indexer = self.indexer;

        let pre_cb = move |storage: VerifierStorage| -> via_btc_watch_common::orchestrator::PreFut {
            Box::pin(async move {
                let mut conn = storage.pool.connection_tagged("via_btc_watch").await?;
                if let Some(last_protocol_version) = conn
                    .via_protocol_versions_dal()
                    .latest_protocol_semantic_version()
                    .await
                    .expect("Error load the protocol version")
                {
                    check_if_supported_sequencer_version(last_protocol_version)
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                }
                Ok(())
            })
        };

        let orchestrator = WatchOrchestrator::new(
            cfg,
            indexer,
            move || {
                let mut s = storage_adapter.clone();
                Box::pin(async move { Ok::<_, anyhow::Error>(s) })
            },
            {
                let processors = processors.clone();
                move |storage: VerifierStorage, messages, indexer| {
                    let processors = processors.clone();
                    let pool = storage.pool.clone();
                    Box::pin(async move {
                        let mut guard = processors.lock().await;
                        for p in guard.iter_mut() {
                            p.process_messages(
                                &mut pool.connection_tagged("via_btc_watch").await?,
                                messages.clone(),
                                &mut *indexer.lock().await,
                            )
                            .await
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        }
                        Ok(())
                    })
                }
            },
            Some(pre_cb),
            {
                let sys_proc = sys_proc.clone();
                move |storage: VerifierStorage, messages, indexer| {
                    let sys_proc = sys_proc.clone();
                    let pool = storage.pool.clone();
                    Box::pin(async move {
                        let mut conn = pool.connection_tagged("via_btc_watch").await?;
                        let mut guard = sys_proc.lock().await;
                        guard
                            .process_messages(&mut conn, messages, &mut *indexer.lock().await)
                            .await
                            .map_err(|e| anyhow::anyhow!(e.to_string()))
                    })
                }
            },
            module,
            self.config.start_l1_block_number,
            false,
        ).await?;

        orchestrator.run(stop_receiver).await
    }

    async fn loop_iteration(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> Result<(), MessageProcessorError> {
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

        let mut to_block = last_processed_bitcoin_block + L1_BLOCKS_CHUNK;
        if to_block > current_l1_block_number {
            to_block = current_l1_block_number;
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
            .update_last_processed_l1_block(VerifierBtcWatch::module_name(), to_block)
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
