use anyhow::Result;
use tokio::sync::watch;
use via_btc_client::inscriber::Inscriber;
use zksync_config::ViaBtcSenderConfig;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_shared_metrics::BlockL1Stage;
use zksync_types::{btc_inscription_operations::ViaBtcInscriptionRequestType};

use crate::metrics::METRICS;

#[derive(Debug)]
pub struct ViaBtcInscriptionManager {
    inscriber: Inscriber,
    config: ViaBtcSenderConfig,
    pool: ConnectionPool<Core>,
}

impl ViaBtcInscriptionManager {
    pub async fn new(
        inscriber: Inscriber, pool: ConnectionPool<Core>, config: ViaBtcSenderConfig,
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
                    METRICS.manager_errors.inc();
                    tracing::error!("Failed to process btc_sender_inscription_manager: {err}");
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
        self.update_inscription_status(storage).await?;
        via_btc_storage::publication::publish_pending(
            &self.pool,
            &mut self.inscriber,
            self.config.max_txs_in_flight,
        )
        .await?;
        Ok(())
    }

    async fn update_inscription_status(
        &mut self,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<()> {
        self.inscriber.sync_context_with_blockchain().await?;

        let inflight_inscriptions_ids = storage.btc_sender_dal().list_inflight_inscription_ids().await?;

        via_btc_storage::metrics::METRICS.in_flight[&"core"].set(inflight_inscriptions_ids.len());

        let mut report_blocked_l1_batch_inscription: Option<u32> = None;

        let wallets = via_btc_storage::ReaderDal::new(storage).mature_wallets().await?;
        anyhow::ensure!(
            wallets.sequencer == self.inscriber.inscriber_address()?,
            "BTC sender wallet differs from canonical sequencer"
        );
        METRICS.track_block_numbers(storage).await;

        for inscription_id in inflight_inscriptions_ids {
            // jscpd:ignore-start inherited sender confirmation flow. Role adapters preserve upstream shape.
            if let Some(last_inscription_history) =
                storage.btc_sender_dal().get_last_inscription_request_history(inscription_id).await?
            {
                // Keep sender completion at its configured depth, independently of content maturity.
                // Copied from Via's inherited confirmation path; only the guarded DB write differs: https://github.com/vianetwork/via-core/blob/3694bf7e2712a3ff1530f98d420cba1b262457f1/core/node/via_btc_sender/src/btc_inscription_manager.rs
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
                    // jscpd:ignore-end
                    let inscription = authority
                        .btc_sender_dal()
                        .confirm_inscription(inscription_id, last_inscription_history.id)
                        .await?;
                    authority.commit().await?;
                    tracing::info!(
                        "Inscription confirmed {reveal_tx}",
                        reveal_tx = last_inscription_history.reveal_tx_id,
                    );

                    METRICS
                        .track_btc_tx_metrics(
                            storage,
                            BlockL1Stage::Mined,
                            vec![(inscription.id as u32, ViaBtcInscriptionRequestType::from(inscription.request_type))],
                        )
                        .await;

                    // jscpd:ignore-start publication::stuck_inscription_height. Reporting by role.
                    via_btc_storage::metrics::METRICS.confirmed("core",last_inscription_history.created_at);
                } else {
                    let Some(current_block) = via_btc_storage::publication::stuck_inscription_height(
                        self.inscriber.get_client().await,
                        last_inscription_history.sent_at_block,
                        self.config.stuck_inscription_block_number(),
                    ).await? else { continue };

                    if report_blocked_l1_batch_inscription.is_none() {
                        let l1_batch_number = storage
                            .via_blocks_dal()
                            // jscpd:ignore-end
                            .get_first_stuck_l1_batch_number_inscription_request(
                                self.config.stuck_inscription_block_number(),
                                current_block,
                            )
                            .await?;

                        METRICS.report_blocked_l1_batch_inscription.set(l1_batch_number as usize);

                        report_blocked_l1_batch_inscription = Some(l1_batch_number);

                        tracing::debug!(
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

#[cfg(test)]
#[path = "tests/btc_inscription_manager_test.rs"]
pub(crate) mod test_support;
