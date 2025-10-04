use std::str::FromStr;

use anyhow::Context;
use bitcoin::{
    hex::{Case, DisplayHex},
    Address as BitcoinAddress, Amount, Network,
};
use ethers::abi::{decode, ParamType};
use via_da_client::types::WITHDRAW_FUNC_SIG;
use via_verifier_types::withdrawal::WithdrawalRequest;
use zksync_basic_types::{web3::keccak256, U256};
use zksync_types::{api::Log, Address};

pub fn parse_l2_withdrawal_message(
    l2_to_l1_message: Vec<u8>,
    log: Log,
    network: Network,
) -> anyhow::Result<WithdrawalRequest> {
    let Some(l2_tx_hash) = log.transaction_hash else {
        anyhow::bail!("Tx hash not found for withdrawal");
    };

    let Some(l2_tx_log_index) = log.transaction_log_index else {
        anyhow::bail!("Tx log index not found for withdrawal");
    };

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
    if func_selector_bytes != _get_withdraw_function_selector() {
        return Err(anyhow::format_err!("Invalid message function selector."));
    }

    // The address bytes represent the l1 receiver
    let address_bytes = &l2_to_l1_message[4..4 + address_size];
    let address_str =
        String::from_utf8(address_bytes.to_vec()).with_context(|| "Parse address to string")?;
    let receiver = BitcoinAddress::from_str(&address_str)
        .with_context(|| "parse bitcoin address")?
        .require_network(network)?;

    // The last 32 bytes represent the amount (uint256)
    let amount_bytes = &l2_to_l1_message[address_size + 4..];
    let amount = Amount::from_sat(U256::from_big_endian(amount_bytes).as_u64());

    let l2_sender = Address::from_slice(&log.topics[1].as_bytes()[12..]);
    let tokens = decode(&[ParamType::Bytes, ParamType::Uint(256)], &log.data.0)?;

    let data_bytes: Vec<u8> = tokens[0]
        .clone()
        .into_bytes()
        .ok_or_else(|| anyhow::anyhow!("Failed to decode bytes from log"))?;

    let log_receiver_str =
        String::from_utf8(data_bytes).with_context(|| "Failed to parse bytes as UTF-8 string")?;

    let log_receiver = BitcoinAddress::from_str(&log_receiver_str)
        .with_context(|| "Failed to parse Bitcoin address string")?
        .require_network(network)
        .with_context(|| "Bitcoin address network mismatch")?;

    let log_amount = Amount::from_sat(
        tokens[1]
            .clone()
            .into_uint()
            .ok_or_else(|| anyhow::anyhow!("Could not parse the withdrawal amount from log"))?
            .as_u64()
            / 10_000_000_000, // Scale down to L1 BTC with 8 decimals
    );

    if log_receiver != receiver {
        anyhow::bail!(
            "Mismatch l1 receiver, log {:?}, pubdata {:?}",
            log_receiver,
            receiver
        )
    }

    if log_amount != amount {
        anyhow::bail!(
            "Mismatch amount, log {:?}, pubdata {:?}",
            log_amount,
            amount
        )
    }

    let mut id_bytes = Vec::with_capacity(10);
    id_bytes.extend_from_slice(&l2_tx_hash.0[..8]);

    let log_index_u16: u16 = l2_tx_log_index
        .as_u64()
        .try_into()
        .expect("l2_tx_log_index too large for u16");

    id_bytes.extend_from_slice(&log_index_u16.to_be_bytes());

    Ok(WithdrawalRequest {
        id: id_bytes.to_hex_string(Case::Lower),
        receiver,
        amount,
        l2_sender,
        l2_tx_hash: l2_tx_hash.to_string(),
        l2_tx_log_index: l2_tx_log_index.as_u64() as u16,
    })
}

