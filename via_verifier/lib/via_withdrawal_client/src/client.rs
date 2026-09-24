use std::{collections::HashSet, str::FromStr};

use anyhow::{ensure, Context};
use bitcoin::Network;
use ethers::abi::{decode, encode, ParamType};
use via_da_client::{pubdata::Pubdata, types::L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR};
use via_verifier_types::withdrawal::{
    CompleteWithdrawalBatch, NonPayableWithdrawal, WithdrawalRequest,
};
use zksync_da_client::DataAvailabilityClient;
use zksync_types::{
    api::{BlockId, BlockNumber, Log, TransactionReceipt, TransactionVariant},
    web3::keccak256,
    Address, L1BatchNumber, L2BlockNumber, H256, U256, U64,
};
use zksync_web3_decl::{
    client::{DynClient, L2},
    namespaces::{EthNamespaceClient, ZksNamespaceClient},
};

use crate::withdraw::{parse_l2_withdrawal_message, ParsedWithdrawal};

#[derive(Debug, Clone)]
pub struct WithdrawalClient {
    pub network: Network,
    client: Box<dyn DataAvailabilityClient>,
    web3_client: Box<DynClient<L2>>,
}

impl WithdrawalClient {
    pub fn new(
        client: Box<dyn DataAvailabilityClient>,
        network: Network,
        web3_client: Box<DynClient<L2>>,
    ) -> Self {
        Self {
            client,
            network,
            web3_client,
        }
    }

    pub async fn get_withdrawals(
        &self,
        blob_id: &str,
        batch: L1BatchNumber,
    ) -> anyhow::Result<CompleteWithdrawalBatch> {
        let pubdata = self
            .client
            .get_inclusion_data(blob_id)
            .await?
            .context("Missing DA inclusion data")?
            .data;
        let decoded = decode_pubdata(&pubdata)?;
        let chain_id = self.web3_client.chain_id().await?.as_u64();
        let details = self
            .web3_client
            .get_l1_batch_details(batch)
            .await?
            .context("Missing L1 batch metadata")?;
        ensure!(
            details.number == batch && details.base.root_hash.is_some(),
            "Unsealed or inconsistent L1 batch metadata"
        );
        let (start, end) = self
            .web3_client
            .get_l2_block_range(batch)
            .await?
            .context("Missing L1 batch block range")?;
        ensure!(
            start <= end && end.as_u64() <= u64::from(u32::MAX),
            "Invalid L1 batch block range"
        );
        let mut receipts = Vec::new();
        let mut previous_hash = None;
        let mut protocol_version = None;
        let mut transactions = HashSet::new();
        for number in start.as_u64()..=end.as_u64() {
            let block = self
                .web3_client
                .get_block_by_number(BlockNumber::Number(number.into()), false)
                .await?
                .context("Missing batch block")?;
            ensure!(
                block.number == U64::from(number)
                    && block.l1_batch_number == Some(U64::from(batch.0)),
                "Block is outside requested batch"
            );
            if let Some(hash) = previous_hash {
                ensure!(block.parent_hash == hash, "Discontinuous batch block chain");
            }
            previous_hash = Some(block.hash);
            let block_details = self
                .web3_client
                .get_block_details(L2BlockNumber(u32::try_from(number)?))
                .await?
                .context("Missing L2 block metadata")?;
            ensure!(
                block_details.number.0 == number as u32
                    && block_details.l1_batch_number == batch
                    && block_details.base.root_hash == Some(block.hash),
                "Inconsistent L2 block metadata"
            );
            let version = block_details
                .protocol_version
                .context("Missing batch protocol version")? as u16;
            if let Some(previous) = protocol_version {
                ensure!(previous == version, "Mixed protocol versions in batch");
            }
            protocol_version = Some(version);
            let block_receipts = self
                .web3_client
                .get_block_receipts(BlockId::Hash(block.hash))
                .await?
                .context("Missing block receipts")?;
            ensure!(
                block_receipts.len() == block.transactions.len(),
                "Incomplete block receipts"
            );
            for (index, (transaction, receipt)) in
                block.transactions.iter().zip(&block_receipts).enumerate()
            {
                let hash = match transaction {
                    TransactionVariant::Hash(hash) => *hash,
                    TransactionVariant::Full(transaction) => transaction.hash,
                };
                ensure!(transactions.insert(hash), "Repeated transaction in batch");
                ensure!(
                    receipt.transaction_hash == hash
                        && receipt.block_hash == block.hash
                        && receipt.block_number == block.number
                        && receipt.transaction_index == U64::from(index),
                    "Receipt does not match ordered block transaction"
                );
            }
            receipts.extend(block_receipts);
        }
        let expected_count = details
            .base
            .l1_tx_count
            .checked_add(details.base.l2_tx_count)
            .context("Batch transaction count overflow")?;
        ensure!(
            receipts.len() == expected_count,
            "Incomplete batch transactions"
        );
        // A changing RPC snapshot must not turn a partial import into complete-empty evidence.
        ensure!(
            self.web3_client.get_l2_block_range(batch).await? == Some((start, end)),
            "Batch range changed during import"
        );
        let final_details = self
            .web3_client
            .get_l1_batch_details(batch)
            .await?
            .context("Batch disappeared during import")?;
        ensure!(
            final_details.number == details.number
                && final_details.base.root_hash == details.base.root_hash
                && final_details.base.l1_tx_count == details.base.l1_tx_count
                && final_details.base.l2_tx_count == details.base.l2_tx_count,
            "Batch metadata changed during import"
        );
        let (withdrawals, nonpayable) =
            associate_withdrawals(self.network, batch, &decoded, &receipts)?;
        Ok(CompleteWithdrawalBatch {
            batch_number: batch.0,
            chain_id,
            network: self.network,
            protocol_version: protocol_version.context("Missing protocol version")?,
            blob_id: blob_id.to_owned(),
            pubdata_hash: H256::from(keccak256(&pubdata)),
            start_block: start.as_u64(),
            end_block: end.as_u64(),
            pubdata,
            receipts,
            withdrawals,
            nonpayable,
        })
    }
}

