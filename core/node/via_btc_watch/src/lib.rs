mod message_processors;
mod metrics;

use anyhow::Context;
use message_processors::GovernanceUpgradesEventProcessor;
use tokio::sync::watch;
// re-export via_btc_client types
pub use via_btc_client::types::BitcoinNetwork;
use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::BitcoinAddress};
use zksync_config::ViaBtcWatchConfig;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_types::PriorityOpId;

use self::{
    message_processors::{
        L1ToL2MessageProcessor, MessageProcessor, MessageProcessorError, VotableMessageProcessor,
    },
    metrics::{ErrorType, METRICS},
};

#[derive(Debug)]
struct BtcWatchState {
    last_processed_bitcoin_block: u32,
    next_expected_priority_id: PriorityOpId,
}

#[derive(Debug)]
pub struct BtcWatch {
    btc_watch_config: ViaBtcWatchConfig,
    indexer: BitcoinInscriptionIndexer,
    pool: ConnectionPool<Core>,
    state: BtcWatchState,
    message_processors: Vec<Box<dyn MessageProcessor>>,
}

impl BtcWatch {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        btc_watch_config: ViaBtcWatchConfig,
        indexer: BitcoinInscriptionIndexer,
        pool: ConnectionPool<Core>,
        bridge_address: BitcoinAddress,
        zk_agreement_threshold: f64,
    ) -> anyhow::Result<Self> {
        let mut storage = pool.connection_tagged("via_btc_watch").await?;
        let state =
            Self::initialize_state(&indexer, &mut storage, btc_watch_config.btc_blocks_lag).await?;
        tracing::info!("initialized state: {state:?}");

        let protocol_semantic_version = storage
            .protocol_versions_dal()
            .latest_semantic_version()
            .await
            .expect("Failed to load the latest protocol semantic version")
            .ok_or_else(|| anyhow::anyhow!("Protocol version is missing"))?;

        drop(storage);

        // Only build message processors that match the actor role:
        let message_processors: Vec<Box<dyn MessageProcessor>> = vec![
            Box::new(GovernanceUpgradesEventProcessor::new(
                protocol_semantic_version,
            )),
            Box::new(L1ToL2MessageProcessor::new(
                bridge_address,
                state.next_expected_priority_id,
            )),
            Box::new(VotableMessageProcessor::new(zk_agreement_threshold)),
        ];

        Ok(Self {
            btc_watch_config,
            indexer,
            pool,
            state,
            message_processors,
        })
    }

    async fn initialize_state(
        indexer: &BitcoinInscriptionIndexer,
        storage: &mut Connection<'_, Core>,
        btc_blocks_lag: u32,
    ) -> anyhow::Result<BtcWatchState> {
        let last_processed_bitcoin_block = match storage
            .via_transactions_dal()
            .get_last_processed_l1_block()
            .await?
        {
            Some(block) => block.0.saturating_sub(1),
            None => {
                let current_block = indexer
                    .fetch_block_height()
                    .await
                    .with_context(|| "cannot get current Bitcoin block")?
                    as u32;

                current_block.saturating_sub(btc_blocks_lag)
            }
        };

        let next_expected_priority_id = storage
            .via_transactions_dal()
            .last_priority_id()
            .await?
            .map(|id| id + 1)
            .unwrap_or(PriorityOpId(0));

        Ok(BtcWatchState {
            last_processed_bitcoin_block,
            next_expected_priority_id,
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
                    self.state.last_processed_bitcoin_block = Self::initialize_state(
                        &self.indexer,
                        &mut storage,
                        self.btc_watch_config.btc_blocks_lag,
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
        storage: &mut Connection<'_, Core>,
    ) -> Result<(), MessageProcessorError> {
        let to_block =
            self.indexer
                .fetch_block_height()
                .await
                .map_err(|e| MessageProcessorError::Internal(anyhow::anyhow!(e.to_string())))?
                .saturating_sub(self.btc_watch_config.block_confirmations) as u32;
        if to_block <= self.state.last_processed_bitcoin_block {
            return Ok(());
        }

        let messages = self
            .indexer
            .process_blocks(self.state.last_processed_bitcoin_block + 1, to_block)
            .await
            .map_err(|e| MessageProcessorError::Internal(e.into()))?;

        for processor in self.message_processors.iter_mut() {
            processor
                .process_messages(storage, messages.clone(), &mut self.indexer)
                .await
                .map_err(|e| MessageProcessorError::Internal(e.into()))?;
        }

        self.state.last_processed_bitcoin_block = to_block;
        Ok(())
    }
}
