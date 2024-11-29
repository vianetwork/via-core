use std::str::FromStr;

use alloy_dyn_abi::DynSolValue;
use alloy_primitives::{self as primitives};
use anyhow::Context;
use bitcoin::{address::NetworkUnchecked, Address as BitcoinAddress};
use zksync_basic_types::{web3::keccak256, H160, H256, U256};

use crate::constant::{
    L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR, L2_L1_LOGS_TREE_DEFAULT_LEAF_HASH,
    L2_TO_L1_MESSENGER_SYSTEM_CONTRACT_ADDR, WITHDRAW_FUNC_SIG,
};

#[derive(Clone, Debug)]
pub struct WithdrawalRequest {
    /// The receiver l1 address.
    pub address: BitcoinAddress<NetworkUnchecked>,
    /// The amount user will receive.
    pub amount: U256,
}

#[derive(Clone, Debug, Default)]
pub struct VerifyParams {
    /// The L2 log root hash.
    pub root_hash: H256,
    /// The position in the L2 logs Merkle tree of the l2Log that was sent with the message.
    pub l2_message_index: u64,
    /// The L2 transaction number in the batch, in which the log was sent.
    pub l2_tx_number_in_batch: u64,
    /// The L2 withdraw data, stored in an L2 -> L1 message
    pub message: Vec<u8>,
    /// The Merkle proof of the inclusion L2 -> L1 message about withdrawal initialization
    pub merkel_proof_hashes: Vec<H256>,
}

/// The log passed from L2
#[derive(Clone, Debug, Default)]
struct L2Log {
    /// l2ShardId The shard identifier, 0 - rollup, 1 - porter
    /// All other values are not used but are reserved for the future
    l2_shard_id: u8,
    /// isService A boolean flag that is part of the log along with `key`, `value`, and `sender` address.
    /// This field is required formally but does not have any special meaning
    is_service: bool,
    /// txNumberInBatch The L2 transaction number in a Batch, in which the log was sent
    tx_number_in_batch: u64,
    /// sender The L2 address which sent the log
    sender: H160,
    /// key The 32 bytes of information that was sent in the log
    key: H256,
    /// value The 32 bytes of information that was sent in the log
    value: H256,
}

#[derive(Clone, Debug, Default)]
pub struct WithdrawalValidation {}

impl WithdrawalValidation {
    pub fn get_validated_withdrawal(
        &self,
        verify_params: VerifyParams,
    ) -> anyhow::Result<WithdrawalRequest> {
        if !self._prove_l2_message_inclusion(verify_params.clone()) {
            return Err(anyhow::format_err!(format!(
                "Withdrawal not included in the l1 batch {}",
                verify_params.l2_tx_number_in_batch
            )));
        }
        return self._parse_l2_withdrawal_message(verify_params.message);
    }

    fn _parse_l2_withdrawal_message(
        &self,
        l2_to_l1_message: Vec<u8>,
    ) -> anyhow::Result<WithdrawalRequest, anyhow::Error> {
        // We check that the message is long enough to read the data.
        // Please note that there are two versions of the message:
        // The message that is sent by `withdraw(address _l1Receiver)`
        // It should be equal to the length of the bytes4 function signature + bytes l1Receiver + uint256 amount = 4 + X + 32.
        let message_len = l2_to_l1_message.len();
        let address_size = message_len - 36;
        if message_len <= 36 {
            return Err(anyhow::format_err!("Invalid message length."));
        }

        let func_selector_bytes = &l2_to_l1_message[0..4];
        if func_selector_bytes != self._get_withdraw_function_selector() {
            return Err(anyhow::format_err!("Invalid message function selector."));
        }

        // The address bytes represent the l1 receiver
        let address_bytes = &l2_to_l1_message[4..4 + address_size];
        let address_str =
            String::from_utf8(address_bytes.to_vec()).context("Parse address to string")?;
        let address = BitcoinAddress::from_str(&address_str).context("parse bitcoin address")?;

        // The last 32 bytes represent the amount (uint256)
        let amount_bytes = &l2_to_l1_message[address_size + 4..];
        let amount = U256::from_big_endian(amount_bytes);

        return Ok(WithdrawalRequest { address, amount });
    }