/// Bound the existing codec's length allocations and reject noncanonical serialized booleans.
/// Bytes after the messages belong to the remaining protocol pubdata and are retained unchanged.
fn decode_pubdata(bytes: &[u8]) -> anyhow::Result<Pubdata> {
    fn take_u32(bytes: &mut &[u8]) -> anyhow::Result<usize> {
        ensure!(bytes.len() >= 4, "Truncated pubdata length");
        let value = u32::from_be_bytes(bytes[..4].try_into()?);
        *bytes = &bytes[4..];
        Ok(value as usize)
    }
    let mut remaining = bytes;
    let logs = take_u32(&mut remaining)?;
    ensure!(logs <= remaining.len() / 88, "Truncated pubdata logs");
    for log in remaining[..logs * 88].chunks_exact(88) {
        ensure!(log[1] <= 1, "Noncanonical pubdata service flag");
    }
    remaining = &remaining[logs * 88..];
    let messages = take_u32(&mut remaining)?;
    ensure!(
        messages <= remaining.len() / 4,
        "Truncated pubdata messages"
    );
    for _ in 0..messages {
        let len = take_u32(&mut remaining)?;
        ensure!(len <= remaining.len(), "Truncated pubdata message");
        remaining = &remaining[len..];
    }
    Pubdata::decode_pubdata(bytes.to_vec())
}

fn validate_event(log: &Log, receipt: &TransactionReceipt, index: usize) -> anyhow::Result<()> {
    ensure!(
        !log.is_removed()
            && log.transaction_hash == Some(receipt.transaction_hash)
            && log.block_hash == Some(receipt.block_hash)
            && log.block_number == Some(receipt.block_number)
            && log.l1_batch_number == receipt.l1_batch_number
            && log.transaction_index == Some(receipt.transaction_index)
            && log.transaction_log_index == Some(U256::from(index)),
        "Event origin/order differs from receipt"
    );
    Ok(())
}

