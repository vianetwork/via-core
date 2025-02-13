use std::str::FromStr;

use anyhow::Context;
use bitcoin::{Address as BitcoinAddress, Amount, Network};
use via_da_client::types::WITHDRAW_FUNC_SIG;
use via_verifier_types::withdrawal::WithdrawalRequest;
use zksync_basic_types::{web3::keccak256, U256};

pub fn parse_l2_withdrawal_message(
    l2_to_l1_message: Vec<u8>,
    network: Network,
) -> anyhow::Result<WithdrawalRequest> {
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
        String::from_utf8(address_bytes.to_vec()).context("Parse address to string")?;
    let address = BitcoinAddress::from_str(&address_str)
        .context("parse bitcoin address")?
        .require_network(network)?;

    // The last 32 bytes represent the amount (uint256)
    let amount_bytes = &l2_to_l1_message[address_size + 4..];
    let amount = Amount::from_sat(U256::from_big_endian(amount_bytes).as_u64());

    Ok(WithdrawalRequest { address, amount })
}

/// Get the withdrawal function selector.
fn _get_withdraw_function_selector() -> Vec<u8> {
    let hash = keccak256(WITHDRAW_FUNC_SIG.as_bytes());
    hash[0..4].to_vec()
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn test_parse_l2_withdrawal_message_when_address_bech32m() {
        // Example transaction: https://etherscan.io/tx/0x70afe07734e9b0c2d8393ab2a51fda5ac2cfccc80a01cc4a5cf587eaea3c4610
        let l2_to_l1_message = hex::decode("6c0960f96263317179383267617732687466643573736c706c70676d7a346b74663979336b37706163323232366b30776c6a6c6d7733617466773571776d346176340000000000000000000000000000000000000000000000000de0b6b3a7640000").unwrap();
        let expected_receiver = BitcoinAddress::from_str(
            "bc1qy82gaw2htfd5sslplpgmz4ktf9y3k7pac2226k0wljlmw3atfw5qwm4av4",
        )
        .unwrap()
        .assume_checked();
        let expected_amount = Amount::from_sat(1000000000000000000);
        let res = parse_l2_withdrawal_message(l2_to_l1_message, Network::Bitcoin).unwrap();

        assert_eq!(res.address, expected_receiver);
        assert_eq!(res.amount, expected_amount);
    }

    #[test]
    fn test_parse_l2_withdrawal_message_when_address_p2pkh() {
        let l2_to_l1_message = hex::decode("6c0960f93141317a5031655035514765666932444d505466544c35534c6d7637446976664e610000000000000000000000000000000000000000000000000de0b6b3a7640000").unwrap();
        let expected_receiver = BitcoinAddress::from_str("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa")
            .unwrap()
            .assume_checked();
        let expected_amount = Amount::from_sat(1000000000000000000);
        let res = parse_l2_withdrawal_message(l2_to_l1_message, Network::Bitcoin).unwrap();

        assert_eq!(res.address, expected_receiver);
        assert_eq!(res.amount, expected_amount);
    }
}
