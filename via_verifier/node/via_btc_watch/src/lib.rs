mod message_processors;
mod metrics;

use anyhow::Context;
use message_processors::GovernanceUpgradesEventProcessor;
use tokio::sync::watch;
// re-export via_btc_client types
use via_btc_client::indexer::BitcoinInscriptionIndexer;
pub use via_btc_client::types::BitcoinNetwork;
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use via_verifier_types::protocol_version::check_if_supported_sequencer_version;
use zksync_config::ViaBtcWatchConfig;

use self::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::METRICS,
};
use crate::{
    message_processors::{L1ToL2MessageProcessor, VerifierMessageProcessor},
    metrics::ErrorType,
};

#[derive(Debug)]
struct BtcWatchState {
    last_processed_bitcoin_block: u32,
}

#[derive(Debug)]
pub struct VerifierBtcWatch {
    config: ViaBtcWatchConfig,
    indexer: BitcoinInscriptionIndexer,
    last_processed_bitcoin_block: u32,
    pool: ConnectionPool<Verifier>,
    message_processors: Vec<Box<dyn MessageProcessor>>,
}

impl VerifierBtcWatch {
    pub async fn new(
        config: ViaBtcWatchConfig,
        indexer: BitcoinInscriptionIndexer,
        pool: ConnectionPool<Verifier>,
        zk_agreement_threshold: f64,
    ) -> anyhow::Result<Self> {
        let mut storage = pool.connection_tagged("via_btc_watch").await?;
        let state = Self::initialize_state(&indexer, &mut storage, config.btc_blocks_lag).await?;
        tracing::info!("initialized state: {state:?}");

        drop(storage);

        let message_processors: Vec<Box<dyn MessageProcessor>> = vec![
            Box::new(GovernanceUpgradesEventProcessor::new()),
            Box::new(L1ToL2MessageProcessor::new(indexer.get_state().0)),
            Box::new(VerifierMessageProcessor::new(zk_agreement_threshold)),
        ];

        Ok(Self {
            config,
            indexer,
            last_processed_bitcoin_block: state.last_processed_bitcoin_block,
            pool,
            message_processors,
        })
    }

    async fn initialize_state(
        indexer: &BitcoinInscriptionIndexer,
        storage: &mut Connection<'_, Verifier>,
        btc_blocks_lag: u32,
    ) -> anyhow::Result<BtcWatchState> {
        let last_processed_bitcoin_block = match storage
            .via_votes_dal()
            .get_last_finalized_l1_batch()
            .await?
        {
            Some(block) => block.saturating_sub(1),
            None => {
                let current_block = indexer
                    .fetch_block_height()
                    .await
                    .with_context(|| "cannot get current Bitcoin block")?
                    as u32;

                current_block.saturating_sub(btc_blocks_lag)
            }
        };

        Ok(BtcWatchState {
            last_processed_bitcoin_block,
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
            METRICS.btc_poll.inc();

            let mut storage = pool.connection_tagged("via_btc_watch").await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => { /* everything went fine */ }
                Err(MessageProcessorError::Internal(err)) => {
                    METRICS.errors[&ErrorType::InternalError].inc();
                    tracing::error!("Internal error processing new blocks: {err:?}");
                    return Err(err);
                }
                Err(err) => {
                    tracing::error!("Failed to process new blocks: {err}");
                    self.last_processed_bitcoin_block = Self::initialize_state(
                        &self.indexer,
                        &mut storage,
                        self.config.btc_blocks_lag,
                    )
                    .await?
                    .last_processed_bitcoin_block;
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
        if let Some(last_protocol_version) = storage
            .via_protocol_versions_dal()
            .latest_protocol_semantic_version()
            .await
            .expect("Error load the protocol version")
        {
            check_if_supported_sequencer_version(last_protocol_version)
                .map_err(|e| MessageProcessorError::Internal(anyhow::anyhow!(e.to_string())))?;
        }

        let to_block = self
            .indexer
            .fetch_block_height()
            .await
            .map_err(|e| MessageProcessorError::Internal(anyhow::anyhow!(e.to_string())))?
            .saturating_sub(self.config.block_confirmations) as u32;
        if to_block <= self.last_processed_bitcoin_block {
            return Ok(());
        }

        let messages = self
            .indexer
            .process_blocks(self.last_processed_bitcoin_block + 1, to_block)
            .await
            .map_err(|e| MessageProcessorError::Internal(e.into()))?;

        for processor in self.message_processors.iter_mut() {
            processor
                .process_messages(storage, messages.clone(), &mut self.indexer)
                .await
                .map_err(|e| MessageProcessorError::Internal(e.into()))?;
        }

        self.last_processed_bitcoin_block = to_block;
        Ok(())
    }
}