// zkSync Era stores L2-to-L1 logs in transaction and within-transaction order.
// Preserve that order across receipts and DA; equal message hashes are not unique origins:
// https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/dal/src/events_dal.rs#L119-L179
fn associate_withdrawals(
    network: Network,
    batch: L1BatchNumber,
    pubdata: &Pubdata,
    receipts: &[TransactionReceipt],
) -> anyhow::Result<(Vec<WithdrawalRequest>, Vec<NonPayableWithdrawal>)> {
    let messenger = Address::from_low_u64_be(0x8008);
    let bridge = Address::from_str(L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR)?;
    let message_topic = H256::from(keccak256(b"L1MessageSent(address,bytes32,bytes)"));
    let withdrawal_topic = H256::from(keccak256(b"Withdrawal(address,bytes,uint256)"));
    let mut da_logs = pubdata.user_logs.iter();
    let mut da_messages = pubdata.l2_to_l1_messages.iter();
    let mut withdrawals = Vec::new();
    let mut nonpayable = Vec::new();
    let mut origins = HashSet::new();
    let mut block = None;
    let mut event_index = 0usize;
    let mut service_index = 0usize;
    // https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/types/src/api/mod.rs#L219-L266
    // https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/types/src/api/mod.rs#L518-L531
    // https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/dal/src/models/storage_event.rs#L72-L88
    // https://github.com/vianetwork/era-contracts/blob/a14df38896f6e7eafc45e65eefdb63bf3cacebe4/system-contracts/contracts/L1Messenger.sol#L118-L158
    // https://github.com/vianetwork/era-contracts/blob/a14df38896f6e7eafc45e65eefdb63bf3cacebe4/system-contracts/contracts/L2BaseToken.sol#L74-L108
    // Bind ordered service logs -> messages -> withdrawals, not a hash-keyed set or amount search.
    for (tx_index, receipt) in receipts.iter().enumerate() {
        ensure!(
            receipt.l1_batch_number == Some(U64::from(batch.0))
                && receipt.l1_batch_tx_index == Some(U64::from(tx_index)),
            "Receipt batch origin/order mismatch"
        );
        if block != Some(receipt.block_number) {
            block = Some(receipt.block_number);
            event_index = 0;
            service_index = 0;
        }
        let mut message_logs = Vec::new();
        for (index, log) in receipt.l2_to_l1_logs.iter().enumerate() {
            ensure!(
                log.transaction_hash == receipt.transaction_hash
                    && log.block_number == receipt.block_number
                    && log
                        .block_hash
                        .map_or(true, |hash| hash == receipt.block_hash)
                    && log.l1_batch_number == receipt.l1_batch_number
                    && log.transaction_index == receipt.transaction_index
                    && log.tx_index_in_l1_batch == receipt.l1_batch_tx_index
                    && log.transaction_log_index == U256::from(index)
                    && log.log_index == U256::from(service_index),
                "Service log origin/order mismatch"
            );
            service_index += 1;
            let da = da_logs.next().context("Receipt has excess L2-to-L1 logs")?;
            ensure!(
                u64::from(da.l2_shard_id) == log.shard_id.as_u64()
                    && da.is_service == log.is_service
                    && Some(U64::from(da.tx_number_in_block)) == log.tx_index_in_l1_batch
                    && da.sender == log.sender
                    && da.key == log.key
                    && da.value == log.value,
                "Serialized DA log differs from receipt"
            );
            if log.sender == messenger && log.is_service {
                ensure!(log.shard_id == U64::zero(), "Unsupported messenger shard");
                message_logs.push(log);
            }
        }
        let mut message_logs = message_logs.into_iter();
        let mut pending_withdrawal = None;
        for (index, event) in receipt.logs.iter().enumerate() {
            validate_event(event, receipt, index)?;
            ensure!(
                event.log_index == Some(U256::from(event_index)),
                "Event block order mismatch"
            );
            event_index += 1;
            if event.address == messenger && event.topics.first() == Some(&message_topic) {
                ensure!(
                    event.topics.len() == 3 && event.topics[1].as_bytes()[..12] == [0; 12],
                    "Malformed messenger event topics"
                );
                let tokens = decode(&[ParamType::Bytes], &event.data.0)?;
                ensure!(
                    encode(&tokens) == event.data.0,
                    "Noncanonical messenger event ABI"
                );
                let message = tokens[0]
                    .clone()
                    .into_bytes()
                    .context("Malformed messenger bytes")?;
                let service = message_logs
                    .next()
                    .context("Messenger event missing service log")?;
                let da_message = da_messages
                    .next()
                    .context("Messenger event missing DA message")?;
                ensure!(
                    service.key == event.topics[1]
                        && service.value == event.topics[2]
                        && H256::from(keccak256(&message)) == service.value
                        && &message == da_message,
                    "Messenger/service/DA message association mismatch"
                );
                if event.topics[1] == H256::from(bridge) {
                    ensure!(
                        pending_withdrawal.is_none(),
                        "Missing ordered withdrawal event"
                    );
                    pending_withdrawal = Some(message);
                }
            }
            if event.address == bridge && event.topics.first() == Some(&withdrawal_topic) {
                ensure!(
                    receipt.status == U64::one(),
                    "Withdrawal in failed transaction"
                );
                let message = pending_withdrawal
                    .take()
                    .context("Withdrawal missing preceding messenger event")?;
                let origin = match parse_l2_withdrawal_message(&message, event, network)? {
                    ParsedWithdrawal::Payable(request) => {
                        let origin = (request.l2_tx_hash, request.l2_tx_log_index);
                        withdrawals.push(request);
                        origin
                    }
                    ParsedWithdrawal::NonPayable(request) => {
                        let origin = (request.l2_tx_hash, request.l2_tx_log_index);
                        nonpayable.push(request);
                        origin
                    }
                };
                ensure!(origins.insert(origin), "Duplicate full withdrawal origin");
            }
        }
        ensure!(
            message_logs.next().is_none() && pending_withdrawal.is_none(),
            "Unmatched message or withdrawal in receipt"
        );
    }
    ensure!(
        da_logs.next().is_none() && da_messages.next().is_none(),
        "DA contains unaccounted logs or messages"
    );
    Ok((withdrawals, nonpayable))
}

