use std::str::FromStr;

use anyhow::{Context, Ok, Result};
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

            if let Err(err) = self.loop_iteration(&mut storage).await {
                // Web3 API request failures can cause this,
                // and anything more important is already properly reported.
                tracing::warn!("btc_sender_inscription_aggregator error {err:?}");
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

        if let Some(op) = self
            .aggregator
            .get_next_ready_operation(storage, base_system_contracts_hashes, protocol_version_id)
            .await?
        {
            let mut transaction = storage.start_transaction().await.unwrap();

            let inscription_message =
                InscriptionMessage::to_bytes(&op.get_inscription_message().clone());

            // Estimate the tx fee to execute the inscription request.
            let inscribe_info = self
                .inscriber
                .prepare_inscribe(
                    &op.get_inscription_message().clone(),
                    InscriptionConfig::default(),
                )
                .await
                .context("Get fee prepare inscriber")
                .unwrap();

            let prediction_fee = inscribe_info.reveal_tx_output_info._reveal_fee
                + inscribe_info.commit_tx_output_info._commit_tx_fee;

            let inscription_request = transaction
                .btc_sender_dal()
                .via_save_btc_inscriptions_request(
                    op.get_action_type(),
                    inscription_message,
                    prediction_fee.to_sat(),
                )
                .await
                .unwrap();

            transaction
                .via_blocks_dal()
                .set_inscription_request_id(
                    op.get_l1_batch_metadata().header.number,
                    inscription_request.id,
                    op.get_action_type(),
                )
                .await
                .unwrap();
            transaction.commit().await.unwrap();
        }
        Ok(())
    }

    // Todo: call indexer to fetch  the data
    async fn get_bootloader_code_hash(&self) -> anyhow::Result<H256> {
        let hex_str = "0000000000000000000000000000000000000000000000000000000000000000";
        Ok(H256::from_str(&hex_str).unwrap())
    }

    // Todo: call indexer to fetch  the data
    async fn get_aa_code_hash(&self) -> anyhow::Result<H256> {
        let hex_str = "0000000000000000000000000000000000000000000000000000000000000000";
        Ok(H256::from_str(&hex_str).unwrap())
    }

    // Todo: call indexer to fetch  the data
    async fn get_protocol_version_id(&self) -> anyhow::Result<ProtocolVersionId> {
        Ok(ProtocolVersionId::Version0)
    }
}
