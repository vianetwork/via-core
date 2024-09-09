mod message_processors;

use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::watch;
use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinAddress, BitcoinNetwork, BitcoinTxid},
};
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_types::PriorityOpId;

use self::message_processors::{L1ToL2MessageProcessor, MessageProcessor, MessageProcessorError};

// Number of blocks that we should wait before processing the new blocks.
pub(crate) const BTC_BLOCKS_LAG: u32 = 1000;

#[derive(Debug)]
struct BtcWatchState {
    last_processed_bitcoin_block: u32,
    next_expected_priority_id: PriorityOpId,
    bridge_address: BitcoinAddress,
}

#[derive(Debug)]
pub struct BtcWatch {
    indexer: BitcoinInscriptionIndexer,
    poll_interval: Duration,
    last_processed_bitcoin_block: u32,
    pool: ConnectionPool<Core>,
    message_processors: Vec<Box<dyn MessageProcessor>>,
}

impl BtcWatch {
    pub async fn new(
        rpc_url: &str,
        network: BitcoinNetwork,
        bootstrap_txids: Vec<BitcoinTxid>,
        pool: ConnectionPool<Core>,
        poll_interval: Duration,
    ) -> anyhow::Result<Self> {
        let indexer = BitcoinInscriptionIndexer::new(rpc_url, network, bootstrap_txids).await?;
        let mut storage = pool.connection_tagged("via_btc_watch").await?;
        let state = Self::initialize_state(&indexer, &mut storage).await?;
        tracing::info!("initialized state: {state:?}");
        drop(storage);

        // TODO: add other message processors if needed
        let message_processors: Vec<Box<dyn MessageProcessor>> =
            vec![Box::new(L1ToL2MessageProcessor::new(
                state.bridge_address.clone(),
                state.next_expected_priority_id,
            ))];

        Ok(Self {
            indexer,
            poll_interval,
            last_processed_bitcoin_block: state.last_processed_bitcoin_block,
            pool,
            message_processors,
        })
    }

    async fn initialize_state(
        indexer: &BitcoinInscriptionIndexer,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<BtcWatchState> {
        let last_processed_bitcoin_block = match storage
            .via_transactions_dal()
            .get_last_processed_l1_block()
            .await?
        {
            Some(block) => block.0.saturating_sub(1),
            None => indexer
                .fetch_block_height()
                .await
                .context("cannot get current Bitcoin block")?
                .saturating_sub(BTC_BLOCKS_LAG as u128) as u32, // TODO: remove cast
        };

        // TODO: get the bridge address from the database?
        let (bridge_address, ..) = indexer.get_state();

        let next_expected_priority_id = storage
            .via_transactions_dal()
            .last_priority_id()
            .await?
            .map(|id| id + 1)
            .unwrap_or(PriorityOpId(0));

        Ok(BtcWatchState {
            last_processed_bitcoin_block,
            bridge_address,
            next_expected_priority_id,
        })
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.poll_interval);
        let pool = self.pool.clone();

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            let mut storage = pool.connection_tagged("via_btc_watch").await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => { /* everything went fine */ }
                Err(MessageProcessorError::Internal(err)) => {
                    tracing::error!("Internal error processing new blocks: {err:?}");
                    return Err(err);
                }
                Err(err) => {
                    tracing::error!("Failed to process new blocks: {err}");
                    self.last_processed_bitcoin_block =
                        Self::initialize_state(&self.indexer, &mut storage)
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
        let to_block = self
            .indexer
            .fetch_block_height()
            .await
            .map_err(|e| MessageProcessorError::Internal(anyhow::anyhow!(e.to_string())))?
            as u32;
        if to_block <= self.last_processed_bitcoin_block {
            return Ok(());
        }

        let messages = self
            .indexer
            .process_blocks(self.last_processed_bitcoin_block + 1, to_block)
            .await
            .map_err(|e| MessageProcessorError::Internal(e.into()))?;

        // temporary use only one processor to avoid cloning
        if let Some(processor) = self.message_processors.first_mut() {
            processor.process_messages(storage, messages).await?;
        }

        self.last_processed_bitcoin_block = to_block;
        Ok(())
    }
}
