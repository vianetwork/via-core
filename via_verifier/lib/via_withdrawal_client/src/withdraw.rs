use std::str::FromStr;

use anyhow::{ensure, Context};
use bitcoin::{Address as BitcoinAddress, Amount, Network};
use ethers::abi::{decode, encode, ParamType};
use via_da_client::types::{L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR, WITHDRAW_FUNC_SIG};
use via_verifier_types::withdrawal::{
    NonPayableWithdrawal, NonPayableWithdrawalReason, WithdrawalRequest,
};
use zksync_types::{api::Log, web3::keccak256, Address, H256, U256};

#[derive(Debug)]
pub enum ParsedWithdrawal {
    Payable(WithdrawalRequest),
    NonPayable(NonPayableWithdrawal),
}

pub fn parse_l2_withdrawal_message(
    message: &[u8],
    log: &Log,
    network: Network,
) -> anyhow::Result<ParsedWithdrawal> {
    let l2_tx_hash = log
        .transaction_hash
        .context("Missing withdrawal transaction hash")?;
    let index = log
        .transaction_log_index
        .context("Missing withdrawal event index")?;
    ensure!(
        index <= U256::from(u16::MAX),
        "Withdrawal event index exceeds wire reference width"
    );
    let index = index.as_u32() as u16;
    ensure!(message.len() >= 36, "Invalid withdrawal message length");
    ensure!(
        message[..4] == keccak256(WITHDRAW_FUNC_SIG.as_bytes())[..4],
        "Invalid withdrawal selector"
    );
    ensure!(
        log.address == Address::from_str(L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR)?,
        "Wrong withdrawal emitter"
    );
    ensure!(
        log.topics.len() == 2
            && log.topics[0] == H256::from(keccak256(b"Withdrawal(address,bytes,uint256)")),
        "Invalid withdrawal topics"
    );
    ensure!(
        log.topics[1].as_bytes()[..12] == [0; 12],
        "Noncanonical withdrawal sender"
    );
    ensure!(!log.is_removed(), "Removed withdrawal event");
    let receiver_bytes = &message[4..message.len() - 32];
    let amount = U256::from_big_endian(&message[message.len() - 32..]);
    let tokens = decode(&[ParamType::Bytes, ParamType::Uint(256)], &log.data.0)?;
    ensure!(
        encode(&tokens) == log.data.0,
        "Noncanonical withdrawal ABI data"
    );
    let event_receiver = tokens[0]
        .clone()
        .into_bytes()
        .context("Invalid withdrawal receiver")?;
    let event_amount = tokens[1]
        .clone()
        .into_uint()
        .context("Invalid withdrawal amount")?;
    ensure!(
        event_receiver == receiver_bytes,
        "Withdrawal receiver differs from DA message"
    );
    // https://github.com/vianetwork/era-contracts/blob/a14df38896f6e7eafc45e65eefdb63bf3cacebe4/system-contracts/contracts/L2BaseToken.sol#L74-L108
    // The contract accepts arbitrary receiver bytes and floors uint256 before emitting the message.
    // Do not narrow the original 18-decimal value or require exact divisibility.
    ensure!(
        event_amount / U256::from(10_000_000_000_u64) == amount,
        "Withdrawal amount differs from DA message"
    );
    ensure!(
        amount <= U256::from(u64::MAX),
        "Withdrawal satoshi amount overflows u64"
    );
    let mut id = [0u8; 10];
    id[..8].copy_from_slice(&l2_tx_hash.as_bytes()[..8]);
    id[8..].copy_from_slice(&index.to_be_bytes());
    let id = hex::encode(id);
    let amount = Amount::from_sat(amount.as_u64());
    let l2_sender = Address::from_slice(&log.topics[1].as_bytes()[12..]);
    // Classify usability only after all content, amount, and origin checks have succeeded.
    let receiver = std::str::from_utf8(receiver_bytes)
        .map_err(|_| NonPayableWithdrawalReason::InvalidUtf8)
        .and_then(|receiver| {
            BitcoinAddress::from_str(receiver)
                .map_err(|_| NonPayableWithdrawalReason::InvalidAddress)
        })
        .and_then(|receiver| {
            receiver
                .require_network(network)
                .map_err(|_| NonPayableWithdrawalReason::WrongNetwork)
        });
    Ok(match receiver {
        Ok(receiver) => ParsedWithdrawal::Payable(WithdrawalRequest {
            id,
            receiver,
            amount,
            l2_sender,
            l2_tx_hash,
            l2_tx_log_index: index,
        }),
        Err(reason) => ParsedWithdrawal::NonPayable(NonPayableWithdrawal {
            id,
            amount,
            l2_sender,
            l2_tx_hash,
            l2_tx_log_index: index,
            raw_receiver: receiver_bytes.to_vec(),
            reason,
        }),
    })
}

#[cfg(test)]
mod tests {
    use ethers::abi::Token;
    use zksync_types::web3::Bytes;

