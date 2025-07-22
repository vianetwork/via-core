mod message_processors;
mod metrics;

use std::sync::Arc;

use message_processors::GovernanceUpgradesEventProcessor;
use tokio::sync::watch;
// re-export via_btc_client types
pub use via_btc_client::types::BitcoinNetwork;
use via_btc_client::{
    client::BitcoinClient, indexer::BitcoinInscriptionIndexer, types::BitcoinAddress,
};
use zksync_config::{configs::via_btc_watch::L1_BLOCKS_CHUNK, ViaBtcWatchConfig};
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};

use self::message_processors::{
    L1ToL2MessageProcessor, MessageProcessor, MessageProcessorError, VotableMessageProcessor,
};

#[derive(Debug)]
struct BtcWatchState {
    last_processed_bitcoin_block: u32,
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
        btc_client: Arc<BitcoinClient>,
        pool: ConnectionPool<Core>,
        bridge_address: BitcoinAddress,
        zk_agreement_threshold: f64,
    ) -> anyhow::Result<Self> {
        let mut storage = pool.connection_tagged(BtcWatch::module_name()).await?;
        let state = Self::initialize_state(
            &mut storage,
            btc_watch_config.start_l1_block_number,
            btc_watch_config.restart_indexing,
        )
        .await?;
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
                btc_client,
                protocol_semantic_version,
            )),
            Box::new(L1ToL2MessageProcessor::new(bridge_address)),
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
        storage: &mut Connection<'_, Core>,
        start_l1_block_number: u32,
        restart_indexing: bool,
    ) -> anyhow::Result<BtcWatchState> {
        let mut last_processed_bitcoin_block = storage
            .via_indexer_dal()
            .get_last_processed_l1_block(BtcWatch::module_name())
            .await? as u32;

        if last_processed_bitcoin_block == 0 || restart_indexing {
            storage
                .via_indexer_dal()
                .init_indexer_metadata(BtcWatch::module_name(), start_l1_block_number)
                .await?;
            last_processed_bitcoin_block = start_l1_block_number - 1;
        }

        Ok(BtcWatchState {
            last_processed_bitcoin_block,
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
                    self.state.last_processed_bitcoin_block = Self::initialize_state(
                        &mut storage,
                        self.btc_watch_config.start_l1_block_number,
                        self.btc_watch_config.restart_indexing,
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
        let current_l1_block_number =
            self.indexer
                .fetch_block_height()
                .await
                .map_err(|e| MessageProcessorError::Internal(anyhow::anyhow!(e.to_string())))?
                .saturating_sub(self.btc_watch_config.block_confirmations) as u32;
        if current_l1_block_number <= self.state.last_processed_bitcoin_block {
            return Ok(());
        }

        let mut to_block = self.state.last_processed_bitcoin_block + L1_BLOCKS_CHUNK;
        if to_block > current_l1_block_number {
            to_block = current_l1_block_number;
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

        storage
            .via_indexer_dal()
            .update_last_processed_l1_block(BtcWatch::module_name(), to_block)
            .await
            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

        self.state.last_processed_bitcoin_block = to_block;
        Ok(())
    }

    fn module_name() -> &'static str {
        "via_btc_watch"
    }
}
