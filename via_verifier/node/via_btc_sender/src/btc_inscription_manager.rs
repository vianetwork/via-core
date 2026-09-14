use anyhow::{Context, Result};
use bincode::serialize;
use tokio::sync::watch;
use via_btc_client::{
    inscriber::Inscriber, traits::Serializable, types::InscriptionMessage, InscriptionObserver,
};
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use zksync_config::ViaBtcSenderConfig;
use zksync_types::via_btc_sender::ViaBtcInscriptionRequest;

use crate::metrics::METRICS;

#[derive(Debug)]
pub struct ViaBtcInscriptionManager {
    inscriber: Inscriber,
    config: ViaBtcSenderConfig,
    pool: ConnectionPool<Verifier>,
    observer: InscriptionObserver,
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
            observer: InscriptionObserver::default(),
        })
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        self.observer.register();
        let mut timer = tokio::time::interval(self.config.poll_interval());
        let pool = self.pool.clone();

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            self.observer.invalidate();
            let mut storage = pool.connection_tagged("via_btc_sender").await?;

            match self.loop_iteration(&mut storage).await {
                Ok(()) => {}
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
        if storage
            .via_l1_block_dal()
            .has_reorg_in_progress()
            .await?
            .is_some()
        {
            return Ok(());
        }

        self.update_inscription_status_or_resend(storage).await?;
        self.send_new_inscription_txs(storage).await?;
        self.observer
            .observe(
                self.inscriber.get_client().await.fetch_block_height(),
                async {
                    Ok(storage
                        .via_block_dal()
                        .get_pending_inscription_attempts()
                        .await?)
                },
                self.config.stuck_inscription_block_number(),
                self.config.poll_interval(),
            )
            .await;
        Ok(())
    }

    async fn update_inscription_status_or_resend(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<()> {
        self.inscriber.sync_context_with_blockchain().await?;

        let inflight_inscriptions = storage
            .via_btc_sender_dal()
            .get_inflight_inscriptions()
            .await?;

        METRICS
            .inflight_inscriptions
            .set(inflight_inscriptions.len());

        METRICS.track_block_numbers(storage).await?;

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
                        self.config.block_confirmations,
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

                    METRICS.track_inscription_confirmation(last_inscription_history.created_at);
                }
            }
        }

        let balance = self.inscriber.get_balance().await?;
        METRICS.btc_sender_account_balance[&self.config.wallet_address.clone()]
            .set(balance as usize);

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
            .max_txs_in_flight
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
            .await? as i64;

        let input =
            InscriptionMessage::from_bytes(&tx.inscription_message.clone().unwrap_or_default());

        let latency = METRICS.broadcast_time.start();
        let inscribe_info = match self.inscriber.inscribe(input).await {
            Ok(info) => info,
            Err(e) => {
                METRICS.l1_transient_errors.inc();
                return Err(anyhow::anyhow!(e));
            }
        };
        latency.observe();

        let signed_commit_tx = serialize(&inscribe_info.final_commit_tx.tx)
            .with_context(|| "Error serializing the commit tx")?;

        let signed_reveal_tx = serialize(&inscribe_info.final_reveal_tx.tx)
            .with_context(|| "Error serializing the reveal tx")?;

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
