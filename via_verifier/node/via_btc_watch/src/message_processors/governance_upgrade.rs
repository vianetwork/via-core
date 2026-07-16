use std::sync::Arc;

use via_btc_client::{
    indexer::{BitcoinInscriptionIndexer, MessageParser},
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
    /// BTC client
    btc_client: Arc<dyn BitcoinOps>,
    /// Message parser
    message_parser: MessageParser,
    /// upgrade proposal
    upgrade: ViaProtocolUpgrade,
}

impl GovernanceUpgradesEventProcessor {
    pub fn new(btc_client: Arc<dyn BitcoinOps>) -> Self {
        let message_parser = MessageParser::new(btc_client.get_network());
        Self {
            btc_client,
            message_parser,
            upgrade: ViaProtocolUpgrade::default(),
        }
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
            let proposal_tx = self
                .btc_client
                .get_transaction(&activation.input.proposal_tx_id)
                .await
                .map_err(|err| {
                    MessageProcessorError::Internal(anyhow::anyhow!(
                        "Failed to fetch protocol upgrade transaction: {}, error {}",
                        activation.input.proposal_tx_id,
                        err
                    ))
                })?;

            let messages = self.message_parser.parse_system_transaction(
                &proposal_tx,
                activation.common.block_height,
                None,
            );

            for message in messages {
                let FullInscriptionMessage::SystemContractUpgradeProposal(proposal) = message
                else {
                    continue;
                };
                let input = proposal.input;
                if input.version < get_sequencer_version() {
                    tracing::info!(
                        "Upgrade transaction with version {} already processed, skipping",
                        input.version
                    );
                    continue;
                }

                tracing::info!("Received upgrades with versions: {:?}", input.version);

                let hash = self
                    .upgrade
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
