use std::sync::Arc;

use anyhow::Context as _;
use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, MessageParser},
    traits::BitcoinOps,
    types::FullInscriptionMessage,
};
use zksync_dal::{Connection, Core, CoreDal, DalError};
use zksync_types::{via_protocol_upgrade::ViaProtocolUpgrade, ProtocolUpgrade};

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::{InscriptionStage, METRICS},
};

/// Listens to operation events coming from the governance contract and saves new protocol upgrade proposals to the database.
#[derive(Debug)]
pub struct GovernanceUpgradesEventProcessor {
    /// BTC client
    btc_client: Arc<BitcoinClient>,
}

impl GovernanceUpgradesEventProcessor {
    pub fn new(btc_client: Arc<BitcoinClient>) -> Self {
        Self { btc_client }
    }
}
#[async_trait::async_trait]
impl MessageProcessor for GovernanceUpgradesEventProcessor {
    async fn process_messages(
        &mut self, storage: &mut Connection<'_, Core>, msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        if !msgs.iter().any(|message| matches!(message, FullInscriptionMessage::SystemContractUpgrade(_))) {
            return Ok(None);
        }
        let latest_committed_version = storage
            .protocol_versions_dal()
            .latest_semantic_version()
            .await
            .map_err(DalError::generalize)?
            .context("expected some version to be present in DB")?;
        let mut message_parser = MessageParser::new(self.btc_client.get_network());
        let mut upgrades = Vec::new();
        for msg in msgs {
            if let FullInscriptionMessage::SystemContractUpgrade(system_contract_upgrade_msg) = &msg {
                let proposal_tx =
                    self.btc_client.get_transaction(&system_contract_upgrade_msg.input.proposal_tx_id).await.map_err(
                        |err| {
                            MessageProcessorError::Internal(anyhow::anyhow!(
                                "Failed to fetch protocol upgrade transaction: {}, error {}",
                                system_contract_upgrade_msg.input.proposal_tx_id,
                                err
                            ))
                        },
                    )?;

                let messages = message_parser.parse_system_transaction(
                    &proposal_tx,
                    system_contract_upgrade_msg.common.block_height,
                    None,
                );

                for message in messages {
                    match message {
                        FullInscriptionMessage::SystemContractUpgradeProposal(system_contract_upgrade_proposal_msg) => {
                            if system_contract_upgrade_proposal_msg.input.version <= latest_committed_version {
                                tracing::info!(
                                    "Upgrade transaction with version {} already processed, skipping",
                                    system_contract_upgrade_proposal_msg.input.version
                                );
                                continue;
                            }
                            tracing::info!(
                                "Received upgrades with versions: {:?}",
                                system_contract_upgrade_proposal_msg.input.version
                            );
                            let tx = ViaProtocolUpgrade::default().create_protocol_upgrade_tx(
                                system_contract_upgrade_proposal_msg.input.version,
                                system_contract_upgrade_proposal_msg.input.system_contracts,
                            )?;

                            let upgrade = ProtocolUpgrade {
                                version: system_contract_upgrade_proposal_msg.input.version,
                                bootloader_code_hash: Some(
                                    system_contract_upgrade_proposal_msg.input.bootloader_code_hash,
                                ),
                                default_account_code_hash: Some(
                                    system_contract_upgrade_proposal_msg.input.default_account_code_hash,
                                ),
                                evm_emulator_code_hash: system_contract_upgrade_proposal_msg
                                    .input
                                    .evm_emulator_code_hash,
                                tx: Some(tx),
                                timestamp: 0,
                                verifier_address: None,
                                verifier_params: None,
                            };
                            upgrades.push((
                                upgrade,
                                system_contract_upgrade_proposal_msg.input.recursion_scheduler_level_vk_hash,
                            ));
                        }
                        _ => (),
                    }
                }
            }
        }

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
                        format!("expected minor version {} to be present in DB", latest_semantic_version.minor as u16)
                    })?;

                let new_version = latest_version.apply_upgrade(upgrade, Some(recursion_scheduler_level_vk_hash));

                storage
                    .protocol_versions_dal()
                    .save_protocol_version_with_tx(&new_version)
                    .await
                    .map_err(DalError::generalize)?;

                METRICS.inscriptions_processed[&InscriptionStage::Upgrade].set(new_version.version.minor as usize);
            }
        }
        Ok(None)
    }
}
