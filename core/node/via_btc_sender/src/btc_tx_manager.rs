use anyhow::{Context, Result};

use tokio::sync::watch;

use via_btc_client::{
    client::BitcoinClient, inscriber::Inscriber, traits::Serializable, types::InscriptionMessage,
};
use zksync_config::ViaBtcSenderConfig;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_types::btc_sender::ViaBtcInscriptionRequest;

pub struct ViaBtcInscriptionManager {
    client: BitcoinClient,
    inscriber: Inscriber,
    config: ViaBtcSenderConfig,
    pool: ConnectionPool<Core>,
}

impl ViaBtcInscriptionManager {
    pub async fn new(
        client: BitcoinClient,
        inscriber: Inscriber,
        pool: ConnectionPool<Core>,
        config: ViaBtcSenderConfig,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            client,
            inscriber,
            config,
            pool,
        })
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.config.poll_interval());
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
        storage: &mut Connection<'_, Core>,
    ) -> Result<(), anyhow::Error> {
        self.send_new_inscription_txs(storage).await;
        Ok(())
    }

    async fn send_new_inscription_txs(&mut self, storage: &mut Connection<'_, Core>) {
        let number_inflight_txs = storage
            .btc_sender_dal()
            .get_inflight_inscriptions()
            .await
            .unwrap()
            .len();

        let number_of_available_slots_for_inscription_txs = self
            .config
            .max_txs_in_flight()
            .saturating_sub(number_inflight_txs as i64);

        if number_of_available_slots_for_inscription_txs > 0 {
            let list_new_inscription_request = storage
                .btc_sender_dal()
                .list_new_inscription_request(number_of_available_slots_for_inscription_txs)
                .await
                .unwrap();

            for inscription in list_new_inscription_request {
                let result = self.send_inscription_tx(storage, &inscription).await;
                // If one of the transactions doesn't succeed, this means we should return
                // as new transactions have increasing nonces, so they will also result in an error
                // about gapped nonces
                if result.is_err() {
                    tracing::info!("Skipping sending rest of new transactions because of error");
                    break;
                }
            }
        }
    }

    pub(crate) async fn send_inscription_tx(
        &mut self,
        storage: &mut Connection<'_, Core>,
        tx: &ViaBtcInscriptionRequest,
    ) -> Result<(), anyhow::Error> {
        let sent_at_block = self
            .client
            .rpc
            .get_block_count()
            .await
            .context("Error to fetch current block number")
            .unwrap() as i64;

        let input =
            InscriptionMessage::from_bytes(&tx.inscription_message.clone().unwrap_or_default());
        let txs = self.inscriber.inscribe(input).await.unwrap();

        storage
            .btc_sender_dal()
            .insert_inscription_request_history(
                txs[0],
                txs[1],
                tx.id,
                0 as i64,   // Todo: add the context id
                Vec::new(), // Todo: add signature,
                0,
                sent_at_block,
            )
            .await
            .unwrap();
        Ok(())
    }
}