    /// Prove that a specific L2 log was sent in a specific L2 batch number.
    fn _prove_l2_message_inclusion(&self, verify_params: VerifyParams) -> bool {
        let hashed_log: H256 = self._calculate_log_hash(
            verify_params.message.clone(),
            verify_params.l2_tx_number_in_batch,
        );

        // Check that hashed log is not the default one,
        // otherwise it means that the value is out of range of sent L2 -> L1 logs
        if H256::from_slice(L2_L1_LOGS_TREE_DEFAULT_LEAF_HASH.as_bytes()) == hashed_log {
            return false;
        }

        let calculated_root_hash = self._calculate_root(
            verify_params.merkel_proof_hashes,
            verify_params.l2_message_index,
            hashed_log,
        );
        calculated_root_hash == verify_params.root_hash
    }

    /// Calculate the log hash.
    fn _calculate_log_hash(&self, message: Vec<u8>, l2_tx_number_in_batch: u64) -> H256 {
        let log = L2Log {
            l2_shard_id: 0,
            is_service: true,
            tx_number_in_batch: l2_tx_number_in_batch,
            sender: H160::from_str(L2_TO_L1_MESSENGER_SYSTEM_CONTRACT_ADDR).unwrap(),
            key: H256::from(H160::from_str(L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR).unwrap()),
            value: H256::from(keccak256(hex::encode(&message).as_bytes())),
        };

        let dt = DynSolValue::Tuple(vec![
            DynSolValue::Uint(primitives::U256::from(log.l2_shard_id), 256),
            DynSolValue::Bool(log.is_service),
            DynSolValue::Uint(primitives::U256::from(log.tx_number_in_batch), 16),
            DynSolValue::Address(primitives::Address::from_slice(log.sender.as_bytes())),
            DynSolValue::FixedBytes(
                primitives::FixedBytes::from_slice(&log.key.as_bytes().to_vec()),
                32,
            ),
            DynSolValue::FixedBytes(
                primitives::FixedBytes::from_slice(&log.value.as_bytes().to_vec()),
                32,
            ),
        ]);

        H256::from(keccak256(&dt.abi_encode_packed()))
    }

    /// Get the withdrawal function selector.
    fn _get_withdraw_function_selector(&self) -> Vec<u8> {
        let hash = keccak256(WITHDRAW_FUNC_SIG.as_bytes());
        hash[0..4].to_vec()
    }

    /// Calculate the merkel root hash from list proof.
    fn _calculate_root(&self, path: Vec<H256>, index: u64, item_hash: H256) -> H256 {
        if path.len() == 0 || path.len() >= 256 {
            H256::zero();
        }

        if index > (1 << path.len()) {
            H256::zero();
        }

        let mut current_index = index;
        let mut current_hash: H256 = item_hash;
        for h in path {
            if current_index % 2 == 0 {
                current_hash = self._hash(current_hash, h);
            } else {
                current_hash = self._hash(h, current_hash);
            }
            current_index /= 2;
        }
        current_hash
    }

