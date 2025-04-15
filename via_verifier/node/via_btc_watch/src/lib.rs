mod message_processors;
mod metrics;

use std::time::Duration;

use tokio::sync::watch;
// re-export via_btc_client types
pub use via_btc_client::types::BitcoinNetwork;
use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinTxid, NodeAuth},
};
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use zksync_config::{configs::via_btc_watch::L1_BLOCKS_CHUNK, ActorRole};

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
    indexer: BitcoinInscriptionIndexer,
    poll_interval: Duration,
    confirmations_for_btc_msg: u64,
    last_processed_bitcoin_block: u32,
    pool: ConnectionPool<Verifier>,
    message_processors: Vec<Box<dyn MessageProcessor>>,
    start_l1_block_number: u32,
}

impl VerifierBtcWatch {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        rpc_url: &str,
        network: BitcoinNetwork,
        node_auth: NodeAuth,
        confirmations_for_btc_msg: Option<u64>,
        bootstrap_txids: Vec<BitcoinTxid>,
        pool: ConnectionPool<Verifier>,
        poll_interval: Duration,
        start_l1_block_number: u32,
        actor_role: &ActorRole,
        zk_agreement_threshold: f64,
        restart_indexing: bool,
    ) -> anyhow::Result<Self> {
        let indexer =
            BitcoinInscriptionIndexer::new(rpc_url, network, node_auth, bootstrap_txids).await?;
        let mut storage = pool
            .connection_tagged(VerifierBtcWatch::module_name())
            .await?;
        let state =
            Self::initialize_state(&mut storage, start_l1_block_number, restart_indexing).await?;
        tracing::info!("initialized state: {state:?}");
        drop(storage);

        assert_eq!(actor_role, &ActorRole::Verifier);

        let message_processors: Vec<Box<dyn MessageProcessor>> = vec![
            Box::new(L1ToL2MessageProcessor::new(indexer.get_state().0)),
            Box::new(VerifierMessageProcessor::new(zk_agreement_threshold)),
        ];

        let confirmations_for_btc_msg = confirmations_for_btc_msg.unwrap_or(0);

        // We should not set confirmations_for_btc_msg to 0 for mainnet,
        // because we need to wait for some confirmations to be sure that the transaction is included in a block.
        if network == BitcoinNetwork::Bitcoin && confirmations_for_btc_msg == 0 {
            return Err(anyhow::anyhow!(
                "confirmations_for_btc_msg cannot be 0 for mainnet"
            ));
        }

        Ok(Self {
            indexer,
            poll_interval,
            confirmations_for_btc_msg,
            last_processed_bitcoin_block: state.last_processed_bitcoin_block,
            pool,
            message_processors,
            start_l1_block_number,
        })
    }

    async fn initialize_state(
        storage: &mut Connection<'_, Verifier>,
        start_l1_block_number: u32,
        restart_indexing: bool,
    ) -> anyhow::Result<BtcWatchState> {
        let mut last_processed_bitcoin_block = storage
            .via_indexer_dal()
            .get_last_processed_l1_block(VerifierBtcWatch::module_name())
            .await? as u32;

        if last_processed_bitcoin_block == 0 || restart_indexing {
            storage
                .via_indexer_dal()
                .init_indexer_metadata(VerifierBtcWatch::module_name(), start_l1_block_number)
                .await?;
            last_processed_bitcoin_block = start_l1_block_number - 1;
        }

        Ok(BtcWatchState {
            last_processed_bitcoin_block,
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
            METRICS.btc_poll.inc();

            let mut storage = pool
                .connection_tagged(VerifierBtcWatch::module_name())
                .await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => { /* everything went fine */ }
                Err(MessageProcessorError::Internal(err)) => {
                    METRICS.errors[&ErrorType::InternalError].inc();
                    tracing::error!("Internal error processing new blocks: {err:?}");
                    return Err(err);
                }
                Err(err) => {
                    tracing::error!("Failed to process new blocks: {err}");
                    self.last_processed_bitcoin_block =
                        Self::initialize_state(&mut storage, self.start_l1_block_number, false)
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
        let current_l1_block_number =
            self.indexer
                .fetch_block_height()
                .await
                .map_err(|e| MessageProcessorError::Internal(anyhow::anyhow!(e.to_string())))?
                .saturating_sub(self.confirmations_for_btc_msg) as u32;
        if current_l1_block_number <= self.last_processed_bitcoin_block {
            return Ok(());
        }

        let mut to_block = self.last_processed_bitcoin_block + L1_BLOCKS_CHUNK;
        if to_block > current_l1_block_number {
            to_block = current_l1_block_number;
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

        storage
            .via_indexer_dal()
            .update_last_processed_l1_block(VerifierBtcWatch::module_name(), to_block)
            .await
            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

        self.last_processed_bitcoin_block = to_block;
        Ok(())
    }

    fn module_name() -> &'static str {
        "via_btc_watch"
    }
}
