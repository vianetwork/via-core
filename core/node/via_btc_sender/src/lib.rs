use std::time::Duration;

use anyhow::Result; // add context in logic implementation phase
use tokio::sync::watch;
use via_btc_client::inscriber::Inscriber;
// re-exporting here isn't necessary, but it's a good practice to keep all the public types in one place
pub use via_btc_client::types::BitcoinNetwork;
use zksync_dal::{Connection, ConnectionPool, Core};

#[derive(Debug)]
pub struct BtcSender {
    _inscriber: Inscriber,
    pool: ConnectionPool<Core>,
    poll_interval: Duration,
}

impl BtcSender {
    pub async fn new(
        _rpc_url: &str,
        _rpc_username: &str,
        _rpc_password: &str,
        _pool: ConnectionPool<Core>,
        _poll_interval: Duration,
    ) -> anyhow::Result<Self> {
        // create a new instance of the BitcoinInscriber
        // read context from database and create a new instance of the BitcoinInscriber context
        todo!();
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.poll_interval);
        let pool = self.pool.clone();

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            let mut storage = pool.connection_tagged("via_btc_sender").await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => { /* everything went fine */ }
                Err(err) => {
                    tracing::error!("Failed to process new blocks: {err}");
                }
            }
        }

        tracing::info!("Stop signal received, btc_sender is shutting down");
        Ok(())
    }

    async fn loop_iteration(
        &mut self,
        _storage: &mut Connection<'_, Core>,
    ) -> Result<(), anyhow::Error> {
        todo!();
    }
}
