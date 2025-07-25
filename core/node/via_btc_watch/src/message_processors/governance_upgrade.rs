use std::sync::Arc;

use anyhow::Context as _;
use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, MessageParser},
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
    /// BTC client
    btc_client: Arc<BitcoinClient>,
    /// Message parser
    message_parser: MessageParser,
    /// upgrade proposal
    upgrade: ViaProtocolUpgrade,
}

impl GovernanceUpgradesEventProcessor {
    pub fn new(
        btc_client: Arc<BitcoinClient>,
        last_seen_protocol_version: ProtocolSemanticVersion,
    ) -> Self {
        let message_parser = MessageParser::new(btc_client.get_network());
        Self {
            last_seen_protocol_version,
            btc_client,
            message_parser,
            upgrade: ViaProtocolUpgrade::new(),
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
    ) -> Result<(), MessageProcessorError> {
        let mut upgrades = Vec::new();
        for msg in msgs {
            if let FullInscriptionMessage::SystemContractUpgrade(system_contract_upgrade_msg) = &msg
            {
                let proposal_tx = self
                    .btc_client
                    .get_transaction(&system_contract_upgrade_msg.input.proposal_tx_id)
                    .await
                    .map_err(|err| {
                        MessageProcessorError::Internal(anyhow::anyhow!(
                            "Failed to fetch protocol upgrade transaction: {}, error {}",
                            system_contract_upgrade_msg.input.proposal_tx_id,
                            err
                        ))
                    })?;

                let messages = self.message_parser.parse_system_transaction(
                    &proposal_tx,
                    system_contract_upgrade_msg.common.block_height,
                );

                for message in messages {
                    match message {
                        FullInscriptionMessage::SystemContractUpgradeProposal(
                            system_contract_upgrade_proposal_msg,
                        ) => {
                            // Ignore if old version
                            if system_contract_upgrade_proposal_msg.input.version
                                <= self.last_seen_protocol_version
                            {
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
                            let tx = self.upgrade.create_protocol_upgrade_tx(
                                system_contract_upgrade_proposal_msg.input.version,
                                system_contract_upgrade_proposal_msg.input.system_contracts,
                            )?;

                            let upgrade = ProtocolUpgrade {
                                version: system_contract_upgrade_proposal_msg.input.version,
                                bootloader_code_hash: Some(
                                    system_contract_upgrade_proposal_msg
                                        .input
                                        .bootloader_code_hash,
                                ),
                                default_account_code_hash: Some(
                                    system_contract_upgrade_proposal_msg
                                        .input
                                        .default_account_code_hash,
                                ),
                                tx: Some(tx),
                                timestamp: 0,
                                verifier_address: None,
                                verifier_params: None,
                            };
                            upgrades.push((
                                upgrade,
                                system_contract_upgrade_proposal_msg
                                    .input
                                    .recursion_scheduler_level_vk_hash,
                            ));
                        }
                        _ => (),
                    }
                }
            }
        }

        let Some(last_upgrade) = upgrades.last() else {
            return Ok(());
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
                // if new_version.version.minor == latest_semantic_version.minor {
                //     // Only verification parameters may change if only patch is bumped.
                //     assert_eq!(
                //         new_version.base_system_contracts_hashes,
                //         latest_version.base_system_contracts_hashes
                //     );
                //     assert!(new_version.tx.is_none());
                // }
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

        Ok(())
    }
}

// impl GovernanceUpgradesEventProcessor {
//     fn create_l1_tx_from_message(
//         &self,
//         msg: &SystemContractUpgradeProposal,
//     ) -> Result<ProtocolUpgradeTx, MessageProcessorError> {
//         let l2_transaction = self
//             .upgrade
//             .get_canonical_tx_hash(msg.input.version, msg.input.system_contracts.clone())?;

//         let tx = ProtocolUpgradeTx {
//             execute: Execute {
//                 contract_address: CONTRACT_DEPLOYER_ADDRESS,
//                 calldata: calldata.clone(),
//                 value: U256::zero(),
//                 factory_deps: vec![],
//             },
//             common_data: ProtocolUpgradeTxCommonData {
//                 sender: CONTRACT_FORCE_DEPLOYER_ADDRESS,
//                 upgrade_id: msg.input.version.minor,
//                 max_fee_per_gas: U256::zero(),
//                 gas_limit,
//                 gas_per_pubdata_limit: gas_per_pubdata_byte_limit,
//                 eth_block: 0,
//                 canonical_tx_hash: l2_transaction.hash(),
//                 to_mint: U256::zero(),
//                 refund_recipient: Address::zero(),
//             },
//             received_timestamp_ms: unix_timestamp_ms(),
//         };

//         Ok(tx)
//     }
// }

// fn address_to_u256(address: &Address) -> U256 {
//     U256::from_big_endian(&address.0)
// }
