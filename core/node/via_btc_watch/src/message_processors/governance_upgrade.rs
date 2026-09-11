use std::sync::Arc;

use anyhow::Context;
use via_btc_client::{
    indexer::{resolve_upgrade_proposals, BitcoinInscriptionIndexer},
    traits::BitcoinOps,
    types::FullInscriptionMessage,
};
use zksync_dal::{Connection, Core, CoreDal, DalError};
use zksync_types::{
    protocol_version::ProtocolSemanticVersion, via_protocol_upgrade::ViaProtocolUpgrade,
    ProtocolUpgrade,
};

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::{InscriptionStage, METRICS},
};

/// Listens to operation events coming from the governance contract and saves new protocol upgrade proposals to the database.
#[derive(Debug)]
pub struct GovernanceUpgradesEventProcessor {
    /// Last protocol version seen. Used to skip events for already known upgrade proposals.
    last_seen_protocol_version: ProtocolSemanticVersion,
    btc_client: Arc<dyn BitcoinOps>,
}

impl GovernanceUpgradesEventProcessor {
    pub fn new(
        btc_client: Arc<dyn BitcoinOps>,
        last_seen_protocol_version: ProtocolSemanticVersion,
    ) -> Self {
        Self {
            last_seen_protocol_version,
            btc_client,
        }
    }
}
#[async_trait::async_trait]
impl MessageProcessor for GovernanceUpgradesEventProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        let mut upgrades = Vec::new();
        for msg in msgs {
            let FullInscriptionMessage::SystemContractUpgrade(activation) = msg else {
                continue;
            };
            for input in resolve_upgrade_proposals(self.btc_client.as_ref(), &activation).await? {
                if input.version <= self.last_seen_protocol_version {
                    tracing::info!(
                        "Upgrade transaction with version {} already processed, skipping",
                        input.version
                    );
                    continue;
                }

                tracing::info!("Received upgrades with versions: {:?}", input.version);
                let tx = ViaProtocolUpgrade::default()
                    .create_protocol_upgrade_tx(input.version, input.system_contracts)?;

                let upgrade = ProtocolUpgrade {
                    version: input.version,
                    bootloader_code_hash: Some(input.bootloader_code_hash),
                    default_account_code_hash: Some(input.default_account_code_hash),
                    evm_emulator_code_hash: input.evm_emulator_code_hash,
                    tx: Some(tx),
                    timestamp: 0,
                    verifier_address: None,
                    verifier_params: None,
                };
                upgrades.push((upgrade, input.recursion_scheduler_level_vk_hash));
            }
        }

        let Some(last_upgrade) = upgrades.last() else {
            return Ok(None);
        };

        let last_version = last_upgrade.0.version;
        for (upgrade, recursion_scheduler_level_vk_hash) in upgrades {
            let latest_semantic_version = storage
                .protocol_versions_dal()
                .latest_semantic_version()
                .await
                .map_err(DalError::generalize)?
                .context("expected some version to be present in DB")?;

            if upgrade.version > latest_semantic_version {
                let latest_version = storage
                    .protocol_versions_dal()
                    .get_protocol_version_with_latest_patch(latest_semantic_version.minor)
                    .await
                    .map_err(DalError::generalize)?
                    .with_context(|| {
                        format!(
                            "expected minor version {} to be present in DB",
                            latest_semantic_version.minor as u16
                        )
                    })?;

                let new_version =
                    latest_version.apply_upgrade(upgrade, Some(recursion_scheduler_level_vk_hash));

                storage
                    .protocol_versions_dal()
                    .save_protocol_version_with_tx(&new_version)
                    .await
                    .map_err(DalError::generalize)?;

                METRICS.inscriptions_processed[&InscriptionStage::Upgrade]
                    .set(new_version.version.minor as usize);
            }
        }
        self.last_seen_protocol_version = last_version;

        Ok(None)
    }
}
