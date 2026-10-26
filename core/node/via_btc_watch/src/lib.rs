mod metrics;

use std::sync::Arc;
use tokio::sync::watch;
use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, WithdrawalScanMode},
};
use zksync_config::ViaBtcWatchConfig;
use zksync_dal::{Connection, ConnectionPool, Core};
use crate::metrics::METRICS;

#[derive(Debug)]
pub struct BtcWatch {
    btc_watch_config: ViaBtcWatchConfig,
    client: Arc<BitcoinClient>,
    indexer: BitcoinInscriptionIndexer,
    pool: ConnectionPool<Core>,
}
impl BtcWatch {
    pub fn new(
        btc_watch_config: ViaBtcWatchConfig,
        indexer: BitcoinInscriptionIndexer,
        client: Arc<BitcoinClient>,
        pool: ConnectionPool<Core>,
    ) -> Self {
        Self {
            btc_watch_config,
            client,
            indexer: indexer.with_withdrawal_mode(WithdrawalScanMode::Ignore),
            pool,
        }
    }
    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.btc_watch_config.poll_interval());
        let pool = self.pool.clone();

        let mut progressed = false;
        while !*stop_receiver.borrow_and_update() {
            if !progressed {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            }
            let result = async {
                let mut storage = pool.connection_tagged(BtcWatch::module_name()).await?;
                self.loop_iteration(&mut storage).await
            }.await;
            match result {
                Ok(next) => progressed = next,
                Err(err) => {
                    progressed = false;
                    if !via_btc_storage::transition::is_held(&err) {
                        return Err(err);
                    }
                    METRICS.errors.inc();
                    tracing::error!("Failed to process new blocks: {err}");
                }
            }
        }

        tracing::info!("Stop signal received, via_btc_watch is shutting down");
        Ok(())
    }

    async fn loop_iteration(&mut self, storage: &mut Connection<'_, Core>) -> anyhow::Result<bool> {
        via_btc_storage::transition::process_next(
            storage,
            &self.client,
            &mut self.indexer,
            via_consensus::consensus::BATCH_FINALIZATION_THRESHOLD,
        )
        .await
    }
    fn module_name() -> &'static str {
        "via_btc_watch"
    }
}
