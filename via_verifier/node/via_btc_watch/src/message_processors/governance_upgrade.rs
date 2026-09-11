use std::sync::Arc;

use via_btc_client::{
    indexer::{resolve_upgrade_proposals, BitcoinInscriptionIndexer},
    traits::BitcoinOps,
    types::FullInscriptionMessage,
};
use via_verifier_dal::{Connection, DalError, Verifier, VerifierDal};
use via_verifier_types::protocol_version::get_sequencer_version;
use zksync_types::via_protocol_upgrade::ViaProtocolUpgrade;

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::{InscriptionStage, METRICS},
};

/// Listens to operation events coming from the governance contract and saves new protocol upgrade proposals to the database.
#[derive(Debug)]
pub struct GovernanceUpgradesEventProcessor {
    btc_client: Arc<dyn BitcoinOps>,
}

impl GovernanceUpgradesEventProcessor {
    pub fn new(btc_client: Arc<dyn BitcoinOps>) -> Self {
        Self { btc_client }
    }
}

#[async_trait::async_trait]
impl MessageProcessor for GovernanceUpgradesEventProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        let mut upgrades = Vec::new();
        for msg in msgs {
            let FullInscriptionMessage::SystemContractUpgrade(activation) = msg else {
                continue;
            };
            for input in resolve_upgrade_proposals(self.btc_client.as_ref(), &activation).await? {
                if input.version < get_sequencer_version() {
                    tracing::info!(
                        "Upgrade transaction with version {} already processed, skipping",
                        input.version
                    );
                    continue;
                }

                tracing::info!("Received upgrades with versions: {:?}", input.version);

                let hash = ViaProtocolUpgrade::default()
                    .get_canonical_tx_hash(input.version, input.system_contracts)?;
                upgrades.push((
                    input.version,
                    input.bootloader_code_hash,
                    input.default_account_code_hash,
                    hash,
                    input.recursion_scheduler_level_vk_hash,
                ));
            }
        }

        for (
            version,
            bootloader_code_hash,
            default_account_code_hash,
            canonical_tx_hash,
            recursion_scheduler_level_vk_hash,
        ) in upgrades
        {
            METRICS.inscriptions_processed[&InscriptionStage::Upgrade].set(version.minor as usize);

            storage
                .via_protocol_versions_dal()
                .save_protocol_version(
                    version,
                    bootloader_code_hash.as_bytes(),
                    default_account_code_hash.as_bytes(),
                    canonical_tx_hash.as_bytes(),
                    recursion_scheduler_level_vk_hash.as_bytes(),
                )
                .await
                .map_err(DalError::generalize)?;
        }
        Ok(None)
    }
}
