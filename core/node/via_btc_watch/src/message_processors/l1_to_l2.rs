use via_btc_client::types::{BitcoinAddress, FullInscriptionMessage, L1ToL2Message};
use zksync_dal::{Connection, Core, CoreDal};
use zksync_types::{
    helpers::unix_timestamp_ms, l1::L1Tx, Execute, L1TxCommonData, PriorityOpId, U256,
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
                    let l1_tx = self.create_l1_tx_from_message(&l1_to_l2_msg)?;
                    priority_ops.push(l1_tx);
                }
            }
        }

        if priority_ops.is_empty() {
            return Ok(());
        }

        priority_ops.sort_by_key(|op| op.common_data.serial_id);

        let first = &priority_ops[0];
        let last = &priority_ops[priority_ops.len() - 1];
        tracing::debug!(
            "Received priority requests with serial ids: {} (block {}) - {} (block {})",
            first.serial_id(),
            first.eth_block(),
            last.serial_id(),
            last.eth_block(),
        );

        if last.serial_id().0 - first.serial_id().0 + 1 != priority_ops.len() as u64 {
            return Err(MessageProcessorError::PriorityOpsGap);
        }

        let new_ops: Vec<_> = priority_ops
            .into_iter()
            .skip_while(|tx| tx.serial_id() < self.next_expected_priority_id)
            .collect();

        if new_ops.is_empty() {
            return Ok(());
        }

        let first_new = new_ops.first().unwrap();
        if first_new.serial_id() != self.next_expected_priority_id {
            return Err(MessageProcessorError::PriorityIdMismatch {
                expected: self.next_expected_priority_id,
                actual: first_new.serial_id(),
            });
        }

        for new_op in &new_ops {
            let eth_block = new_op.eth_block();
            tracing::debug!(
                "Inserting new priority operation with serial id {:?} (block {})",
                new_op,
                eth_block
            );
            storage
                .via_transactions_dal()
                .insert_transaction_l1(new_op, eth_block)
                .await
                .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;
        }

        if let Some(last_new) = new_ops.last() {
            self.next_expected_priority_id = last_new.serial_id().next();
        }

        Ok(())
    }
}

impl L1ToL2MessageProcessor {
    fn create_l1_tx_from_message(
        &self,
        msg: &L1ToL2Message,
    ) -> Result<L1Tx, MessageProcessorError> {
        let amount = msg.amount.to_sat();
        let eth_address_sender = via_btc_client::indexer::get_eth_address(&msg.common)
            .ok_or_else(|| MessageProcessorError::EthAddressParsingError)?;
        let eth_address_l2 = msg.input.receiver_l2_address;
        let calldata = msg.input.call_data.clone();

        Ok(L1Tx {
            execute: Execute {
                contract_address: Default::default(),
                calldata,
                value: Default::default(),
                factory_deps: vec![],
            },
            common_data: L1TxCommonData {
                sender: eth_address_l2,
                serial_id: self.next_expected_priority_id,
                layer_2_tip_fee: Default::default(),
                full_fee: Default::default(),
                max_fee_per_gas: Default::default(),
                gas_limit: Default::default(),
                gas_per_pubdata_limit: Default::default(),
                op_processing_type: Default::default(),
                priority_queue_type: Default::default(),
                canonical_tx_hash: Default::default(),
                to_mint: U256::from(amount),
                refund_recipient: Default::default(),
                eth_block: msg.common.block_height as u64,
            },
            received_timestamp_ms: unix_timestamp_ms(),
        })
    }
}
