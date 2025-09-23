use anyhow::Result;
use tokio::sync::watch;
use via_btc_client::{inscriber::Inscriber, traits::Serializable, types::InscriptionMessage};
use zksync_config::ViaBtcSenderConfig;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_shared_metrics::BlockL1Stage;

use crate::{aggregator::ViaAggregator, metrics::METRICS};

#[derive(Debug)]
pub struct ViaBtcInscriptionAggregator {
    inscriber: Inscriber,
    aggregator: ViaAggregator,
    pool: ConnectionPool<Core>,
    config: ViaBtcSenderConfig,
}

impl ViaBtcInscriptionAggregator {
    pub async fn new(
        inscriber: Inscriber,
        pool: ConnectionPool<Core>,
        config: ViaBtcSenderConfig,
    ) -> anyhow::Result<Self> {
        let aggregator = ViaAggregator::new(config.clone());

        Ok(Self {
            inscriber,
            aggregator,
            pool,
            config,
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

            let mut storage = pool
                .connection_tagged("via_btc_inscription_creator")
                .await?;

            match self.loop_iteration(&mut storage).await {
                Ok(()) => {}
                Err(err) => {
                    tracing::error!("Failed to process btc_sender_inscription_aggregator: {err}");
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
        if storage
            .via_l1_block_dal()
            .has_reorg_in_progress()
            .await?
            .is_some()
        {
            return Ok(());
        }

        let latency = METRICS.inscription_preparation_time.start();
        let mut processed_inscriptions = vec![];

        if let Some(operation) = self.aggregator.get_next_ready_operation(storage).await? {
            tracing::info!("New operation ready to be processed {operation}");

            let mut transaction = storage.start_transaction().await?;

            for batch in operation.get_l1_batches_detail() {
                let inscription_message = self.aggregator.construct_inscription_message(
                    &operation.get_inscription_request_type(),
                    batch,
                )?;

                // Estimate the tx fee to execute the inscription request.
                let inscribe_info = self
                    .inscriber
                    .prepare_inscribe(&inscription_message, None)
                    .await?;

                let prediction_fee = inscribe_info.reveal_tx_output_info._reveal_fee
                    + inscribe_info.commit_tx_output_info.commit_tx_fee;

                let inscription_request_id = transaction
                    .btc_sender_dal()
                    .via_save_btc_inscriptions_request(
                        batch.number,
                        operation.get_inscription_request_type().to_string(),
                        InscriptionMessage::to_bytes(&inscription_message),
                        prediction_fee.to_sat(),
                    )
                    .await?;

                transaction
                    .via_blocks_dal()
                    .insert_l1_batch_inscription_request_id(
                        batch.number,
                        inscription_request_id,
                        operation.get_inscription_request_type(),
                    )
                    .await?;

                processed_inscriptions.push((
                    inscription_request_id as u32,
                    operation.get_inscription_request_type(),
                ));
            }
            transaction.commit().await?;

            METRICS
                .track_btc_tx_metrics(storage, BlockL1Stage::Mined, processed_inscriptions)
                .await;
            latency.observe();
            METRICS.pending_inscription_requests.inc_by(1);
        }
        Ok(())
    }
}
