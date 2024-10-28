use std::str::FromStr;

use anyhow::{Context, Result};
use tokio::sync::watch;
use via_btc_client::{
    inscriber::Inscriber,
    traits::Serializable,
    types::{InscriptionConfig, InscriptionMessage},
};
use zksync_config::ViaBtcSenderConfig;
use zksync_contracts::BaseSystemContractsHashes;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_types::{ProtocolVersionId, H256};

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
        let base_system_contracts_hashes = BaseSystemContractsHashes {
            bootloader: self.get_bootloader_code_hash().await?,
            default_aa: self.get_aa_code_hash().await?,
        };
        let protocol_version_id = self.get_protocol_version_id().await?;

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
                    .prepare_inscribe(&inscription_message, InscriptionConfig::default(), None)
                    .await
                    .context("Via get inscriber info")?;

                let prediction_fee = inscribe_info.reveal_tx_output_info._reveal_fee
                    + inscribe_info.commit_tx_output_info._commit_tx_fee;

                let inscription_request = transaction
                    .btc_sender_dal()
                    .via_save_btc_inscriptions_request(
                        operation.get_inscription_request_type(),
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

    // Todo: call indexer to fetch  the data
    async fn get_bootloader_code_hash(&self) -> anyhow::Result<H256> {
        let hex_str = "010008e742608b21bf7eb23c1a9d0602047e3618b464c9b59c0fba3b3d7ab66e";
        Ok(H256::from_str(hex_str).unwrap())
    }

    // Todo: call indexer to fetch  the data
    async fn get_aa_code_hash(&self) -> anyhow::Result<H256> {
        let hex_str = "01000563374c277a2c1e34659a2a1e87371bb6d852ce142022d497bfb50b9e32";
        Ok(H256::from_str(hex_str).unwrap())
    }

    // Todo: call indexer to fetch  the data
    async fn get_protocol_version_id(&self) -> anyhow::Result<ProtocolVersionId> {
        Ok(ProtocolVersionId::latest())
    }
}
