use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinAddress, FullInscriptionMessage, L1ToL2Message},
};
use zksync_dal::{Connection, Core, CoreDal};
use zksync_types::{
    l1::{via_l1::ViaL1Deposit, L1Tx},
    PriorityOpId, H256,
};

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::{InscriptionStage, METRICS},
};

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
        _: &mut BitcoinInscriptionIndexer,
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
                        tracing::info!(
                            "Transaction with tx_id {} already processed, skipping",
                            tx_id
                        );
                        continue;
                    }
                    let serial_id = self.next_expected_priority_id;
                    let Some(l1_tx) = self.create_l1_tx_from_message(&l1_to_l2_msg, serial_id)
                    else {
                        tracing::warn!("Invalid deposit, l1 tx_id {}", &l1_to_l2_msg.common.tx_id);
                        continue;
                    };

                    priority_ops.push((l1_tx, tx_id));
                    self.next_expected_priority_id = self.next_expected_priority_id.next();
                }
            }
        }

        if priority_ops.is_empty() {
            return Ok(());
        }

        for (new_op, txid) in priority_ops {
            METRICS.inscriptions_processed[&InscriptionStage::Deposit]
                .set(new_op.common_data.serial_id.0 as usize);
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
    ) -> Option<L1Tx> {
        let deposit = ViaL1Deposit {
            l2_receiver_address: msg.input.receiver_l2_address,
            amount: msg.amount.to_sat(),
            calldata: msg.input.call_data.clone(),
            serial_id,
            l1_block_number: msg.common.block_height as u64,
        };

        if let Some(l1_tx) = deposit.l1_tx() {
            tracing::info!(
                "Created L1 transaction with serial id {:?} (block {}) with deposit amount {} and tx hash {}",
                l1_tx.common_data.serial_id,
                l1_tx.common_data.eth_block,
                deposit.amount,
                l1_tx.common_data.canonical_tx_hash,
            );
            return Some(l1_tx);
        }
        None
    }
}
