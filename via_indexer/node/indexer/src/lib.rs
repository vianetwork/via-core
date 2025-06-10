mod message_processors;
mod metrics;

use std::sync::Arc;

use message_processors::WithdrawalProcessor;
use metrics::METRICS;
use tokio::sync::watch;
// re-export via_btc_client types
pub use via_btc_client::types::BitcoinNetwork;
use via_btc_client::{client::BitcoinClient, indexer::BitcoinInscriptionIndexer};
use via_indexer_dal::{Connection, ConnectionPool, Indexer, IndexerDal};
use zksync_config::{configs::via_consensus::ViaGenesisConfig, ViaBtcWatchConfig};

use self::message_processors::MessageProcessor;
use crate::message_processors::L1ToL2MessageProcessor;

/// Total L1 blocks to process at a time.
pub const L1_BLOCKS_CHUNK: u32 = 10;

#[derive(Debug)]
pub struct L1Indexer {
    config: ViaBtcWatchConfig,
    indexer: BitcoinInscriptionIndexer,
    pool: ConnectionPool<Indexer>,
    message_processors: Vec<Box<dyn MessageProcessor>>,
}

impl L1Indexer {
    pub async fn new(
        config: ViaBtcWatchConfig,
        via_genesis_config: ViaGenesisConfig,
        indexer: BitcoinInscriptionIndexer,
        client: Arc<BitcoinClient>,
        pool: ConnectionPool<Indexer>,
    ) -> anyhow::Result<Self> {
        let mut storage = pool.connection_tagged(L1Indexer::module_name()).await?;
        L1Indexer::initialize_indexer(
            &mut storage,
            config.start_l1_block_number,
            config.restart_indexing,
        )
        .await?;

        drop(storage);

        let message_processors: Vec<Box<dyn MessageProcessor>> = vec![
            Box::new(L1ToL2MessageProcessor::new()),
            Box::new(WithdrawalProcessor::new(
                via_genesis_config.bridge_address()?,
                client,
            )),
        ];

        Ok(Self {
            config,
            indexer,
            pool,
            message_processors,
        })
    }

    async fn initialize_indexer(
        storage: &mut Connection<'_, Indexer>,
        start_l1_block_number: u32,
        restart_indexing: bool,
    ) -> anyhow::Result<()> {
        let last_processed_bitcoin_block = storage
            .via_indexer_dal()
            .get_last_processed_l1_block(L1Indexer::module_name())
            .await? as u32;

        if restart_indexing || last_processed_bitcoin_block == 0 {
            let mut transaction = storage.start_transaction().await?;

            transaction
                .via_transactions_dal()
                .delete_transactions(start_l1_block_number as i64)
                .await?;
            transaction.via_indexer_dal().delete_metadata().await?;
            transaction
                .via_indexer_dal()
                .init_indexer_metadata(L1Indexer::module_name(), start_l1_block_number - 1)
                .await?;

            transaction.commit().await?;
        }

        Ok(())
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.config.poll_interval());
        let pool = self.pool.clone();

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            let mut storage = pool.connection_tagged(L1Indexer::module_name()).await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => { /* everything went fine */ }
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
        storage: &mut Connection<'_, Indexer>,
    ) -> anyhow::Result<()> {
        let last_processed_bitcoin_block = storage
            .via_indexer_dal()
            .get_last_processed_l1_block(L1Indexer::module_name())
            .await? as u32;

        let current_l1_block_number = self.indexer.fetch_block_height().await? as u32;
        if current_l1_block_number <= last_processed_bitcoin_block {
            return Ok(());
        }

        let mut to_block = last_processed_bitcoin_block + L1_BLOCKS_CHUNK;
        if to_block > current_l1_block_number {
            to_block = current_l1_block_number;
        }

        let messages = self
            .indexer
            .process_blocks(last_processed_bitcoin_block + 1, to_block)
            .await?;

        for processor in self.message_processors.iter_mut() {
            processor
                .process_messages(storage, messages.clone(), &mut self.indexer)
                .await?;
        }

        storage
            .via_indexer_dal()
            .update_last_processed_l1_block(L1Indexer::module_name(), to_block)
            .await?;

        METRICS
            .current_block_number
            .set(current_l1_block_number as usize);
        METRICS.last_indexed_block_number.set(to_block as usize);

        tracing::info!(
            "Blocks from {} to {} processed",
            last_processed_bitcoin_block,
            to_block
        );

        Ok(())
    }

    fn module_name() -> &'static str {
        "l1_indexer"
    }
}
