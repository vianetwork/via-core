use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinAddress, FullInscriptionMessage, L1ToL2Message},
};
use via_verifier_dal::{Connection, Verifier, VerifierDal};
use zksync_types::{
    abi::L2CanonicalTransaction,
    ethabi::Address,
    helpers::unix_timestamp_ms,
    l1::{L1Tx, OpProcessingType, PriorityQueueType},
    Execute, L1TxCommonData, PriorityOpId, H256, PRIORITY_OPERATION_L2_TX_TYPE, U256,
};

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::{ErrorType, InscriptionStage, METRICS},
};

#[derive(Debug)]
pub struct L1ToL2Transaction {
    priority_id: i64,
    tx_id: H256,
    receiver: Address,
    value: i64,
    calldata: Vec<u8>,
    canonical_tx_hash: H256,
}

#[derive(Debug)]
pub struct L1ToL2MessageProcessor {
    bridge_address: BitcoinAddress,
}

impl L1ToL2MessageProcessor {
    pub fn new(bridge_address: BitcoinAddress) -> Self {
        Self { bridge_address }
    }
}

#[async_trait::async_trait]
impl MessageProcessor for L1ToL2MessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<(), MessageProcessorError> {
        let mut priority_ops = Vec::new();
        let last_priority_id = storage
            .via_transactions_dal()
            .get_last_priority_id()
            .await
            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;
        let mut next_expected_priority_id = PriorityOpId::from(last_priority_id as u64);

        for msg in msgs {
            if let FullInscriptionMessage::L1ToL2Message(l1_to_l2_msg) = msg {
                if l1_to_l2_msg
                    .tx_outputs
                    .iter()
                    .any(|output| output.script_pubkey == self.bridge_address.script_pubkey())
                {
                    let mut tx_id_bytes = l1_to_l2_msg.common.tx_id.as_raw_hash()[..].to_vec();
                    tx_id_bytes.reverse();
                    let tx_id = H256::from_slice(&tx_id_bytes);

                    if storage
                        .via_transactions_dal()
                        .transaction_exists_with_txid(&tx_id)
                        .await
                        .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?
                    {
                        tracing::info!(
                            "Transaction with tx_id {} already processed, skipping",
                            tx_id
                        );
                        continue;
                    }
                    let serial_id = next_expected_priority_id;
                    let l1_tx = self.create_l1_tx_from_message(tx_id, serial_id, &l1_to_l2_msg)?;
                    priority_ops.push(l1_tx);
                    next_expected_priority_id = next_expected_priority_id.next();
                }
            }
        }

        if priority_ops.is_empty() {
            return Ok(());
        }

        for new_op in priority_ops {
            METRICS.inscriptions_processed[&InscriptionStage::Deposit].inc();
            storage
                .via_transactions_dal()
                .insert_transaction(
                    new_op.priority_id,
                    new_op.tx_id,
                    new_op.receiver.to_string(),
                    new_op.value,
                    new_op.calldata,
                    new_op.canonical_tx_hash,
                )
                .await
                .map_err(|e| {
                    METRICS.errors[&ErrorType::DatabaseError].inc();
                    MessageProcessorError::DatabaseError(e.to_string())
                })?;
        }

        Ok(())
    }
}

impl L1ToL2MessageProcessor {
    fn create_l1_tx_from_message(
        &self,
        tx_id: H256,
        serial_id: PriorityOpId,
        msg: &L1ToL2Message,
    ) -> Result<L1ToL2Transaction, MessageProcessorError> {
        let amount = msg.amount.to_sat() as i64;
        let eth_address_l2 = msg.input.receiver_l2_address;
        let calldata = msg.input.call_data.clone();

        let mantissa = U256::from(10_000_000_000u64); // Eth 18 decimals - BTC 8 decimals
        let value = U256::from(amount) * mantissa;
        let max_fee_per_gas = U256::from(100_000_000u64);
        let gas_limit = U256::from(1_000_000u64);
        let gas_per_pubdata_limit = U256::from(800u64);

        let mut l1_tx = L1Tx {
            execute: Execute {
                contract_address: eth_address_l2,
                calldata: calldata.clone(),
                value: U256::zero(),
                factory_deps: vec![],
            },
            common_data: L1TxCommonData {
                sender: eth_address_l2,
                serial_id,
                layer_2_tip_fee: U256::zero(),
                full_fee: U256::zero(),
                max_fee_per_gas,
                gas_limit,
                gas_per_pubdata_limit,
                op_processing_type: OpProcessingType::Common,
                priority_queue_type: PriorityQueueType::Deque,
                canonical_tx_hash: H256::zero(),
                to_mint: value,
                refund_recipient: eth_address_l2,
                eth_block: msg.common.block_height as u64,
            },
            received_timestamp_ms: unix_timestamp_ms(),
        };

        let l2_transaction = L2CanonicalTransaction {
            tx_type: PRIORITY_OPERATION_L2_TX_TYPE.into(),
            from: address_to_u256(&l1_tx.common_data.sender),
            to: address_to_u256(&l1_tx.execute.contract_address),
            gas_limit: l1_tx.common_data.gas_limit,
            gas_per_pubdata_byte_limit: l1_tx.common_data.gas_per_pubdata_limit,
            max_fee_per_gas: l1_tx.common_data.max_fee_per_gas,
            max_priority_fee_per_gas: U256::zero(),
            paymaster: U256::zero(),
            nonce: l1_tx.common_data.serial_id.0.into(),
            value: l1_tx.execute.value,
            reserved: [
                l1_tx.common_data.to_mint,
                address_to_u256(&l1_tx.common_data.refund_recipient),
                U256::zero(),
                U256::zero(),
            ],
            data: l1_tx.execute.calldata.clone(),
            signature: vec![],
            factory_deps: vec![],
            paymaster_input: vec![],
            reserved_dynamic: vec![],
        };

        let canonical_tx_hash = l2_transaction.hash();

        l1_tx.common_data.canonical_tx_hash = canonical_tx_hash;

        tracing::info!(
            "Created L1 transaction with serial id {:?} (block {}) with deposit amount {} and tx hash {}",
            l1_tx.common_data.serial_id,
            l1_tx.common_data.eth_block,
            amount,
            l1_tx.common_data.canonical_tx_hash,
        );

        Ok(L1ToL2Transaction {
            priority_id: serial_id.0 as i64,
            tx_id,
            receiver: eth_address_l2,
            value: amount,
            calldata,
            canonical_tx_hash: l1_tx.common_data.canonical_tx_hash,
        })
    }
}

fn address_to_u256(address: &Address) -> U256 {
    U256::from_big_endian(&address.0)
}
