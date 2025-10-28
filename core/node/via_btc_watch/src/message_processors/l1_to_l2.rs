use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{FullInscriptionMessage, L1ToL2Message},
};
use zksync_dal::{Connection, Core, CoreDal};
use zksync_types::{
    l1::{via_l1::ViaL1Deposit, L1Tx},
    H256,
};

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::METRICS,
};

#[derive(Debug, Default)]
pub struct L1ToL2MessageProcessor {}

#[async_trait::async_trait]
impl MessageProcessor for L1ToL2MessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        let mut priority_ops = Vec::new();
        for msg in msgs {
            if let FullInscriptionMessage::L1ToL2Message(l1_to_l2_msg) = msg {
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
                let Some(l1_tx) = self.create_l1_tx_from_message(&l1_to_l2_msg)? else {
                    tracing::warn!("Invalid deposit, l1 tx_id {}", &l1_to_l2_msg.common.tx_id);
                    continue;
                };

                priority_ops.push((l1_tx, tx_id));
            }
        }

        if priority_ops.is_empty() {
            return Ok(None);
        }

        for (new_op, txid) in priority_ops {
            METRICS.deposit.inc();

            storage
                .via_transactions_dal()
                .insert_transaction_l1(&new_op, new_op.eth_block(), txid)
                .await
                .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;
        }

        Ok(None)
    }
}

impl L1ToL2MessageProcessor {
    fn create_l1_tx_from_message(
        &self,
        msg: &L1ToL2Message,
    ) -> Result<Option<L1Tx>, MessageProcessorError> {
        let deposit = ViaL1Deposit {
            l2_receiver_address: msg.input.receiver_l2_address,
            amount: msg.amount.to_sat(),
            calldata: msg.input.call_data.clone(),
            l1_block_number: msg.common.block_height as u64,
            tx_index: msg.common.tx_index.ok_or_else(|| {
                MessageProcessorError::Internal(anyhow::anyhow!("deposit missing tx_index"))
            })?,
            output_vout: msg.common.output_vout.ok_or_else(|| {
                MessageProcessorError::Internal(anyhow::anyhow!("deposit missing output_vout"))
            })?,
        };

        if let Some(l1_tx) = deposit.l1_tx() {
            tracing::info!(
                "Created L1 transaction with serial id {:?} (block {}) with deposit amount {} and tx hash {}",
                l1_tx.common_data.serial_id,
                l1_tx.common_data.eth_block,
                deposit.amount,
                l1_tx.common_data.canonical_tx_hash,
            );
            return Ok(Some(l1_tx));
        }
        Ok(None)
    }
}