/// Get the withdrawal function selector.
fn _get_withdraw_function_selector() -> Vec<u8> {
    let hash = keccak256(WITHDRAW_FUNC_SIG.as_bytes());
    hash[0..4].to_vec()
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use ethers::abi::{encode, Token};
    use zksync_types::{web3::Bytes, H160, H256, U64};

    use super::*;

    #[test]
    fn test_parse_l2_withdrawal_message_when_address_bech32m() {
        let btc_bytes = b"bc1qy82gaw2htfd5sslplpgmz4ktf9y3k7pac2226k0wljlmw3atfw5qwm4av4".to_vec();
        let amount = U256::from("0000000000000000000000000000000000000000000000000de0b6b3a7640000");
        let encoded_data = encode(&[Token::Bytes(btc_bytes.clone()), Token::Uint(amount.clone())]);
        let data = Bytes::from(encoded_data);

        let log = Log {
            block_timestamp: None,
            l1_batch_number: Some(U64::one()),
            address: H160::random(),
            topics: vec![
                H256::from_str(
                    "0x2d6ef0fc97a54b2a96a5f3c96e3e69dca5b8d5ef4f68f01472c9e7c2b8d1f17b",
                )
                .unwrap(),
                H256::from_str(
                    "0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .unwrap(),
            ],
            data,
            block_hash: None,
            block_number: Some(U64::one()),
            transaction_hash: Some(H256::zero()),
            transaction_index: None,
            log_index: Some(U256::zero()),
            transaction_log_index: Some(U256::zero()),
            log_type: None,
            removed: None,
        };

        // Example transaction: https://etherscan.io/tx/0x70afe07734e9b0c2d8393ab2a51fda5ac2cfccc80a01cc4a5cf587eaea3c4610
        let l2_to_l1_message = hex::decode("6c0960f96263317179383267617732687466643573736c706c70676d7a346b74663979336b37706163323232366b30776c6a6c6d7733617466773571776d346176340000000000000000000000000000000000000000000000000de0b6b3a7640000").unwrap();
        let expected_receiver = BitcoinAddress::from_str(
            "bc1qy82gaw2htfd5sslplpgmz4ktf9y3k7pac2226k0wljlmw3atfw5qwm4av4",
        )
        .unwrap()
        .assume_checked();
        let expected_amount = Amount::from_sat(1000000000000000000);
        let res = parse_l2_withdrawal_message(l2_to_l1_message, log, Network::Bitcoin).unwrap();

        assert_eq!(res.receiver, expected_receiver);
        assert_eq!(res.amount, expected_amount);
    }

    #[test]
    fn test_parse_l2_withdrawal_message_when_address_p2pkh() {
        let btc_bytes = b"1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".to_vec();
        let amount = U256::from("0000000000000000000000000000000000000000000000000de0b6b3a7640000");
        let encoded_data = encode(&[Token::Bytes(btc_bytes.clone()), Token::Uint(amount.clone())]);
        let data = Bytes::from(encoded_data);

        let log = Log {
            block_timestamp: None,
            l1_batch_number: Some(U64::one()),
            address: H160::random(),
            topics: vec![
                H256::from_str(
                    "0x2d6ef0fc97a54b2a96a5f3c96e3e69dca5b8d5ef4f68f01472c9e7c2b8d1f17b",
                )
                .unwrap(),
                H256::from_str(
                    "0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .unwrap(),
            ],
            data,
            block_hash: None,
            block_number: Some(U64::one()),
            transaction_hash: Some(H256::zero()),
            transaction_index: None,
            log_index: Some(U256::zero()),
            transaction_log_index: Some(U256::zero()),
            log_type: None,
            removed: None,
        };

        let l2_to_l1_message = hex::decode("6c0960f93141317a5031655035514765666932444d505466544c35534c6d7637446976664e610000000000000000000000000000000000000000000000000de0b6b3a7640000").unwrap();
        let expected_receiver = BitcoinAddress::from_str("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa")
            .unwrap()
            .assume_checked();
        let expected_amount = Amount::from_sat(1000000000000000000);
        let res = parse_l2_withdrawal_message(l2_to_l1_message, log, Network::Bitcoin).unwrap();

        assert_eq!(res.receiver, expected_receiver);
        assert_eq!(res.amount, expected_amount);
    }
}
