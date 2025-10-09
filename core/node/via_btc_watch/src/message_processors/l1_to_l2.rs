use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
use via_btc_watch_common::utils::create_l1_tx_from_message;
use zksync_dal::{Connection, Core, CoreDal};

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::{InscriptionStage, METRICS},
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
    ) -> Result<bool, MessageProcessorError> {
        let mut priority_ops = Vec::new();
        for msg in msgs {
            if let FullInscriptionMessage::L1ToL2Message(l1_to_l2_msg) = msg {
                if let Some((l1_tx, tx_id)) = create_l1_tx_from_message(&l1_to_l2_msg)
                    .map_err(|e| MessageProcessorError::Internal(e))?
                {
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

                    priority_ops.push((l1_tx, tx_id));
                } else {
                    tracing::warn!("Invalid deposit, l1 tx_id {}", &l1_to_l2_msg.common.tx_id);
                }
            }
        }

        if priority_ops.is_empty() {
            return Ok(false);
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

        Ok(true)
    }
}

impl L1ToL2MessageProcessor {}
