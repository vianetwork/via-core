use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
use via_verifier_dal::{Connection, Verifier, VerifierDal};

use super::{convert_txid_to_h256, MessageProcessor, MessageProcessorError};

#[derive(Debug)]
pub struct VerifierMessageProcessor {
    zk_agreement_threshold: f64,
}

impl VerifierMessageProcessor {
    pub fn new(zk_agreement_threshold: f64) -> Self {
        Self {
            zk_agreement_threshold,
        }
    }
}

#[async_trait::async_trait]
impl MessageProcessor for VerifierMessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<(), MessageProcessorError> {
        for msg in msgs {
            match msg {
                ref f @ FullInscriptionMessage::ProofDAReference(ref proof_msg) => {
                    if let Some(l1_batch_number) = indexer.get_l1_batch_number(f).await {
                        let mut votes_dal = storage.via_votes_dal();

                        let last_finilized_l1_batch = votes_dal
                            .get_last_finalized_l1_batch()
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?
                            .unwrap_or(0);

                        if l1_batch_number.0 < last_finilized_l1_batch {
                            tracing::info!(
                                "Skipping ProofDAReference message with l1_batch_number: {:?}. Last inserted block: {:?}",
                                l1_batch_number, last_finilized_l1_batch
                            );
                            continue;
                        }

                        let proof_reveal_tx_id = convert_txid_to_h256(proof_msg.common.tx_id);

                        let pubdata_msgs = indexer
                            .parse_transaction(&proof_msg.input.l1_batch_reveal_txid)
                            .await?;

                        if pubdata_msgs.len() != 1 {
                            return Err(MessageProcessorError::Internal(anyhow::Error::msg(
                                "Invalid pubdata msg lenght",
                            )));
                        }

                        let inscription = pubdata_msgs[0].clone();

                        let l1_batch_da_ref_inscription = match inscription {
                            FullInscriptionMessage::L1BatchDAReference(da_msg) => da_msg,
                            _ => {
                                return Err(MessageProcessorError::Internal(anyhow::Error::msg(
                                    "Invalid inscription type",
                                )))
                            }
                        };

                        votes_dal
                            .insert_votable_transaction(
                                l1_batch_number.0,
                                l1_batch_da_ref_inscription.input.l1_batch_hash,
                                l1_batch_da_ref_inscription.input.prev_l1_batch_hash,
                                proof_msg.input.da_identifier.clone(),
                                proof_reveal_tx_id,
                                proof_msg.input.blob_id.clone(),
                                proof_msg.input.l1_batch_reveal_txid.to_string(),
                                l1_batch_da_ref_inscription.input.blob_id,
                            )
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

                        tracing::info!(
                            "New votable transaction for L1 batch {:?}",
                            l1_batch_number
                        );
                        continue;
                    } else {
                        tracing::warn!(
                            "L1BatchNumber not found for ProofDAReference message : {:?}",
                            proof_msg
                        );
                    }
                }
                ref f @ FullInscriptionMessage::ValidatorAttestation(ref attestation_msg) => {
                    if let Some(l1_batch_number) = indexer.get_l1_batch_number(f).await {
                        let reveal_proof_txid =
                            convert_txid_to_h256(attestation_msg.input.reference_txid);
                        let tx_id = convert_txid_to_h256(attestation_msg.common.tx_id);

                        // Vote = true if attestation_msg.input.attestation == Vote::Ok
                        let is_ok = matches!(
                            attestation_msg.input.attestation,
                            via_btc_client::types::Vote::Ok
                        );

                        if let Some(votable_transaction_id) = storage
                            .via_votes_dal()
                            .get_votable_transaction_id(reveal_proof_txid)
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?
                        {
                            let p2wpkh_address =
                                attestation_msg.common.p2wpkh_address.as_ref().expect(
                                    "ValidatorAttestation message must have a p2wpkh address",
                                );

                            let mut transaction = storage
                                .start_transaction()
                                .await
                                .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

                            transaction
                                .via_votes_dal()
                                .insert_vote(
                                    votable_transaction_id,
                                    &p2wpkh_address.to_string(),
                                    is_ok,
                                )
                                .await
                                .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

                            tracing::info!("New vote found for L1 batch {:?}", l1_batch_number);

                            // Check finalization
                            if transaction
                                .via_votes_dal()
                                .finalize_transaction_if_needed(
                                    votable_transaction_id,
                                    self.zk_agreement_threshold,
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

                            transaction
                                .commit()
                                .await
                                .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?
                        }
                    }
                }
                // bootstrapping phase is already covered
                FullInscriptionMessage::SystemContractUpgrade(_)
                | FullInscriptionMessage::ProposeSequencer(_)
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
