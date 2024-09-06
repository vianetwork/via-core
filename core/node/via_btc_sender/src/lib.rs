use std::{sync::Arc, time::Duration};

use anyhow::Result; // add context in logic implementation phase
use tokio::sync::watch;
// re-exporting here isn't necessary, but it's a good practice to keep all the public types in one place
pub use via_btc_client::types::{BitcoinNetwork, NodeAuth};
use via_btc_client::{inscriber::Inscriber, types::InscriberContext};
use zksync_config::{ObjectStoreConfig, ViaBtcSenderConfig};
use zksync_dal::{Connection, ConnectionPool, Core};
use zksync_object_store::{ObjectStore, ObjectStoreFactory};

#[derive(Debug)]
pub struct BtcSender {
    _inscriber: Inscriber,
    pool: ConnectionPool<Core>,
    poll_interval: Duration,
    _object_store: Arc<dyn ObjectStore>,
}

impl BtcSender {
    pub async fn new(
        config: ViaBtcSenderConfig,
        _pool: ConnectionPool<Core>,
        _poll_interval: Duration,
        object_store_conf: ObjectStoreConfig,
    ) -> anyhow::Result<Self> {
        // create a new instance of the BitcoinInscriber
        // read context from database and create a new instance of the BitcoinInscriber context

        let network = match config.network() {
            "mainnet" => BitcoinNetwork::Bitcoin,
            "testnet" => BitcoinNetwork::Testnet,
            "regtest" => BitcoinNetwork::Regtest,
            _ => return Err(anyhow::anyhow!("Invalid network")),
        };

        let auth = NodeAuth::UserPass(
            config.rpc_user().to_string(),
            config.rpc_password().to_string(),
        );

        // TODO: Read the actor role and apply the logic accordingly

        // Read the persisted context from the gcs bucket

        let object_store = ObjectStoreFactory::new(object_store_conf)
            .create_store()
            .await?;

        // read latest stored context id from the database (id of the last stored inscriber transaction history)

        let temp_id = 2_u32;

        // read the context from the object store

        let context = object_store.get::<InscriberContext>(temp_id).await?;

        let inscriber = Inscriber::new(
            config.rpc_url(),
            network,
            auth,
            config.private_key(),
            Some(context),
        )
        .await?;

        Ok(Self {
            _inscriber: inscriber,
            pool: _pool,
            poll_interval: _poll_interval,
            _object_store: object_store,
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
