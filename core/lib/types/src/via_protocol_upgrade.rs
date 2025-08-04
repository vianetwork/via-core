use zksync_basic_types::{protocol_version::ProtocolSemanticVersion, H256};
use zksync_contracts::deployer_contract;
use zksync_system_constants::{CONTRACT_DEPLOYER_ADDRESS, CONTRACT_FORCE_DEPLOYER_ADDRESS};

use crate::{
    abi::L2CanonicalTransaction,
    ethabi::Token,
    helpers::unix_timestamp_ms,
    protocol_upgrade::{ProtocolUpgradeTx, ProtocolUpgradeTxCommonData},
    Address, Execute, PROTOCOL_UPGRADE_TX_TYPE, U256,
};

const GAS_LIMIT: u64 = 72_000_000;
const GAS_PER_PUB_DATA_BYTE_LIMIT: u64 = 800;

#[derive(Debug, Clone, Default)]
pub struct ViaProtocolUpgrade {}

impl ViaProtocolUpgrade {
    pub fn create_protocol_upgrade_tx(
        &self,
        version: ProtocolSemanticVersion,
        system_contracts: Vec<(Address, H256)>,
    ) -> anyhow::Result<ProtocolUpgradeTx> {
        let canonical_tx_hash = self.get_canonical_tx_hash(version, system_contracts.clone())?;

        let tx = ProtocolUpgradeTx {
            execute: Execute {
                contract_address: CONTRACT_DEPLOYER_ADDRESS,
                calldata: self.get_calldata(system_contracts)?,
                value: U256::zero(),
                factory_deps: vec![],
            },
            common_data: ProtocolUpgradeTxCommonData {
                sender: CONTRACT_FORCE_DEPLOYER_ADDRESS,
                upgrade_id: version.minor,
                max_fee_per_gas: U256::zero(),
                gas_limit: U256::from(GAS_LIMIT),
                gas_per_pubdata_limit: U256::from(GAS_PER_PUB_DATA_BYTE_LIMIT),
                eth_block: 0,
                canonical_tx_hash,
                to_mint: U256::zero(),
                refund_recipient: Address::zero(),
            },
            received_timestamp_ms: unix_timestamp_ms(),
        };

        Ok(tx)
    }

    pub fn get_canonical_tx_hash(
        &self,
        version: ProtocolSemanticVersion,
        system_contracts: Vec<(Address, H256)>,
    ) -> anyhow::Result<H256> {
        let l2_transaction = L2CanonicalTransaction {
            tx_type: PROTOCOL_UPGRADE_TX_TYPE.into(),
            from: U256::from_big_endian(&CONTRACT_FORCE_DEPLOYER_ADDRESS.0),
            to: U256::from_big_endian(&CONTRACT_DEPLOYER_ADDRESS.0),
            gas_limit: U256::from(GAS_LIMIT),
            gas_per_pubdata_byte_limit: U256::from(GAS_PER_PUB_DATA_BYTE_LIMIT),
            max_fee_per_gas: U256::zero(),
            max_priority_fee_per_gas: U256::zero(),
            paymaster: U256::zero(),
            nonce: U256::from(version.minor as u64),
            value: U256::zero(),
            reserved: [U256::zero(), U256::zero(), U256::zero(), U256::zero()],
            data: self.get_calldata(system_contracts.clone())?,
            signature: vec![],
            factory_deps: vec![],
            paymaster_input: vec![],
            reserved_dynamic: vec![],
        };

        Ok(l2_transaction.hash())
    }

    fn get_calldata(&self, system_contracts: Vec<(Address, H256)>) -> anyhow::Result<Vec<u8>> {
        let encoded_deployments: Vec<_> = system_contracts
            .into_iter()
            .map(|(address, bytecode_hash)| {
                Token::Tuple(vec![
                    Token::FixedBytes(bytecode_hash.as_bytes().to_vec()),
                    Token::Address(address),
                    Token::Bool(false),
                    Token::Uint(U256::zero()),
                    Token::Bytes(vec![]),
                ])
            })
            .collect();

        Ok(deployer_contract()
            .function("forceDeployOnAddresses")?
            .encode_input(&[Token::Array(encoded_deployments)])?)
    }
}
