use anyhow::Result;
use tokio::sync::watch;
use via_btc_client::inscriber::Inscriber;
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use zksync_config::ViaBtcSenderConfig;

use crate::metrics::METRICS;

#[derive(Debug)]
pub struct ViaBtcInscriptionManager {
    inscriber: Inscriber,
    config: ViaBtcSenderConfig,
    pool: ConnectionPool<Verifier>,
}

impl ViaBtcInscriptionManager {
    pub async fn new(
        inscriber: Inscriber, pool: ConnectionPool<Verifier>, config: ViaBtcSenderConfig,
    ) -> anyhow::Result<Self> {
        Ok(Self { inscriber, config, pool })
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
        crate::btc_vote_inscription::create_request(
            &self.pool,
            self.inscriber.get_client().await,
            &self.inscriber.inscriber_address()?,
        )
        .await?;
        self.update_inscription_status_or_resend(storage).await?;
        via_btc_storage::publication::publish_pending(
            &self.pool,
            &mut self.inscriber,
            self.config.max_txs_in_flight,
        )
        .await?;
        Ok(())
    }

    async fn update_inscription_status_or_resend(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<()> {
        self.inscriber.sync_context_with_blockchain().await?;

        let inflight_inscriptions = storage.via_btc_sender_dal().get_inflight_inscriptions().await?;

        via_btc_storage::metrics::METRICS.in_flight[&"verifier"].set(inflight_inscriptions.len());

        METRICS.track_block_numbers(storage).await?;

        let mut report_blocked_l1_batch_inscription: Option<u32> = None;

        for inscription in inflight_inscriptions {
            if let Some(last_inscription_history) =
                storage.via_btc_sender_dal().get_last_inscription_request_history(inscription.id).await?
            {
                // Keep sender completion at its configured depth, independently of content maturity.
                // Copied from Via's inherited confirmation path; only the guarded DB write differs: https://github.com/vianetwork/via-core/blob/3694bf7e2712a3ff1530f98d420cba1b262457f1/via_verifier/node/via_btc_sender/src/btc_inscription_manager.rs
                let is_confirmed = self
                    .inscriber
                    .get_client()
                    .await
                    .check_tx_confirmation(&last_inscription_history.reveal_tx_id, self.config.block_confirmations)
                    .await?;

                if is_confirmed {
                    let mut authority = via_btc_storage::transition::begin_authority(
                        &self.pool,
                        self.inscriber.get_client().await,
                        &[],
                    ).await?;
                    authority
                        .via_btc_sender_dal()
                        .confirm_inscription(inscription.id, last_inscription_history.id)
                        .await?;
                    authority.commit().await?;
                    tracing::info!(
                        "Inscription confirmed {reveal_tx}",
                        reveal_tx = last_inscription_history.reveal_tx_id,
                    );

                    via_btc_storage::metrics::METRICS.confirmed("verifier",last_inscription_history.created_at);
                } else {
                    let Some(current_block) = via_btc_storage::publication::stuck_inscription_height(
                        self.inscriber.get_client().await,
                        last_inscription_history.sent_at_block,
                        self.config.stuck_inscription_block_number(),
                    ).await? else { continue };

                    if report_blocked_l1_batch_inscription.is_none() {
                        let l1_batch_number = storage
                            .via_block_dal()
                            .get_first_stuck_l1_batch_number_inscription_request(
                                self.config.stuck_inscription_block_number(),
                                current_block,
                            )
                            .await?;

                        METRICS.report_blocked_l1_batch_inscription.set(l1_batch_number as usize);

                        report_blocked_l1_batch_inscription = Some(l1_batch_number);
                        tracing::warn!(
                            "Inscription {} stuck for more than {} block.",
                            last_inscription_history.reveal_tx_id,
                            self.config.stuck_inscription_block_number()
                        );
                    }
                }
            }
        }

        Ok(())
    }
}
