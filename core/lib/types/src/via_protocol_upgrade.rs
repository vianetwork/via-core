#![warn(clippy::cognitive_complexity)]

use zksync_basic_types::{
    ethabi::encode,
    protocol_version::{ProtocolSemanticVersion, VersionPatch},
    web3::keccak256,
    H256,
};
use zksync_system_constants::{CONTRACT_DEPLOYER_ADDRESS, CONTRACT_FORCE_DEPLOYER_ADDRESS};

use crate::{
    abi::L2CanonicalTransaction,
    ethabi::Token,
    helpers::unix_timestamp_ms,
    protocol_upgrade::{ProtocolUpgradeTx, ProtocolUpgradeTxCommonData},
    Address, Execute, ProtocolVersionId, PROTOCOL_UPGRADE_TX_TYPE, U256,
};

const GAS_LIMIT: u64 = 72_000_000;
const GAS_PER_PUB_DATA_BYTE_LIMIT: u64 = 800;
const LAST_LEGACY_CONTRACT_DECODER_VERSION: ProtocolSemanticVersion = ProtocolSemanticVersion {
    minor: ProtocolVersionId::Version28,
    patch: VersionPatch(0),
};

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
                contract_address: Some(CONTRACT_DEPLOYER_ADDRESS),
                calldata: self.get_calldata(version, system_contracts)?,
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
            data: self.get_calldata(version, system_contracts)?,
            signature: vec![],
            factory_deps: vec![],
            paymaster_input: vec![],
            reserved_dynamic: vec![],
        };

        Ok(l2_transaction.hash())
    }

    fn get_calldata(
        &self,
        version: ProtocolSemanticVersion,
        mut system_contracts: Vec<(Address, H256)>,
    ) -> anyhow::Result<Vec<u8>> {
        // Versions through 0.28.0 executed the legacy N-3 contract-tail grammar.
        // Reconstructed calldata must preserve that protocol boundary.
        if version <= LAST_LEGACY_CONTRACT_DECODER_VERSION {
            anyhow::ensure!(
                system_contracts.len() >= 3,
                "legacy protocol versions require at least three system contracts"
            );
            system_contracts.truncate(system_contracts.len() - 3);
        }
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

        let args = encode(&[Token::Array(encoded_deployments)]);

        // Function selector
        let selector =
            &keccak256(b"forceDeployOnAddresses((bytes32,address,bool,uint256,bytes)[])")[0..4];

        // Concatenate selector + encoded args
        let mut calldata = selector.to_vec();
        calldata.extend_from_slice(&args);

        Ok(calldata)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use zksync_basic_types::{Address, H256};
    use zksync_contracts::deployer_contract;

    use super::*;

    #[test]
    fn test_calldata_matches_contract_encoding() {
        let upgrader = ViaProtocolUpgrade {};

        let addr = Address::repeat_byte(0x11);
        let bytecode_hash = H256::repeat_byte(0xaa);

        // New way: manual encoding
        let new_calldata = upgrader
            .get_calldata(
                ProtocolSemanticVersion::new(ProtocolVersionId::Version28, VersionPatch(1)),
                vec![(addr, bytecode_hash)],
            )
            .expect("manual calldata");

        // Old way: via ABI contract binding
        let encoded_deployments = vec![Token::Tuple(vec![
            Token::FixedBytes(bytecode_hash.as_bytes().to_vec()),
            Token::Address(addr),
            Token::Bool(false),
            Token::Uint(U256::zero()),
            Token::Bytes(vec![]),
        ])];

        let old_calldata = deployer_contract()
            .function("forceDeployOnAddresses")
            .unwrap()
            .encode_input(&[Token::Array(encoded_deployments)])
            .unwrap();

        assert_eq!(
            new_calldata, old_calldata,
            "manual encoding does not match deployer_contract encoding"
        );
    }

    #[test]
    fn historical_contract_tail_matches_executed_upgrade_hash() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../etc/upgrades/1762959389-via-network/testnet/l2Upgrade.json"
        ))
        .unwrap();
        let contracts: Vec<_> = fixture["systemContracts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|contract| {
                (
                    Address::from_str(contract["address"].as_str().unwrap()).unwrap(),
                    H256::from_str(contract["bytecodeHashes"][0].as_str().unwrap()).unwrap(),
                )
            })
            .collect();
        assert_eq!(contracts.len(), 25);

        let upgrader = ViaProtocolUpgrade::default();
        assert!(upgrader
            .get_canonical_tx_hash(
                LAST_LEGACY_CONTRACT_DECODER_VERSION,
                contracts[..2].to_vec()
            )
            .is_err());
        let hash = |patch| {
            upgrader
                .get_canonical_tx_hash(
                    ProtocolSemanticVersion::new(ProtocolVersionId::Version28, VersionPatch(patch)),
                    contracts.clone(),
                )
                .unwrap()
        };
        assert_eq!(
            hash(0),
            H256::from_str("fedcd7e7d1764682b7c19f09d9fb89d59f3b3123bdfe97b674d7b1fc3f84449f")
                .unwrap()
        );
        assert_eq!(
            hash(1),
            H256::from_str("0f20d970d04f9b9166c3d4b7073fdae35ee9c1fbc253d0c16ee0a4b951f435b0")
                .unwrap()
        );
    }
}
