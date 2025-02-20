use anyhow::{Context, Result};
use bincode::serialize;
use tokio::sync::watch;
use via_btc_client::{inscriber::Inscriber, traits::Serializable, types::InscriptionMessage};
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use zksync_config::ViaBtcSenderConfig;
use zksync_types::btc_sender::ViaBtcInscriptionRequest;

use crate::config::BLOCK_RESEND;

#[derive(Debug)]
pub struct ViaBtcInscriptionManager {
    inscriber: Inscriber,
    config: ViaBtcSenderConfig,
    pool: ConnectionPool<Verifier>,
}

impl ViaBtcInscriptionManager {
    pub async fn new(
        inscriber: Inscriber,
        pool: ConnectionPool<Verifier>,
        config: ViaBtcSenderConfig,
    ) -> anyhow::Result<Self> {
        Ok(Self {
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
                Ok(()) => {
                    tracing::info!("Inscription manager task finished");
                }
                Err(err) => {
                    tracing::error!("Failed to process btc_sender_inscription_manager: {err}");
                }
            }
        }

        tracing::info!("Stop signal received, btc_sender is shutting down");
        Ok(())
    }

    async fn loop_iteration(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> Result<(), anyhow::Error> {
        self.update_inscription_status_or_resend(storage).await?;
        self.send_new_inscription_txs(storage).await?;
        Ok(())
    }

    async fn update_inscription_status_or_resend(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<()> {
        let inflight_inscriptions = storage
            .via_btc_sender_dal()
            .get_inflight_inscriptions()
            .await?;

        for inscription in inflight_inscriptions {
            if let Some(last_inscription_history) = storage
                .via_btc_sender_dal()
                .get_last_inscription_request_history(inscription.id)
                .await?
            {
                let is_confirmed = self
                    .inscriber
                    .get_client()
                    .await
                    .check_tx_confirmation(
                        &last_inscription_history.reveal_tx_id,
                        self.config.block_confirmations(),
                    )
                    .await?;

                if is_confirmed {
                    storage
                        .via_btc_sender_dal()
                        .confirm_inscription(inscription.id, last_inscription_history.id)
                        .await?;
                    tracing::info!(
                        "Inscription confirmed {reveal_tx}",
                        reveal_tx = last_inscription_history.reveal_tx_id,
                    );
                } else {
                    let current_block = self
                        .inscriber
                        .get_client()
                        .await
                        .fetch_block_height()
                        .await
                        .context("context")?;

                    if last_inscription_history.sent_at_block + BLOCK_RESEND as i64
                        > current_block as i64
                    {
                        continue;
                    }

                    tracing::warn!(
                        "Inscription {reveal_tx} stuck for more than {BLOCK_RESEND} block.",
                        reveal_tx = last_inscription_history.reveal_tx_id
                    );
                }
            }
        }

        Ok(())
    }

    async fn send_new_inscription_txs(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<()> {
        let number_inflight_txs = storage
            .via_btc_sender_dal()
            .get_inflight_inscriptions()
            .await?
            .len();

        tracing::debug!(
            "Inflight inscriptions: {count}",
            count = number_inflight_txs
        );

        let number_of_available_slots_for_inscription_txs = self
            .config
            .max_txs_in_flight()
            .saturating_sub(number_inflight_txs as i64);

        tracing::debug!(
            "Available slots to process inscriptions: {count}",
            count = number_of_available_slots_for_inscription_txs
        );

        if number_of_available_slots_for_inscription_txs > 0 {
            let list_new_inscription_request = storage
                .via_btc_sender_dal()
                .list_new_inscription_request(number_of_available_slots_for_inscription_txs)
                .await?;

            for inscription in list_new_inscription_request {
                self.send_inscription_tx(storage, &inscription).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn send_inscription_tx(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        tx: &ViaBtcInscriptionRequest,
    ) -> anyhow::Result<()> {
        let sent_at_block = self
            .inscriber
            .get_client()
            .await
            .fetch_block_height()
            .await
            .context("Error to fetch current block number")? as i64;

        let input =
            InscriptionMessage::from_bytes(&tx.inscription_message.clone().unwrap_or_default());

        let inscribe_info = self
            .inscriber
            .inscribe(input)
            .await
            .context("Sent inscription tx")?;

        let signed_commit_tx =
            serialize(&inscribe_info.final_commit_tx.tx).context("Serilize the commit tx")?;
        let signed_reveal_tx =
            serialize(&inscribe_info.final_reveal_tx.tx).context("Serilize the reveal tx")?;

        let actual_fees = inscribe_info.reveal_tx_output_info._reveal_fee
            + inscribe_info.commit_tx_output_info.commit_tx_fee;

        tracing::info!(
            "New inscription created {commit_tx} {reveal_tx}",
            commit_tx = inscribe_info.final_commit_tx.txid,
            reveal_tx = inscribe_info.final_reveal_tx.txid,
        );

        storage
            .via_btc_sender_dal()
            .insert_inscription_request_history(
                inscribe_info.final_commit_tx.txid.to_string(),
                inscribe_info.final_reveal_tx.txid.to_string(),
                tx.id,
                signed_commit_tx,
                signed_reveal_tx,
                actual_fees.to_sat() as i64,
                sent_at_block,
            )
            .await?;
        Ok(())
    }
}
