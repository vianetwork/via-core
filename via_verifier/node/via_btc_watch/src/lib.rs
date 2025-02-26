mod message_processors;
mod metrics;

use std::time::Duration;

use anyhow::Context;
use tokio::sync::watch;
// re-export via_btc_client types
pub use via_btc_client::types::BitcoinNetwork;
use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinTxid, NodeAuth},
};
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use zksync_config::ActorRole;

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
    btc_blocks_lag: u32,
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
        btc_blocks_lag: u32,
        actor_role: &ActorRole,
        zk_agreement_threshold: f64,
    ) -> anyhow::Result<Self> {
        let indexer =
            BitcoinInscriptionIndexer::new(rpc_url, network, node_auth, bootstrap_txids).await?;
        let mut storage = pool.connection_tagged("via_btc_watch").await?;
        let state = Self::initialize_state(&indexer, &mut storage, btc_blocks_lag).await?;
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
            btc_blocks_lag,
        })
    }

    async fn initialize_state(
        indexer: &BitcoinInscriptionIndexer,
        storage: &mut Connection<'_, Verifier>,
        btc_blocks_lag: u32,
    ) -> anyhow::Result<BtcWatchState> {
        let last_processed_bitcoin_block = match storage
            .via_votes_dal()
            .get_last_finilized_l1_batch()
            .await?
        {
            Some(l1_batch_number) => l1_batch_number.saturating_sub(1),
            None => indexer
                .fetch_block_height()
                .await
                .with_context(|| "Error to get current Bitcoin block")?
                .saturating_sub(btc_blocks_lag as u128) as u32, // TODO: remove cast
        };

        // TODO: get the bridge address from the database?
        let (_bridge_address, ..) = indexer.get_state();

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
                    self.last_processed_bitcoin_block =
                        Self::initialize_state(&self.indexer, &mut storage, self.btc_blocks_lag)
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
        let to_block = self
            .indexer
            .fetch_block_height()
            .await
            .map_err(|e| MessageProcessorError::Internal(anyhow::anyhow!(e.to_string())))?
            .saturating_sub(self.confirmations_for_btc_msg as u128) as u32;
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
