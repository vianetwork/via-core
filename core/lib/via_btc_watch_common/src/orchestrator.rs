use std::{future::Future, marker::PhantomData, pin::Pin, sync::Arc};

use tokio::sync::Mutex;
use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
use zksync_config::{configs::via_btc_watch::L1_BLOCKS_CHUNK, ViaBtcWatchConfig};

use crate::dal::{IndexerMetaDal, WalletsDal};

pub struct WatchOrchestrator<S, GetConn, ProcessCb, PreCb, SysCb> {
    config: ViaBtcWatchConfig,
    indexer: Arc<Mutex<BitcoinInscriptionIndexer>>,
    get_conn: GetConn,
    process_cb: ProcessCb,
    pre_cb: Option<PreCb>,
    system_wallet_cb: SysCb,
    module_name: &'static str,
    last_processed_bitcoin_block: u32,
    _phantom: PhantomData<S>,
}

impl<S, GetConn, ProcessCb, PreCb, SysCb> WatchOrchestrator<S, GetConn, ProcessCb, PreCb, SysCb>
where
    S: IndexerMetaDal + WalletsDal + Send + Clone + 'static,
    GetConn: Fn() -> ConnFut<S> + Send + Sync + 'static,
    ProcessCb: Fn(S, Vec<FullInscriptionMessage>, Arc<Mutex<BitcoinInscriptionIndexer>>) -> ProcFut
        + Send
        + Sync
        + 'static,
    PreCb: Fn(S) -> PreFut + Send + Sync + 'static,
    SysCb: Fn(S, Vec<FullInscriptionMessage>, Arc<Mutex<BitcoinInscriptionIndexer>>) -> SysFut
        + Send
        + Sync
        + 'static,
{
    pub async fn new(
        config: ViaBtcWatchConfig,
        indexer: BitcoinInscriptionIndexer,
        get_conn: GetConn,
        process_cb: ProcessCb,
        pre_cb: Option<PreCb>,
        system_wallet_cb: SysCb,
        module_name: &'static str,
        start_l1_block_number: u32,
        restart_indexing: bool,
    ) -> anyhow::Result<Self> {
        let mut storage = (get_conn)().await?;
        let last_processed_bitcoin_block = Self::initialize_state(
            &mut storage,
            module_name,
            start_l1_block_number,
            restart_indexing,
        )
        .await?;

        Ok(Self {
            config,
            indexer: Arc::new(Mutex::new(indexer)),
            get_conn,
            process_cb,
            pre_cb,
            system_wallet_cb,
            module_name,
            last_processed_bitcoin_block,
            _phantom: PhantomData,
        })
    }

    async fn initialize_state(
        storage: &mut S,
        module_name: &str,
        start_l1_block_number: u32,
        restart_indexing: bool,
    ) -> anyhow::Result<u32> {
        let mut last_processed_bitcoin_block =
            storage.get_last_processed_l1_block(module_name).await?;

        if last_processed_bitcoin_block == 0 || restart_indexing {
            storage
                .init_indexer_metadata(module_name, start_l1_block_number)
                .await?;
            last_processed_bitcoin_block = start_l1_block_number - 1;
        }

        Ok(last_processed_bitcoin_block)
    }

    pub async fn run(
        mut self,
        mut stop_receiver: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.config.poll_interval());

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => {}
                _ = stop_receiver.changed() => break,
            }

            let mut storage = (self.get_conn)().await?;

            if let Some(pre_cb) = &self.pre_cb {
                pre_cb(storage.clone()).await?;
            }

            match self.loop_iteration(&mut storage).await {
                Ok(()) => {}
                Err(err) => {
                    tracing::error!("Failed to process new blocks: {err:?}");
                    self.last_processed_bitcoin_block = Self::initialize_state(
                        &mut storage,
                        self.module_name,
                        self.config.start_l1_block_number,
                        self.config.restart_indexing,
                    )
                    .await?;
                }
            }
        }

        tracing::info!("Stop signal received, via_btc_watch orchestrator is shutting down");
        Ok(())
    }

    async fn loop_iteration(&mut self, storage: &mut S) -> anyhow::Result<()> {
        let current_l1_block_number = {
            let idx = self.indexer.lock().await;
            idx.fetch_block_height()
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
                .saturating_sub(self.config.block_confirmations) as u32
        };

        if current_l1_block_number <= self.last_processed_bitcoin_block {
            return Ok(());
        }

        let mut to_block = self.last_processed_bitcoin_block + L1_BLOCKS_CHUNK;
        if to_block > current_l1_block_number {
            to_block = current_l1_block_number;
        }

        let mut messages = {
            let mut idx = self.indexer.lock().await;
            idx.process_blocks(self.last_processed_bitcoin_block + 1, to_block)
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
        };

        if (self.system_wallet_cb)(storage.clone(), messages.clone(), self.indexer.clone()).await? {
            messages = {
                let mut idx = self.indexer.lock().await;
                idx.process_blocks(self.last_processed_bitcoin_block + 1, to_block)
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?
            };
        }

        (self.process_cb)(storage.clone(), messages, self.indexer.clone()).await?;

        storage
            .update_last_processed_l1_block(self.module_name, to_block)
            .await?;

        self.last_processed_bitcoin_block = to_block;
        Ok(())
    }
}

pub type ConnFut<S> = Pin<Box<dyn Future<Output = anyhow::Result<S>> + Send>>;
pub type ProcFut = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
pub type PreFut = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
pub type SysFut = Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send>>;
