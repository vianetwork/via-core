use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinSecp256k1::hashes::Hash, FullInscriptionMessage},
};
use via_verifier_dal::{Connection, Verifier, VerifierDal};
use zksync_types::H256;

use super::{MessageProcessor, MessageProcessorError};
use crate::metrics::{InscriptionStage, METRICS};

#[derive(Debug)]
pub struct WithdrawalProcessor;

impl WithdrawalProcessor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl MessageProcessor for WithdrawalProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<bool, MessageProcessorError> {
        for msg in msgs {
            if let FullInscriptionMessage::BridgeWithdrawal(withdrawal_msg) = msg {
                tracing::info!("Processing withdrawal bridge transaction...");

                let mut proof_reveal_tx_id_bytes =
                    withdrawal_msg.input.l1_batch_proof_reveal_tx_id.clone();
                proof_reveal_tx_id_bytes.reverse();
                let proof_reveal_tx_id = H256::from_slice(&proof_reveal_tx_id_bytes);
                let indexed_bridge_tx_id = withdrawal_msg
                    .common
                    .tx_id
                    .as_raw_hash()
                    .to_byte_array()
                    .to_vec();

                let Some((votable_tx_id, l1_batch_number, bridge_tx_id)) = storage
                    .via_votes_dal()
                    .get_vote_transaction_info(
                        proof_reveal_tx_id.clone(),
                        withdrawal_msg.input.index_withdrawal,
                    )
                    .await
                    .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?
                else {
                    tracing::warn!(
                        "Votable transaction for proof_reveal_tx_id {} not found",
                        proof_reveal_tx_id.clone()
                    );
                    continue;
                };

                if let Some(existing_tx_id) = bridge_tx_id {
                    if existing_tx_id == indexed_bridge_tx_id {
                        tracing::info!(
                            "Withdrawal already processed for batch {}. Skipping.",
                            l1_batch_number
                        );
                        continue;
                    }

                    return Err(MessageProcessorError::SyncError(format!(
                        "Multiple withdrawals detected for L1 batch {}",
                        l1_batch_number
                    )));
                }

                storage
                    .via_bridge_dal()
                    .update_bridge_tx(
                        votable_tx_id,
                        withdrawal_msg.input.index_withdrawal,
                        &indexed_bridge_tx_id,
                    )
                    .await
                    .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

                tracing::info!(
                    "Marked withdrawal for L1 batch {} as processed",
                    l1_batch_number
                );
                METRICS.inscriptions_processed[&InscriptionStage::Withdrawal]
                    .set(l1_batch_number as usize);
            }
        }

        Ok(true)
    }
}