    fn _hash(&self, lhs: H256, rhs: H256) -> H256 {
        let mut input = Vec::with_capacity(64);
        input.extend_from_slice(&lhs.0);
        input.extend_from_slice(&rhs.0);

        H256::from(keccak256(&input))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn test_parse_l2_withdrawal_message_when_address_bech32m() {
        let withdrawal_validation = WithdrawalValidation::default();
        // Example transaction: https://etherscan.io/tx/0x70afe07734e9b0c2d8393ab2a51fda5ac2cfccc80a01cc4a5cf587eaea3c4610
        let l2_to_l1_message = hex::decode("6c0960f96263317179383267617732687466643573736c706c70676d7a346b74663979336b37706163323232366b30776c6a6c6d7733617466773571776d346176340000000000000000000000000000000000000000000000000de0b6b3a7640000").unwrap();
        let expected_receiver = BitcoinAddress::from_str(
            &"bc1qy82gaw2htfd5sslplpgmz4ktf9y3k7pac2226k0wljlmw3atfw5qwm4av4",
        )
        .unwrap();
        let expected_amount = U256::from_dec_str("1000000000000000000").unwrap();
        let res = withdrawal_validation
            ._parse_l2_withdrawal_message(l2_to_l1_message)
            .unwrap();

        assert_eq!(res.address, expected_receiver);
        assert_eq!(res.amount, expected_amount);
    }

    #[test]
    fn test_parse_l2_withdrawal_message_when_address_p2pkh() {
        let withdrawal_validation = WithdrawalValidation::default();
        let l2_to_l1_message = hex::decode("6c0960f93141317a5031655035514765666932444d505466544c35534c6d7637446976664e610000000000000000000000000000000000000000000000000de0b6b3a7640000").unwrap();
        let expected_receiver =
            BitcoinAddress::from_str(&"1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa").unwrap();
        let expected_amount = U256::from_dec_str("1000000000000000000").unwrap();
        let res = withdrawal_validation
            ._parse_l2_withdrawal_message(l2_to_l1_message)
            .unwrap();

        assert_eq!(res.address, expected_receiver);
        assert_eq!(res.amount, expected_amount);
    }

    #[test]
    fn test_calculate_log_hash() {
        let withdrawal_validation = WithdrawalValidation::default();
        let l2_to_l1_message = hex::decode("6c0960f96263317179383267617732687466643573736c706c70676d7a346b74663979336b37706163323232366b30776c6a6c6d7733617466773571776d346176340000000000000000000000000000000000000000000000000de0b6b3a7640000").unwrap();
        let l2_tx_number_in_batch = 4488;
        let expected_hash =
            H256::from_str("0x0e356785e0d96b689e2f2cd079eab0938e88e9171e7df38ba509e37a5c12ee9e")
                .unwrap();

        let log_hash =
            withdrawal_validation._calculate_log_hash(l2_to_l1_message, l2_tx_number_in_batch);
        assert_eq!(log_hash, expected_hash);
    }

    #[test]
    fn test_calculate_root() {
        let withdrawal_validation = WithdrawalValidation::default();
        let l2_to_l1_message = hex::decode("6c0960f96263317179383267617732687466643573736c706c70676d7a346b74663979336b37706163323232366b30776c6a6c6d7733617466773571776d346176340000000000000000000000000000000000000000000000000de0b6b3a7640000").unwrap();
        let l2_tx_number_in_batch = 4488;
        let expected_hash =
            H256::from_str("0x0e356785e0d96b689e2f2cd079eab0938e88e9171e7df38ba509e37a5c12ee9e")
                .unwrap();

        let log_hash =
            withdrawal_validation._calculate_log_hash(l2_to_l1_message, l2_tx_number_in_batch);
        assert_eq!(log_hash, expected_hash);

        let merkel_path: Vec<H256> = vec![
            H256::from_str("0xda074edf6a2c8b4d86c7025a901b3117d97697697179c123d2a6db01cac6ae67")
                .unwrap(),
            H256::from_str("0xc3d03eebfd83049991ea3d3e358b6712e7aa2e2e63dc2d4b438987cec28ac8d0")
                .unwrap(),
            H256::from_str("0xc9333072d1a90d08acffe3e233bfb524fd0287fbad2eb7c760bc9d435fb43bba")
                .unwrap(),
            H256::from_str("0x199cc5812543ddceeddd0fc82807646a4899444240db2c0d2f20c3cceb5f51fa")
                .unwrap(),
            H256::from_str("0xe4733f281f18ba3ea8775dd62d2fcd84011c8c938f16ea5790fd29a03bf8db89")
                .unwrap(),
            H256::from_str("0x1798a1fd9c8fbb818c98cff190daa7cc10b6e5ac9716b4a2649f7c2ebcef2272")
                .unwrap(),
            H256::from_str("0x66d7c5983afe44cf15ea8cf565b34c6c31ff0cb4dd744524f7842b942d08770d")
                .unwrap(),
            H256::from_str("0xb04e5ee349086985f74b73971ce9dfe76bbed95c84906c5dffd96504e1e5396c")
                .unwrap(),
            H256::from_str("0xac506ecb5465659b3a927143f6d724f91d8d9c4bdb2463aee111d9aa869874db")
                .unwrap(),
            H256::from_str("0x124b05ec272cecd7538fdafe53b6628d31188ffb6f345139aac3c3c1fd2e470f")
                .unwrap(),
            H256::from_str("0xc3be9cbd19304d84cca3d045e06b8db3acd68c304fc9cd4cbffe6d18036cb13f")
                .unwrap(),
            H256::from_str("0xfef7bd9f889811e59e4076a0174087135f080177302763019adaf531257e3a87")
                .unwrap(),
            H256::from_str("0xa707d1c62d8be699d34cb74804fdd7b4c568b6c1a821066f126c680d4b83e00b")
                .unwrap(),
            H256::from_str("0xf6e093070e0389d2e529d60fadb855fdded54976ec50ac709e3a36ceaa64c291")
                .unwrap(),
        ];

        let root_hash_expected =
            H256::from_str("0x864bf9fe8f4b89fb80ba8cccf148204e917892af2933d332902546fee0e6a068")
                .unwrap();

        let root_hash = withdrawal_validation._calculate_root(merkel_path, 5, log_hash);
        assert_eq!(root_hash, root_hash_expected);
    }
}
