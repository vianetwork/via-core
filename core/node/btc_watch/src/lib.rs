use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::watch;
use via_btc_client::indexer::BitcoinInscriptionIndexer;
use via_btc_client::types::{BitcoinNetwork, BitcoinTxid};
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use crate::event_processors::EventProcessorError;

mod event_processors;

#[derive(Debug)]
struct BtcWatchState {
    last_processed_bitcoin_block: u32,
}

#[derive(Debug)]
pub struct BtcWatch {
    indexer: BitcoinInscriptionIndexer,
    poll_interval: Duration,
    last_processed_bitcoin_block: u32,
    pool: ConnectionPool<Core>,
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
        let mut storage = pool.connection_tagged("btc_watch").await?;
        let state = Self::initialize_state(&indexer, &mut storage).await?;
        tracing::info!("initialized state: {state:?}");
        drop(storage);

        // TODO: init event processors

        Ok(Self {
            indexer,
            poll_interval,
            last_processed_bitcoin_block: state.last_processed_bitcoin_block,
            pool,
        })
    }

    async fn initialize_state(
        indexer: &BitcoinInscriptionIndexer,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<BtcWatchState> {
        // TODO: change it to actual value
        let last_processed_bitcoin_block = match storage
            .transactions_dal()
            .get_last_processed_l1_block()
            .await?
        {
            Some(block) => block.0.saturating_sub(1),
            None => indexer
                .fetch_block_height()
                .await
                .context("cannot get current Bitcoin block")? as u32
        };

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

            let mut storage = pool.connection_tagged("btc_watch").await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => { /* everything went fine */ }
                Err(EventProcessorError::Internal(err)) => {
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

        tracing::info!("Stop signal received, btc_watch is shutting down");
        Ok(())
    }

    async fn loop_iteration(
        &mut self,
        _storage: &mut Connection<'_, Core>,
    ) -> Result<(), EventProcessorError> {
        let to_block = self.indexer.fetch_block_height().await.unwrap() as u32;
        if to_block <= self.last_processed_bitcoin_block {
            return Ok(());
        }

        let _messages = self.indexer.process_blocks(
            self.last_processed_bitcoin_block + 1,
            to_block
        ).await.unwrap();

        // TODO: process messages

        self.last_processed_bitcoin_block = to_block;
        Ok(())
    }
}
