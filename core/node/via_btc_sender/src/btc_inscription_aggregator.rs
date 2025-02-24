use anyhow::{Context, Result};
use tokio::sync::watch;
use via_btc_client::{inscriber::Inscriber, traits::Serializable, types::InscriptionMessage};
use zksync_config::ViaBtcSenderConfig;
use zksync_contracts::BaseSystemContractsHashes;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_types::ProtocolVersionId;

use crate::aggregator::ViaAggregator;

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
                Ok(()) => {
                    tracing::info!("Inscription aggregation task finished");
                }
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
        let protocol_version_id = self.get_protocol_version_id().await?;

        let base_system_contracts_hashes = self
            .load_base_system_contracts(storage, protocol_version_id)
            .await?;

        if let Some(operation) = self
            .aggregator
            .get_next_ready_operation(storage, base_system_contracts_hashes, protocol_version_id)
            .await?
        {
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
                    .await
                    .context("Via get inscriber info")?;

                let prediction_fee = inscribe_info.reveal_tx_output_info._reveal_fee
                    + inscribe_info.commit_tx_output_info.commit_tx_fee;

                let inscription_request = transaction
                    .btc_sender_dal()
                    .via_save_btc_inscriptions_request(
                        batch.number,
                        operation.get_inscription_request_type().to_string(),
                        InscriptionMessage::to_bytes(&inscription_message),
                        prediction_fee.to_sat(),
                    )
                    .await
                    .context("Via save btc inscriptions request")?;

                transaction
                    .via_blocks_dal()
                    .insert_l1_batch_inscription_request_id(
                        batch.number,
                        inscription_request.id,
                        operation.get_inscription_request_type(),
                    )
                    .await
                    .context("Via set inscription request id")?;
            }
            transaction.commit().await?;
        }
        Ok(())
    }

    async fn load_base_system_contracts(
        &self,
        storage: &mut Connection<'_, Core>,
        protocol_version: ProtocolVersionId,
    ) -> anyhow::Result<BaseSystemContractsHashes> {
        let base_system_contracts = storage
            .protocol_versions_dal()
            .load_base_system_contracts_by_version_id(protocol_version as u16)
            .await
            .context("failed loading base system contracts")?;
        if let Some(contracts) = base_system_contracts {
            return Ok(BaseSystemContractsHashes {
                bootloader: contracts.bootloader.hash,
                default_aa: contracts.default_aa.hash,
            });
        }
        anyhow::bail!(
            "Failed to load the base system contracts for version {}",
            protocol_version
        )
    }

    async fn get_protocol_version_id(&self) -> anyhow::Result<ProtocolVersionId> {
        Ok(ProtocolVersionId::latest())
    }
}
