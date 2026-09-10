use std::sync::Arc;

use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, MessageParser},
    traits::BitcoinOps,
    types::FullInscriptionMessage,
};
use via_verifier_dal::{Connection, DalError, Verifier, VerifierDal};
use via_verifier_types::protocol_version::get_sequencer_version;
use zksync_types::via_protocol_upgrade::ViaProtocolUpgrade;

use crate::{
    message_processors::{load_tx_locator, MessageProcessor, MessageProcessorError},
    metrics::{InscriptionStage, METRICS},
};

/// Listens to operation events coming from the governance contract and saves new protocol upgrade proposals to the database.
#[derive(Debug)]
pub struct GovernanceUpgradesEventProcessor {
    /// BTC client
    btc_client: Arc<BitcoinClient>,
    /// Message parser
    message_parser: MessageParser,
    /// upgrade proposal
    upgrade: ViaProtocolUpgrade,
}

impl GovernanceUpgradesEventProcessor {
    pub fn new(btc_client: Arc<BitcoinClient>) -> Self {
        let message_parser = MessageParser::new(btc_client.get_network());
        Self { btc_client, message_parser, upgrade: ViaProtocolUpgrade::default() }
    }
}

#[async_trait::async_trait]
impl MessageProcessor for GovernanceUpgradesEventProcessor {
    async fn process_messages(
        &mut self, storage: &mut Connection<'_, Verifier>, msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        let mut upgrades = Vec::new();
        for msg in msgs {
            if let FullInscriptionMessage::SystemContractUpgrade(system_contract_upgrade_msg) = &msg {
                let Some(proposal_locator) =
                    load_tx_locator(storage, &system_contract_upgrade_msg.input.proposal_tx_id).await?
                else {
                    tracing::warn!(
                        "Skipping protocol upgrade, missing Bitcoin locator for proposal txid {}",
                        system_contract_upgrade_msg.input.proposal_tx_id
                    );
                    continue;
                };

                let proposal_tx = self
                    .btc_client
                    .get_transaction_in_block(
                        &system_contract_upgrade_msg.input.proposal_tx_id,
                        &proposal_locator.block_hash,
                    )
                    .await
                    .map_err(|err| {
                        MessageProcessorError::Internal(anyhow::anyhow!(
                            "Failed to fetch protocol upgrade transaction: {}, error {}",
                            system_contract_upgrade_msg.input.proposal_tx_id,
                            err
                        ))
                    })?;

                let messages = self.message_parser.parse_system_transaction_with_block_hash(
                    &proposal_tx,
                    proposal_locator.block_height,
                    Some(proposal_locator.block_hash),
                    None,
                );

                for message in messages {
                    match message {
                        FullInscriptionMessage::SystemContractUpgradeProposal(system_contract_upgrade_proposal_msg) => {
                            if system_contract_upgrade_proposal_msg.input.version < get_sequencer_version() {
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

                            let hash = self.upgrade.get_canonical_tx_hash(
                                system_contract_upgrade_proposal_msg.input.version,
                                system_contract_upgrade_proposal_msg.input.system_contracts,
                            )?;

                            let upgrade = (
                                system_contract_upgrade_proposal_msg.input.version,
                                system_contract_upgrade_proposal_msg.input.bootloader_code_hash,
                                system_contract_upgrade_proposal_msg.input.default_account_code_hash,
                                hash,
                                system_contract_upgrade_proposal_msg.input.recursion_scheduler_level_vk_hash,
                            );

                            upgrades.push(upgrade);
                        }
                        _ => (),
                    }
                }
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
