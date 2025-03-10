use anyhow::Context as _;
use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{
        BitcoinAddress, FullInscriptionMessage, L1ToL2Message, SystemContractUpgrade,
        SystemContractUpgradeInput,
    },
};
use zksync_dal::{Connection, Core, CoreDal, DalError};
use zksync_types::{
    abi::L2CanonicalTransaction,
    helpers::unix_timestamp_ms,
    l1::{L1Tx, OpProcessingType, PriorityQueueType},
    protocol_upgrade::{ProtocolUpgradeTx, ProtocolUpgradeTxCommonData},
    protocol_version::ProtocolSemanticVersion,
    Address, Execute, L1BatchNumber, L1TxCommonData, PriorityOpId, ProtocolUpgrade,
    ProtocolVersionId, COMPLEX_UPGRADER_ADDRESS, CONTRACT_FORCE_DEPLOYER_ADDRESS, H256,
    PROTOCOL_UPGRADE_TX_TYPE, U256,
};

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::{ErrorType, InscriptionStage, METRICS},
};

/// Listens to operation events coming from the governance contract and saves new protocol upgrade proposals to the database.
#[derive(Debug)]
pub struct GovernanceUpgradesEventProcessor {
    gov_address: BitcoinAddress,
    /// Last protocol version seen. Used to skip events for already known upgrade proposals.
    last_seen_protocol_version: ProtocolSemanticVersion,
}

impl GovernanceUpgradesEventProcessor {
    pub fn new(
        gov_address: BitcoinAddress,
        last_seen_protocol_version: ProtocolSemanticVersion,
    ) -> Self {
        Self {
            gov_address,
            last_seen_protocol_version,
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
                // Todo: support Multisig
                if let Some(signer_address) =
                    system_contract_upgrade_msg.common.p2wpkh_address.clone()
                {
                    if signer_address == self.gov_address {
                        let mut tx_id_bytes =
                            system_contract_upgrade_msg.common.tx_id.as_raw_hash()[..].to_vec();
                        tx_id_bytes.reverse();
                        let tx_id = H256::from_slice(&tx_id_bytes);

                        if system_contract_upgrade_msg.input.version
                            <= self.last_seen_protocol_version
                        {
                            tracing::debug!(
                                "Upgrade transaction with version {} already processed, skipping",
                                system_contract_upgrade_msg.input.version
                            );
                            continue;
                        }

                        tracing::debug!(
                            "Received upgrades with versions: {:?}",
                            system_contract_upgrade_msg.input.version
                        );
                        let tx = self.create_l1_tx_from_message(system_contract_upgrade_msg)?;

                        let upgrade = ProtocolUpgrade {
                            version: system_contract_upgrade_msg.input.version,
                            bootloader_code_hash: Some(
                                system_contract_upgrade_msg.input.bootloader_code_hash,
                            ),
                            default_account_code_hash: Some(
                                system_contract_upgrade_msg.input.default_account_code_hash,
                            ),
                            timestamp: system_contract_upgrade_msg.input.execution_timestamp,
                            tx: Some(tx),
                            verifier_address: None,
                            verifier_params: None,
                        };
                        upgrades.push(upgrade);
                    }
                }
            }
        }

        let Some(last_upgrade) = upgrades.last() else {
            return Ok(());
        };

        let last_version = last_upgrade.version;
        for upgrade in upgrades {
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

                let new_version = latest_version.apply_upgrade(upgrade, None);
                if new_version.version.minor == latest_semantic_version.minor {
                    // Only verification parameters may change if only patch is bumped.
                    assert_eq!(
                        new_version.base_system_contracts_hashes,
                        latest_version.base_system_contracts_hashes
                    );
                    assert!(new_version.tx.is_none());
                }
                storage
                    .protocol_versions_dal()
                    .save_protocol_version_with_tx(&new_version)
                    .await
                    .map_err(DalError::generalize)?;
            }
        }
        self.last_seen_protocol_version = last_version;

        Ok(())
    }
}

impl GovernanceUpgradesEventProcessor {
    fn create_l1_tx_from_message(
        &self,
        msg: &SystemContractUpgrade,
    ) -> Result<ProtocolUpgradeTx, MessageProcessorError> {
        let max_fee_per_gas = U256::from(100_000_000u64);
        let gas_limit = U256::from(72_000_000u64);
        let gas_per_pubdata_byte_limit = U256::from(800u64);

        // https://github.com/vianetwork/via-core/blob/bf5e6ed5a1d9a60fcc1783c202c939c20f237d94/core/lib/types/src/lib.rs#L256
        let l2_transaction = L2CanonicalTransaction {
            tx_type: PROTOCOL_UPGRADE_TX_TYPE.into(),
            from: address_to_u256(&CONTRACT_FORCE_DEPLOYER_ADDRESS),
            to: address_to_u256(&COMPLEX_UPGRADER_ADDRESS),
            gas_limit,
            gas_per_pubdata_byte_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas: U256::zero(),
            paymaster: U256::zero(),
            nonce: U256::zero(),
            value: U256::zero(),
            reserved: [
                U256::zero(),
                address_to_u256(&msg.input.refund_recipient),
                U256::zero(),
                U256::zero(),
            ],
            data: msg.input.calldata.clone(),
            signature: vec![],
            factory_deps: vec![],
            paymaster_input: vec![],
            reserved_dynamic: vec![],
        };

        let tx = ProtocolUpgradeTx {
            execute: Execute {
                contract_address: Address::zero(),
                calldata: msg.input.calldata.clone(),
                value: U256::zero(),
                factory_deps: vec![],
            },
            common_data: ProtocolUpgradeTxCommonData {
                sender: Address::zero(),
                upgrade_id: msg.input.version.minor,
                max_fee_per_gas,
                gas_limit,
                gas_per_pubdata_limit: gas_per_pubdata_byte_limit,
                eth_block: 0,
                canonical_tx_hash: l2_transaction.hash(),
                to_mint: U256::zero(),
                refund_recipient: msg.input.refund_recipient,
            },
            received_timestamp_ms: unix_timestamp_ms(),
        };

        Ok(tx)
    }
}

fn address_to_u256(address: &Address) -> U256 {
    U256::from_big_endian(&address.0)
}
