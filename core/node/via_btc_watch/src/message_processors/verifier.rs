use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
use zksync_dal::{Connection, Core, CoreDal};

use super::{convert_txid_to_h256, MessageProcessor, MessageProcessorError};

#[derive(Debug)]
pub struct VerifierMessageProcessor {
    threshold: f64,
}

impl VerifierMessageProcessor {
    pub fn new(threshold: f64) -> Self {
        Self { threshold }
    }
}

#[async_trait::async_trait]
impl MessageProcessor for VerifierMessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<(), MessageProcessorError> {
        for msg in msgs {
            match msg {
                ref f @ FullInscriptionMessage::ProofDAReference(ref proof_msg) => {
                    if let Some(l1_batch_number) = indexer.get_l1_batch_number(&f).await {
                        let mut votes_dal = storage.via_votes_dal();

                        let last_inserted_block = votes_dal
                            .get_last_inserted_block()
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?
                            .unwrap_or(0);

                        if l1_batch_number.0 != last_inserted_block + 1 {
                            tracing::warn!(
                                "Skipping ProofDAReference message with l1_batch_number: {:?}. Last inserted block: {:?}",
                                l1_batch_number, last_inserted_block
                            );
                            continue;
                        }

                        let tx_id = convert_txid_to_h256(proof_msg.common.tx_id);

                        votes_dal
                            .insert_votable_transaction(l1_batch_number.0, tx_id)
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;
                    } else {
                        tracing::warn!(
                            "L1BatchNumber not found for ProofDAReference message : {:?}",
                            proof_msg
                        );
                    }
                }
                ref f @ FullInscriptionMessage::ValidatorAttestation(ref attestation_msg) => {
                    if let Some(l1_batch_number) = indexer.get_l1_batch_number(&f).await {
                        let mut votes_dal = storage.via_votes_dal();

                        let reference_txid =
                            convert_txid_to_h256(attestation_msg.input.reference_txid);
                        let tx_id = convert_txid_to_h256(attestation_msg.common.tx_id);

                        // Vote = true if attestation_msg.input.attestation == Vote::Ok
                        let is_ok = matches!(
                            attestation_msg.input.attestation,
                            via_btc_client::types::Vote::Ok
                        );
                        votes_dal
                            .insert_vote(
                                l1_batch_number.0,
                                reference_txid,
                                &attestation_msg.common.p2wpkh_address.to_string(),
                                is_ok,
                            )
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

                        // Check finalization
                        if votes_dal
                            .finalize_transaction_if_needed(
                                l1_batch_number.0,
                                reference_txid,
                                self.threshold,
                                indexer.get_number_of_verifiers(),
                            )
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?
                        {
                            tracing::info!(
                                "Finalizing transaction with tx_id: {:?} and block number: {:?}",
                                tx_id,
                                l1_batch_number
                            );
                        }
                    }
                }
                // bootstrapping phase is already covered
                FullInscriptionMessage::ProposeSequencer(_)
                | FullInscriptionMessage::SystemBootstrapping(_) => {
                    // do nothing
                }
                // Non-votable messages like L1BatchDAReference or L1ToL2Message are ignored by this processor
                FullInscriptionMessage::L1ToL2Message(_)
                | FullInscriptionMessage::L1BatchDAReference(_) => {
                    // do nothing
                }
            }
        }
        Ok(())
    }
}
