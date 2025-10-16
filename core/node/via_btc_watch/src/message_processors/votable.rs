use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
use via_consensus::consensus::BATCH_FINALIZATION_THRESHOLD;
use zksync_dal::{Connection, Core, CoreDal};

use super::{convert_txid_to_h256, MessageProcessor, MessageProcessorError};
use crate::metrics::{InscriptionStage, METRICS};

#[derive(Default, Debug)]
pub struct VotableMessageProcessor {}

#[async_trait::async_trait]
impl MessageProcessor for VotableMessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        for msg in msgs {
            match msg {
                ref f @ FullInscriptionMessage::ValidatorAttestation(ref attestation_msg) => {
                    if let Some(l1_batch_number) = indexer.get_l1_batch_number(f).await {
                        let proof_reveal_txid = attestation_msg.input.reference_txid[..].to_vec();

                        // Vote = true if attestation_msg.input.attestation == Vote::Ok
                        let is_ok = matches!(
                            attestation_msg.input.attestation,
                            via_btc_client::types::Vote::Ok
                        );

                        if !storage
                            .via_blocks_dal()
                            .l1_batch_proof_tx_exists(l1_batch_number.0 as i64, &proof_reveal_txid)
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?
                        {
                            tracing::warn!(
                                "Invalid verifier attestation, reveal txid not found for the l1 batch number: {:?} proof_reveal_txid: {:?}",
                                l1_batch_number,
                                &proof_reveal_txid,
                            );
                            continue;
                        }

                        let p2wpkh_address = attestation_msg
                            .common
                            .p2wpkh_address
                            .as_ref()
                            .expect("ValidatorAttestation message must have a p2wpkh address");

                        let mut transaction = storage
                            .start_transaction()
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

                        transaction
                            .via_votes_dal()
                            .insert_vote(
                                l1_batch_number.0,
                                &proof_reveal_txid,
                                &p2wpkh_address.to_string(),
                                is_ok,
                            )
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

                        tracing::info!("New vote found for L1 batch {:?}", l1_batch_number);

                        METRICS.inscriptions_processed[&InscriptionStage::Vote]
                            .set(l1_batch_number.0 as usize);
                        // Check finalization
                        if transaction
                            .via_votes_dal()
                            .finalize_transaction_if_needed(
                                l1_batch_number.0,
                                BATCH_FINALIZATION_THRESHOLD,
                                indexer.get_number_of_verifiers(),
                            )
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?
                        {
                            tracing::info!(
                                "Finalizing transaction with tx_id: {:?} and block number: {:?}",
                                convert_txid_to_h256(attestation_msg.common.tx_id),
                                l1_batch_number
                            );
                        }

                        transaction
                            .commit()
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;
                    }
                }
                _ => (),
            }
        }
        Ok(None)
    }
}
