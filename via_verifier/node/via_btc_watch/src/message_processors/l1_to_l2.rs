use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{FullInscriptionMessage, L1ToL2Message},
};
use via_verifier_dal::{Connection, Verifier, VerifierDal};
use zksync_types::{ethabi::Address, l1::via_l1::ViaL1Deposit, H256};

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::METRICS,
};

#[derive(Debug)]
pub struct L1ToL2Transaction {
    priority_id: i64,
    l1_block_number: i64,
    tx_id: H256,
    receiver: Address,
    value: i64,
    calldata: Vec<u8>,
    canonical_tx_hash: H256,
}

#[derive(Default, Debug)]
pub struct L1ToL2MessageProcessor {}

#[async_trait::async_trait]
impl MessageProcessor for L1ToL2MessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
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
                let Some(l1_tx) = self.create_l1_tx_from_message(tx_id, &l1_to_l2_msg)? else {
                    tracing::warn!("Invalid deposit, l1 tx_id {}", &l1_to_l2_msg.common.tx_id);
                    continue;
                };

                priority_ops.push(l1_tx);
            }
        }

        if priority_ops.is_empty() {
            return Ok(None);
        }

        for new_op in priority_ops {
            storage
                .via_transactions_dal()
                .insert_transaction(
                    new_op.priority_id,
                    new_op.tx_id,
                    new_op.receiver.to_string(),
                    new_op.value,
                    new_op.calldata,
                    new_op.canonical_tx_hash,
                    new_op.l1_block_number,
                )
                .await
                .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;
        }

        Ok(None)
    }
}

impl L1ToL2MessageProcessor {
    fn create_l1_tx_from_message(
        &self,
        tx_id: H256,
        msg: &L1ToL2Message,
    ) -> Result<Option<L1ToL2Transaction>, MessageProcessorError> {
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

            METRICS.deposit.inc();

            return Ok(Some(L1ToL2Transaction {
                priority_id: deposit.priority_id().0 as i64,
                tx_id,
                l1_block_number: msg.common.block_height as i64,
                receiver: deposit.l2_receiver_address,
                value: deposit.amount as i64,
                calldata: deposit.calldata,
                canonical_tx_hash: l1_tx.common_data.canonical_tx_hash,
            }));
        }
        Ok(None)
    }
}
