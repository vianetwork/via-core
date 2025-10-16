use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
use via_consensus::consensus::BATCH_FINALIZATION_THRESHOLD;
use via_verifier_dal::{Connection, Verifier, VerifierDal};

use super::{convert_txid_to_h256, MessageProcessor, MessageProcessorError};
use crate::metrics::{InscriptionStage, METRICS};

#[derive(Default, Debug)]
pub struct VerifierMessageProcessor {}

#[async_trait::async_trait]
impl MessageProcessor for VerifierMessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        for msg in msgs {
            match msg {
                FullInscriptionMessage::ProofDAReference(ref proof_msg) => {
                    let proof_reveal_tx_id = convert_txid_to_h256(proof_msg.common.tx_id);

                    if storage
                        .via_votes_dal()
                        .proof_reveal_tx_exists(proof_reveal_tx_id.as_bytes())
                        .await?
                    {
                        tracing::info!(
                            "Skipping duplicate proof reveal tx: {:?}",
                            proof_reveal_tx_id
                        );
                        continue;
                    }

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

                    let new_l1_batch_number = l1_batch_da_ref_inscription.input.l1_batch_index.0;

                    tracing::info!(
                        "Processing ProofDAReference for batch {} with hash {:?}",
                        new_l1_batch_number,
                        l1_batch_da_ref_inscription.input.l1_batch_hash
                    );

                    if new_l1_batch_number == 0 {
                        tracing::info!(
                            "Skipping ProofDAReference message with l1_batch_number ZERO."
                        );
                        continue;
                    } else if new_l1_batch_number == 1 {
                        if storage.via_votes_dal().batch_exists(1).await? {
                            tracing::info!("Skipping duplicate genesis batch 1");
                            continue;
                        }
                    } else if new_l1_batch_number > 1 {
                        let last_batch_in_canonical_chain = match storage
                            .via_votes_dal()
                            .get_last_batch_in_canonical_chain()
                            .await?
                        {
                            Some(last_batch_in_canonical_chain) => last_batch_in_canonical_chain,
                            None => {
                                return Err(MessageProcessorError::Internal(anyhow::Error::msg(
                                    "Last batch in canonical chain not found",
                                )))
                            }
                        };

                        if last_batch_in_canonical_chain.0 + 1 != new_l1_batch_number {
                            // Possible reorg: validate whether the batch is a fork of a previously reverted batch.
                            // If this batch is valid (i.e., a fork of a previously valid batch),
                            // the verifier treats it as a new fork, implicitly marking all earlier batches as invalid.
                            let parent_hash = l1_batch_da_ref_inscription
                                .input
                                .prev_l1_batch_hash
                                .as_bytes()
                                .to_vec();

                            let exists = storage
                                .via_votes_dal()
                                .get_parent_batch_exists_for_l1_batch(
                                    new_l1_batch_number as i64,
                                    &parent_hash,
                                )
                                .await?;
                            if !exists {
                                tracing::info!(
                                    "Skipping ProofDAReference, no valid parent found for the new l1_batch_number: {:?}",
                                    new_l1_batch_number,
                                );
                                continue;
                            }

                            tracing::info!(
                                "A Revert batch was found with number {}, parent hash {}",
                                new_l1_batch_number,
                                l1_batch_da_ref_inscription.input.prev_l1_batch_hash
                            );

                            let from_l1_batch_number = (new_l1_batch_number - 1) as i64;

                            let mut transaction = storage.start_transaction().await?;
                            transaction
                                .via_votes_dal()
                                .delete_votable_transactions(from_l1_batch_number)
                                .await?;

                            transaction
                                .via_transactions_dal()
                                .reset_transactions(from_l1_batch_number)
                                .await?;

                            transaction.commit().await?;
                        } else {
                            if last_batch_in_canonical_chain.1
                                != l1_batch_da_ref_inscription.input.prev_l1_batch_hash.0
                            {
                                tracing::info!(
                                "Skipping ProofDAReference message with l1_batch_number: {:?}. Last batch in canonical chain: {:?}",
                                l1_batch_da_ref_inscription.input.l1_batch_index,
                                last_batch_in_canonical_chain
                            );
                                continue;
                            }
                        }
                    }

                    METRICS.inscriptions_processed[&InscriptionStage::IndexedL1Batch]
                        .set(new_l1_batch_number as usize);

                    storage
                        .via_votes_dal()
                        .insert_votable_transaction(
                            new_l1_batch_number,
                            l1_batch_da_ref_inscription.input.l1_batch_hash,
                            l1_batch_da_ref_inscription.input.prev_l1_batch_hash,
                            proof_msg.input.da_identifier.clone(),
                            proof_reveal_tx_id,
                            proof_msg.input.blob_id.clone(),
                            proof_msg.input.l1_batch_reveal_txid.to_string(),
                            l1_batch_da_ref_inscription.input.blob_id,
                        )
                        .await?;

                    tracing::info!(
                        "New votable transaction for L1 batch {:?}",
                        new_l1_batch_number
                    );
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
                            .get_votable_transaction_id(&reveal_proof_txid.as_bytes())
                            .await?
                        {
                            let p2wpkh_address =
                                attestation_msg.common.p2wpkh_address.as_ref().expect(
                                    "ValidatorAttestation message must have a p2wpkh address",
                                );

                            let mut transaction = storage.start_transaction().await?;

                            transaction
                                .via_votes_dal()
                                .insert_vote(
                                    votable_transaction_id,
                                    &p2wpkh_address.to_string(),
                                    is_ok,
                                )
                                .await?;

                            tracing::info!("New vote found for L1 batch {:?}", l1_batch_number);

                            METRICS.inscriptions_processed[&InscriptionStage::Vote]
                                .set(l1_batch_number.0 as usize);

                            // Check finalization
                            if transaction
                                .via_votes_dal()
                                .finalize_transaction_if_needed(
                                    votable_transaction_id,
                                    BATCH_FINALIZATION_THRESHOLD,
                                    indexer.get_number_of_verifiers(),
                                )
                                .await?
                            {
                                METRICS
                                    .last_finalized_l1_batch
                                    .set(l1_batch_number.0 as usize);
                                tracing::info!(
                                        "Finalizing transaction with tx_id: {:?} and block number: {:?}",
                                        tx_id,
                                        l1_batch_number
                                );
                            }

                            transaction.commit().await?
                        }
                    }
                }
                _ => (),
            }
        }
        Ok(None)
    }
}