#[cfg(test)]
mod tests {
    use ethers::abi::Token;
    use via_da_client::types::{L1MessengerL2ToL1Log, WITHDRAW_FUNC_SIG};
    use zksync_types::{api::L2ToL1Log, web3::Bytes};

    use super::*;

    fn fixture() -> (Pubdata, Vec<TransactionReceipt>) {
        let receiver = b"bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56";
        fixture_with_receivers(&[receiver, receiver])
    }

    fn fixture_with_receivers(receivers: &[&[u8]]) -> (Pubdata, Vec<TransactionReceipt>) {
        let bridge = Address::from_str(L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR).unwrap();
        let messenger = Address::from_low_u64_be(0x8008);
        let mut pubdata = Pubdata::default();
        let mut receipts = Vec::new();
        for (tx, receiver) in receivers.iter().enumerate() {
            let tx = tx as u64;
            let mut message = keccak256(WITHDRAW_FUNC_SIG.as_bytes())[..4].to_vec();
            message.extend_from_slice(receiver);
            let mut sats = [0u8; 32];
            U256::from(1000).to_big_endian(&mut sats);
            message.extend(sats);
            let hash = H256::from(keccak256(&message));
            let receipt_hash = H256::from_low_u64_be(tx + 1);
            let block_hash = H256::repeat_byte(3);
            let event = Log {
                address: messenger,
                topics: vec![
                    H256::from(keccak256(b"L1MessageSent(address,bytes32,bytes)")),
                    H256::from(bridge),
                    hash,
                ],
                data: Bytes(encode(&[Token::Bytes(message.clone())])),
                block_hash: Some(block_hash),
                block_number: Some(U64::one()),
                l1_batch_number: Some(U64::one()),
                transaction_hash: Some(receipt_hash),
                transaction_index: Some(tx.into()),
                transaction_log_index: Some(U256::zero()),
                log_index: Some(U256::from(tx * 2)),
                log_type: None,
                removed: Some(false),
                block_timestamp: None,
            };
            let mut withdrawal = event.clone();
            withdrawal.address = bridge;
            withdrawal.topics = vec![
                H256::from(keccak256(b"Withdrawal(address,bytes,uint256)")),
                H256::from(Address::repeat_byte(7)),
            ];
            withdrawal.data = Bytes(encode(&[
                Token::Bytes(receiver.to_vec()),
                Token::Uint(U256::from(10_000_000_000_001_u64)),
            ]));
            withdrawal.transaction_log_index = Some(U256::one());
            withdrawal.log_index = Some(U256::from(tx * 2 + 1));
            pubdata.user_logs.push(L1MessengerL2ToL1Log {
                l2_shard_id: 0,
                is_service: true,
                tx_number_in_block: tx as u16,
                sender: messenger,
                key: H256::from(bridge),
                value: hash,
            });
            pubdata.l2_to_l1_messages.push(message.clone());
            receipts.push(TransactionReceipt {
                transaction_hash: receipt_hash,
                transaction_index: tx.into(),
                block_hash,
                block_number: U64::one(),
                l1_batch_tx_index: Some(tx.into()),
                l1_batch_number: Some(U64::one()),
                status: U64::one(),
                logs: vec![event, withdrawal],
                l2_to_l1_logs: vec![L2ToL1Log {
                    block_hash: None,
                    block_number: U64::one(),
                    l1_batch_number: Some(U64::one()),
                    log_index: U256::from(tx),
                    transaction_index: tx.into(),
                    transaction_hash: receipt_hash,
                    transaction_log_index: U256::zero(),
                    tx_index_in_l1_batch: Some(tx.into()),
                    shard_id: U64::zero(),
                    is_service: true,
                    sender: messenger,
                    key: H256::from(bridge),
                    value: hash,
                }],
                ..TransactionReceipt::default()
            });
        }
        (pubdata, receipts)
    }

