use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{FullInscriptionMessage, SystemContractUpgrade},
};
use via_verifier_dal::{Connection, DalError, Verifier, VerifierDal};
use via_verifier_types::protocol_version::get_sequencer_version;
use zksync_types::{
    abi::L2CanonicalTransaction, via_protocol_upgrade::get_calldata, CONTRACT_DEPLOYER_ADDRESS,
    CONTRACT_FORCE_DEPLOYER_ADDRESS, H256, PROTOCOL_UPGRADE_TX_TYPE, U256,
};

use crate::message_processors::{MessageProcessor, MessageProcessorError};

/// Listens to operation events coming from the governance contract and saves new protocol upgrade proposals to the database.
#[derive(Debug)]
pub struct GovernanceUpgradesEventProcessor {}

impl GovernanceUpgradesEventProcessor {
    pub fn new() -> Self {
        Self {}
    }
}
#[async_trait::async_trait]
impl MessageProcessor for GovernanceUpgradesEventProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<(), MessageProcessorError> {
        let mut upgrades = Vec::new();
        for msg in msgs {
            if let FullInscriptionMessage::SystemContractUpgrade(system_contract_upgrade_msg) = &msg
            {
                // Ignore if old version but not if same version. The verifier should refresh the protocol version when the
                // in case where the Sequencer revert an upgrade.
                if system_contract_upgrade_msg.input.version < get_sequencer_version() {
                    tracing::debug!(
                        "Upgrade transaction with version {} already processed, skipping",
                        system_contract_upgrade_msg.input.version
                    );
                    continue;
                }

                tracing::info!(
                    "Received upgrades with versions: {:?}",
                    system_contract_upgrade_msg.input.version
                );
                let hash = self.get_canonical_tx_hash(system_contract_upgrade_msg)?;

                let upgrade = (
                    system_contract_upgrade_msg.input.version,
                    system_contract_upgrade_msg.input.bootloader_code_hash,
                    system_contract_upgrade_msg.input.default_account_code_hash,
                    hash,
                    system_contract_upgrade_msg
                        .input
                        .recursion_scheduler_level_vk_hash,
                );

                upgrades.push(upgrade);
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
        Ok(())
    }
}

impl GovernanceUpgradesEventProcessor {
    fn get_canonical_tx_hash(
        &self,
        msg: &SystemContractUpgrade,
    ) -> Result<H256, MessageProcessorError> {
        let gas_limit = U256::from(72_000_000u64);
        let gas_per_pubdata_byte_limit = U256::from(800u64);
        let calldata = get_calldata(msg.input.system_contracts.clone())?;

        let l2_transaction = L2CanonicalTransaction {
            tx_type: PROTOCOL_UPGRADE_TX_TYPE.into(),
            from: U256::from_big_endian(&CONTRACT_FORCE_DEPLOYER_ADDRESS.0),
            to: U256::from_big_endian(&CONTRACT_DEPLOYER_ADDRESS.0),
            gas_limit,
            gas_per_pubdata_byte_limit,
            max_fee_per_gas: U256::zero(),
            max_priority_fee_per_gas: U256::zero(),
            paymaster: U256::zero(),
            nonce: U256::from(msg.input.version.minor as u64),
            value: U256::zero(),
            reserved: [U256::zero(), U256::zero(), U256::zero(), U256::zero()],
            data: calldata.clone(),
            signature: vec![],
            factory_deps: vec![],
            paymaster_input: vec![],
            reserved_dynamic: vec![],
        };

        Ok(l2_transaction.hash())
    }
}