    use super::*;

    pub(super) fn fixture(amount: U256) -> (Vec<u8>, Log) {
        let receiver = b"bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56";
        let mut message = keccak256(WITHDRAW_FUNC_SIG.as_bytes())[..4].to_vec();
        message.extend(receiver);
        let mut sats = [0u8; 32];
        (amount / U256::from(10_000_000_000_u64)).to_big_endian(&mut sats);
        message.extend(sats);
        let log = Log {
            address: Address::from_str(L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR).unwrap(),
            topics: vec![
                H256::from(keccak256(b"Withdrawal(address,bytes,uint256)")),
                H256::from(Address::repeat_byte(9)),
            ],
            data: Bytes(encode(&[
                Token::Bytes(receiver.to_vec()),
                Token::Uint(amount),
            ])),
            transaction_hash: Some(H256::repeat_byte(0xab)),
            transaction_log_index: Some(U256::from(65535)),
            block_hash: None,
            block_number: None,
            l1_batch_number: None,
            transaction_index: None,
            log_index: None,
            log_type: None,
            removed: None,
            block_timestamp: None,
        };
        (message, log)
    }

    #[test]
    fn full_width_floor_and_lossless_origin_round_trip() {
        let (message, log) =
            fixture(U256::from(u64::MAX) * U256::from(10_000_000_000_u64) + U256::from(17));
        let ParsedWithdrawal::Payable(request) =
            parse_l2_withdrawal_message(&message, &log, Network::Regtest).unwrap()
        else {
            panic!("valid receiver must be payable");
        };
        assert_eq!(request.amount.to_sat(), u64::MAX);
        assert_eq!(request.l2_tx_hash, H256::repeat_byte(0xab));
        assert_eq!(request.id, "ababababababababffff");
        let stored = serde_json::to_vec(&request).unwrap();
        assert_eq!(
            serde_json::from_slice::<WithdrawalRequest>(&stored).unwrap(),
            request
        );
    }

    #[test]
    fn nonpayable_preserves_full_width_gross_and_origin_but_not_invalid_content() {
        let amount = U256::from(u64::MAX) * U256::from(10_000_000_000_u64) + U256::from(17);
        let (mut message, mut log) = fixture(amount);
        let sats = message.split_off(message.len() - 32);
        message.truncate(4);
        message.push(0xff);
        message.extend(sats);
        log.data = Bytes(encode(&[Token::Bytes(vec![0xff]), Token::Uint(amount)]));
        let ParsedWithdrawal::NonPayable(item) =
            parse_l2_withdrawal_message(&message, &log, Network::Regtest).unwrap()
        else {
            panic!("invalid UTF-8 cannot be payable");
        };
        assert_eq!(item.amount.to_sat(), u64::MAX);
        assert_eq!(item.l2_tx_hash, H256::repeat_byte(0xab));
        assert_eq!(item.l2_tx_log_index, u16::MAX);
        assert_eq!(item.id, "ababababababababffff");
        assert_eq!(item.l2_sender, Address::repeat_byte(9));
        assert_eq!(item.raw_receiver, vec![0xff]);
        assert_eq!(item.reason, NonPayableWithdrawalReason::InvalidUtf8);
        let stored = serde_json::to_vec(&item).unwrap();
        assert_eq!(
            serde_json::from_slice::<NonPayableWithdrawal>(&stored).unwrap(),
            item
        );
        message[0] ^= 1;
        assert!(parse_l2_withdrawal_message(&message, &log, Network::Regtest).is_err());
        message[0] ^= 1;
        log.transaction_log_index = Some(U256::from(u16::MAX) + U256::one());
        assert!(parse_l2_withdrawal_message(&message, &log, Network::Regtest).is_err());
    }

    #[test]
    fn malformed_lengths_topics_and_wide_indices_are_errors() {
        let (message, mut log) = fixture(U256::from(10_000_000_001_u64));
        for len in 0..36 {
            assert!(parse_l2_withdrawal_message(&message[..len], &log, Network::Regtest).is_err());
        }
        log.transaction_log_index = Some(U256::one() << 200);
        assert!(parse_l2_withdrawal_message(&message, &log, Network::Regtest).is_err());
        log.transaction_log_index = Some(U256::zero());
        log.topics.clear();
        assert!(parse_l2_withdrawal_message(&message, &log, Network::Regtest).is_err());
    }

    #[test]
    fn rejects_amount_overflow_and_mismatched_content() {
        let (message, log) =
            fixture((U256::from(u64::MAX) + U256::one()) * U256::from(10_000_000_000_u64));
        assert!(parse_l2_withdrawal_message(&message, &log, Network::Regtest).is_err());
        let (mut message, log) = fixture(U256::from(10_000_000_000_u64));
        *message.last_mut().unwrap() = 2;
        assert!(parse_l2_withdrawal_message(&message, &log, Network::Regtest).is_err());
    }
}