    #[test]
    fn unpayable_receivers_are_complete_evidence_not_eligible_requests() {
        use via_verifier_types::withdrawal::NonPayableWithdrawalReason::*;
        let receivers: &[&[u8]] = &[
            &[0xff],
            b"garbage",
            b"1BoatSLRHtKNngkdXEeobR76b53LETtpyT",
            b"",
            &[0xff],
            b"bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
        ];
        let (pubdata, receipts) = fixture_with_receivers(receivers);
        let (withdrawals, nonpayable) =
            associate_withdrawals(Network::Regtest, L1BatchNumber(1), &pubdata, &receipts).unwrap();
        assert_eq!(withdrawals.len(), 1);
        assert_eq!(withdrawals[0].l2_tx_hash, receipts[5].transaction_hash);
        assert_eq!(withdrawals[0].amount.to_sat(), 1000);
        assert_eq!(nonpayable.len(), 5);
        for (index, reason) in [
            InvalidUtf8,
            InvalidAddress,
            WrongNetwork,
            InvalidAddress,
            InvalidUtf8,
        ]
        .into_iter()
        .enumerate()
        {
            let item = &nonpayable[index];
            assert_eq!(item.reason, reason);
            assert_eq!(item.raw_receiver, receivers[index]);
            assert_eq!(item.amount.to_sat(), 1000);
            assert_eq!(item.l2_tx_hash, receipts[index].transaction_hash);
            assert_eq!(item.l2_tx_log_index, 1);
            assert_eq!(item.l2_sender, Address::repeat_byte(7));
            assert_eq!(item.id, "00000000000000000001");
        }
        // Retained batch evidence includes every raw receipt/message, not just payable entries.
        let bytes = pubdata.encode_pubdata();
        let batch = CompleteWithdrawalBatch {
            batch_number: 1,
            chain_id: 270,
            network: Network::Regtest,
            protocol_version: 1,
            blob_id: "fixture".to_owned(),
            pubdata_hash: H256::from(keccak256(&bytes)),
            start_block: 1,
            end_block: 1,
            pubdata: bytes,
            receipts,
            withdrawals,
            nonpayable,
        };
        let restored: CompleteWithdrawalBatch =
            serde_json::from_slice(&serde_json::to_vec(&batch).unwrap()).unwrap();
        assert_eq!(restored, batch);
        assert_eq!(
            restored.withdrawals.len() + restored.nonpayable.len(),
            receivers.len()
        );
    }

    #[test]
    fn unusable_receivers_do_not_hide_inconsistent_or_missing_evidence() {
        let (pubdata, receipts) = fixture_with_receivers(&[&[0xff]]);
        let mut changed = receipts.clone();
        changed[0].logs[1].data = Bytes(encode(&[
            Token::Bytes(vec![0xff]),
            Token::Uint(U256::one()),
        ]));
        assert!(
            associate_withdrawals(Network::Regtest, L1BatchNumber(1), &pubdata, &changed).is_err()
        );
        let mut changed = receipts.clone();
        changed[0].logs[1].data.0.pop();
        assert!(
            associate_withdrawals(Network::Regtest, L1BatchNumber(1), &pubdata, &changed).is_err()
        );
        let mut changed = receipts.clone();
        changed[0].logs.remove(0);
        assert!(
            associate_withdrawals(Network::Regtest, L1BatchNumber(1), &pubdata, &changed).is_err()
        );
        let mut changed = pubdata.clone();
        changed.l2_to_l1_messages.clear();
        assert!(
            associate_withdrawals(Network::Regtest, L1BatchNumber(1), &changed, &receipts).is_err()
        );
    }

