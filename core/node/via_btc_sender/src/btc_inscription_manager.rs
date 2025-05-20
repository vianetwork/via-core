use anyhow::{Context, Result};
use bincode::serialize;
use bitcoin::hashes::Hash;
use tokio::sync::watch;
use via_btc_client::{inscriber::Inscriber, traits::Serializable, types::InscriptionMessage};
use zksync_config::ViaBtcSenderConfig;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_shared_metrics::BlockL1Stage;
use zksync_types::{
    btc_inscription_operations::ViaBtcInscriptionRequestType,
    via_btc_sender::ViaBtcInscriptionRequest,
};

use crate::{config::INSCRIPTION_EXECUTION_BLOCK_DELAY, metrics::METRICS};

#[derive(Debug)]
pub struct ViaBtcInscriptionManager {
    inscriber: Inscriber,
    config: ViaBtcSenderConfig,
    pool: ConnectionPool<Core>,
}

impl ViaBtcInscriptionManager {
    pub async fn new(
        inscriber: Inscriber,
        pool: ConnectionPool<Core>,
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
        storage: &mut Connection<'_, Core>,
    ) -> Result<(), anyhow::Error> {
        self.update_inscription_status(storage).await?;
        self.send_new_inscription_txs(storage).await?;
        Ok(())
    }

    async fn update_inscription_status(
        &mut self,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<()> {
        let inflight_inscriptions_ids = storage
            .btc_sender_dal()
            .list_inflight_inscription_ids()
            .await?;

        METRICS
            .inflight_inscriptions
            .set(inflight_inscriptions_ids.len());

        let mut report_blocked_l1_batch_inscription: Option<u32> = None;

        for inscription_id in inflight_inscriptions_ids {
            if let Some(last_inscription_history) = storage
                .btc_sender_dal()
                .get_last_inscription_request_history(inscription_id)
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

                METRICS.track_block_numbers(storage).await;

                if is_confirmed {
                    let inscription = storage
                        .btc_sender_dal()
                        .confirm_inscription(inscription_id, last_inscription_history.id)
                        .await?;
                    tracing::info!(
                        "Inscription confirmed {reveal_tx}",
                        reveal_tx = last_inscription_history.reveal_tx_id,
                    );

                    METRICS
                        .track_btc_tx_metrics(
                            storage,
                            BlockL1Stage::Mined,
                            vec![(
                                inscription.id as u32,
                                ViaBtcInscriptionRequestType::from(inscription.request_type),
                            )],
                        )
                        .await;

                    METRICS.track_inscription_confirmation(last_inscription_history.created_at);
                } else {
                    let current_block = self
                        .inscriber
                        .get_client()
                        .await
                        .fetch_block_height()
                        .await?;

                    if last_inscription_history.sent_at_block
                        + INSCRIPTION_EXECUTION_BLOCK_DELAY as i64
                        > current_block as i64
                    {
                        continue;
                    }

                    if report_blocked_l1_batch_inscription.is_none() {
                        let l1_batch_number = storage
                            .via_blocks_dal()
                            .get_first_stuck_l1_batch_number_inscription_request(
                                INSCRIPTION_EXECUTION_BLOCK_DELAY,
                                current_block,
                            )
                            .await?;

                        METRICS
                            .report_blocked_l1_batch_inscription
                            .set(l1_batch_number as usize);

                        report_blocked_l1_batch_inscription = Some(l1_batch_number);

                        tracing::warn!(
                        "Inscription {reveal_tx} stuck for more than {INSCRIPTION_EXECUTION_BLOCK_DELAY} block.",
                        reveal_tx = last_inscription_history.reveal_tx_id
                    );
                    }
                }
            }
        }

        let balance = self.inscriber.get_balance().await?;
        METRICS.btc_sender_account_balance.set(balance as usize);

        Ok(())
    }

    async fn send_new_inscription_txs(
        &mut self,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<()> {
        let number_inflight_txs = storage
            .btc_sender_dal()
            .list_inflight_inscription_ids()
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
                .btc_sender_dal()
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
        storage: &mut Connection<'_, Core>,
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
            .btc_sender_dal()
            .insert_inscription_request_history(
                &inscribe_info
                    .final_commit_tx
                    .txid
                    .as_raw_hash()
                    .to_byte_array(),
                &inscribe_info
                    .final_reveal_tx
                    .txid
                    .as_raw_hash()
                    .to_byte_array(),
                tx.id,
                &signed_commit_tx,
                &signed_reveal_tx,
                actual_fees.to_sat() as i64,
                sent_at_block,
            )
            .await?;
        METRICS.pending_inscription_requests.dec_by(1);
        Ok(())
    }
}
