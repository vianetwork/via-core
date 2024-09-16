use anyhow::{Context, Result};
use bincode::serialize;
use tokio::sync::watch;
use via_btc_client::{
    client::BitcoinClient,
    inscriber::Inscriber,
    traits::{BitcoinOps, Serializable},
    types::{InscriptionConfig, InscriptionMessage},
};
use zksync_config::ViaBtcSenderConfig;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_types::btc_sender::ViaBtcInscriptionRequest;

use crate::config::{BLOCK_CONFIRMATIONS, BLOCK_RESEND};

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
        self.update_inscription_status_or_resend(storage).await;
        Ok(())
    }

    async fn update_inscription_status_or_resend(&mut self, storage: &mut Connection<'_, Core>) {
        let inflight_inscriptions = storage
            .btc_sender_dal()
            .get_inflight_inscriptions()
            .await
            .unwrap();

        for inscription in inflight_inscriptions {
            if let Some(last_inscription_history) = storage
                .btc_sender_dal()
                .get_last_inscription_request_history(inscription.id)
                .await
                .context("Fetch the inscription request history")
                .unwrap()
            {
                let is_confirmed = self
                    .client
                    .check_tx_confirmation(
                        &last_inscription_history.reveal_tx_id,
                        BLOCK_CONFIRMATIONS,
                    )
                    .await
                    .unwrap();

                if is_confirmed {
                    storage
                        .btc_sender_dal()
                        .confirm_inscription(inscription.id, last_inscription_history.id)
                        .await
                        .unwrap();
                } else {
                    let current_block = self.client.fetch_block_height().await.unwrap();
                    if last_inscription_history.sent_at_block + BLOCK_RESEND as i64
                        > current_block as i64
                    {
                        continue;
                    }

                    let number_inscription_request_history = storage
                        .btc_sender_dal()
                        .get_total_inscription_request_history(inscription.id)
                        .await
                        .unwrap();

                    let config = InscriptionConfig {
                        fee_multiplier: number_inscription_request_history as u64 + 1,
                    };

                    self.send_inscription_tx(storage, &inscription, config)
                        .await
                        .unwrap();
                }
            }
        }
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
                let result = self
                    .send_inscription_tx(storage, &inscription, InscriptionConfig::default())
                    .await;
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
        config: InscriptionConfig,
    ) -> Result<(), anyhow::Error> {
        let sent_at_block = self
            .client
            .fetch_block_height()
            .await
            .context("Error to fetch current block number")
            .unwrap() as i64;

        let input =
            InscriptionMessage::from_bytes(&tx.inscription_message.clone().unwrap_or_default());

        let inscribe_info = self
            .inscriber
            .inscribe(input, config)
            .await
            .context("Sent inscription tx")
            .unwrap();

        let signed_commit_tx = serialize(&inscribe_info.final_commit_tx.tx)
            .context("Serilize the commit tx")
            .unwrap();
        let signed_reveal_tx = serialize(&inscribe_info.final_reveal_tx.tx)
            .context("Serilize the reveal tx")
            .unwrap();

        let actual_fees = inscribe_info.reveal_tx_output_info._reveal_fee
            + inscribe_info.commit_tx_output_info._commit_tx_fee;

        storage
            .btc_sender_dal()
            .insert_inscription_request_history(
                inscribe_info.final_commit_tx.txid,
                inscribe_info.final_reveal_tx.txid,
                tx.id,
                signed_commit_tx,
                signed_reveal_tx,
                actual_fees.to_sat() as i64,
                sent_at_block,
            )
            .await
            .unwrap();
        Ok(())
    }
}