    #[test]
    fn identical_messages_preserve_distinct_full_origins_and_short_collisions() {
        let (pubdata, receipts) = fixture();
        let (requests, nonpayable) =
            associate_withdrawals(Network::Regtest, L1BatchNumber(1), &pubdata, &receipts).unwrap();
        assert!(nonpayable.is_empty());
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].amount.to_sat(), 1000);
        assert_eq!(requests[0].receiver, requests[1].receiver);
        assert_ne!(requests[0].l2_tx_hash, requests[1].l2_tx_hash);
        assert_eq!(requests[0].id, requests[1].id);
    }

    #[test]
    fn identical_messages_in_one_transaction_keep_event_identity() {
        let (mut pubdata, mut receipts) = fixture();
        let mut second = receipts.pop().unwrap();
        let first = &mut receipts[0];
        for (index, event) in second.logs.iter_mut().enumerate() {
            event.transaction_hash = Some(first.transaction_hash);
            event.transaction_index = Some(U64::zero());
            event.transaction_log_index = Some(U256::from(index + 2));
        }
        second.l2_to_l1_logs[0].transaction_hash = first.transaction_hash;
        second.l2_to_l1_logs[0].transaction_index = U64::zero();
        second.l2_to_l1_logs[0].tx_index_in_l1_batch = Some(U64::zero());
        second.l2_to_l1_logs[0].transaction_log_index = U256::one();
        first.logs.extend(second.logs);
        first.l2_to_l1_logs.extend(second.l2_to_l1_logs);
        pubdata.user_logs[1].tx_number_in_block = 0;
        let (requests, nonpayable) =
            associate_withdrawals(Network::Regtest, L1BatchNumber(1), &pubdata, &receipts).unwrap();
        assert!(nonpayable.is_empty());
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].l2_tx_hash, requests[1].l2_tx_hash);
        assert_eq!(requests[0].id, "00000000000000000001");
        assert_eq!(requests[1].id, "00000000000000000003");
    }

    #[test]
    fn every_serialized_service_log_field_is_bound() {
        let (pubdata, receipts) = fixture();
        for field in 0..6 {
            let mut changed = pubdata.clone();
            let log = &mut changed.user_logs[0];
            match field {
                0 => log.l2_shard_id = 1,
                1 => log.is_service = false,
                2 => log.tx_number_in_block = 1,
                3 => log.sender = Address::zero(),
                4 => log.key = H256::zero(),
                5 => log.value = H256::zero(),
                _ => unreachable!(),
            }
            assert!(
                associate_withdrawals(Network::Regtest, L1BatchNumber(1), &changed, &receipts)
                    .is_err()
            );
        }
    }

    #[test]
    fn missing_or_extra_evidence_is_not_complete_empty() {
        let (pubdata, receipts) = fixture();
        let mut missing_message = pubdata.clone();
        missing_message.l2_to_l1_messages.pop();
        assert!(associate_withdrawals(
            Network::Regtest,
            L1BatchNumber(1),
            &missing_message,
            &receipts
        )
        .is_err());
        assert!(associate_withdrawals(
            Network::Regtest,
            L1BatchNumber(1),
            &pubdata,
            &receipts[..1]
        )
        .is_err());
        let mut missing_event = receipts.clone();
        missing_event[1].logs.pop();
        assert!(associate_withdrawals(
            Network::Regtest,
            L1BatchNumber(1),
            &pubdata,
            &missing_event
        )
        .is_err());
        let mut reversed = receipts;
        reversed[0].logs.swap(0, 1);
        assert!(
            associate_withdrawals(Network::Regtest, L1BatchNumber(1), &pubdata, &reversed).is_err()
        );
        assert_eq!(
            associate_withdrawals(Network::Regtest, L1BatchNumber(1), &Pubdata::default(), &[])
                .unwrap(),
            (Vec::new(), Vec::new())
        );
    }

    #[test]
    fn receipt_and_event_origin_relabeling_is_rejected() {
        let (pubdata, mut receipts) = fixture();
        receipts[0].logs[1].transaction_hash = Some(H256::repeat_byte(9));
        assert!(
            associate_withdrawals(Network::Regtest, L1BatchNumber(1), &pubdata, &receipts).is_err()
        );
        let (pubdata, mut receipts) = fixture();
        receipts[1].l2_to_l1_logs[0].tx_index_in_l1_batch = Some(U64::zero());
        assert!(
            associate_withdrawals(Network::Regtest, L1BatchNumber(1), &pubdata, &receipts).is_err()
        );
    }

    #[test]
    fn truncated_pubdata_and_noncanonical_flags_are_errors() {
        let (pubdata, _) = fixture();
        let bytes = pubdata.encode_pubdata();
        for len in 0..bytes.len() {
            assert!(decode_pubdata(&bytes[..len]).is_err());
        }
        let mut malformed = bytes;
        malformed[5] = 2;
        assert!(decode_pubdata(&malformed).is_err());
        assert!(decode_pubdata(&[255; 8]).is_err());
    }
}
