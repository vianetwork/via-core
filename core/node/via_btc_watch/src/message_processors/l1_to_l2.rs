use via_btc_client::types::{BitcoinAddress, FullInscriptionMessage, L1ToL2Message};
use zksync_dal::{Connection, Core, CoreDal};
use zksync_types::{
    abi::L2CanonicalTransaction,
    helpers::unix_timestamp_ms,
    l1::{L1Tx, OpProcessingType, PriorityQueueType},
    Address, Execute, L1TxCommonData, PriorityOpId, H256, PRIORITY_OPERATION_L2_TX_TYPE, U256,
};

use crate::message_processors::{MessageProcessor, MessageProcessorError};

#[derive(Debug)]
pub struct L1ToL2MessageProcessor {
    bridge_address: BitcoinAddress,
    next_expected_priority_id: PriorityOpId,
}

impl L1ToL2MessageProcessor {
    pub fn new(bridge_address: BitcoinAddress, next_expected_priority_id: PriorityOpId) -> Self {
        Self {
            bridge_address,
            next_expected_priority_id,
        }
    }
}

#[async_trait::async_trait]
impl MessageProcessor for L1ToL2MessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
    ) -> Result<(), MessageProcessorError> {
        let mut priority_ops = Vec::new();
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
                        tracing::debug!(
                            "Transaction with tx_id {} already processed, skipping",
                            tx_id
                        );
                        continue;
                    }
                    let serial_id = self.next_expected_priority_id;
                    let l1_tx = self.create_l1_tx_from_message(&l1_to_l2_msg, serial_id)?;
                    priority_ops.push((l1_tx, tx_id));
                    self.next_expected_priority_id = self.next_expected_priority_id.next();
                }
            }
        }

        if priority_ops.is_empty() {
            return Ok(());
        }

        for (new_op, txid) in priority_ops {
            storage
                .via_transactions_dal()
                .insert_transaction_l1(&new_op, new_op.eth_block(), txid)
                .await
                .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;
        }

        Ok(())
    }
}

impl L1ToL2MessageProcessor {
    fn create_l1_tx_from_message(
        &self,
        msg: &L1ToL2Message,
        serial_id: PriorityOpId,
    ) -> Result<L1Tx, MessageProcessorError> {
        let amount = msg.amount.to_sat();
        let eth_address_l2 = msg.input.receiver_l2_address;
        let calldata = msg.input.call_data.clone();

        let value = U256::from(amount);
        let mantissa = U256::from(10_000_000_000u64); // scale down the cost Eth 18 decimals - BTC 8 decimals
        let max_fee_per_gas = U256::from(100_000_000_000u64) / mantissa;
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

        tracing::debug!(
            "Created L1 transaction with serial id {:?} (block {}) with deposit amount {} and tx hash {}",
            l1_tx.common_data.serial_id,
            l1_tx.common_data.eth_block,
            amount,
            l1_tx.common_data.canonical_tx_hash,
        );
        Ok(l1_tx)
    }
}

fn address_to_u256(address: &Address) -> U256 {
    U256::from_big_endian(&address.0)
}
