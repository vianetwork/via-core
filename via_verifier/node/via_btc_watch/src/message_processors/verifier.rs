use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
use via_verifier_dal::{Connection, Verifier, VerifierDal};

use super::{convert_txid_to_h256, MessageProcessor, MessageProcessorError, ProofContinuityGap};
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
                    let source_hash = proof_msg.common.block_hash.ok_or_else(|| {
                        MessageProcessorError::Internal(anyhow::anyhow!(
                            "ProofDAReference is missing its scanned Bitcoin block hash"
                        ))
                    })?;
                    let proof_reveal_tx_id = convert_txid_to_h256(proof_msg.common.tx_id);

                    let pubdata_msgs = indexer.parse_transaction(&proof_msg.input.l1_batch_reveal_txid).await?;

                    if pubdata_msgs.len() != 1 {
                        return Err(MessageProcessorError::Internal(anyhow::Error::msg("Invalid pubdata msg length")));
                    }

                    let inscription = pubdata_msgs[0].clone();

                    let l1_batch_da_ref_inscription = match inscription {
                        FullInscriptionMessage::L1BatchDAReference(da_msg) => da_msg,
                        _ => {
                            return Err(MessageProcessorError::Internal(anyhow::Error::msg("Invalid inscription type")))
                        }
                    };

                    let new_l1_batch_number = l1_batch_da_ref_inscription.input.l1_batch_index.0;
                    let payload=serde_json::json!({
                        "proof":via_btc_client::indexer::replay::semantic_payload(&msg).map_err(anyhow::Error::from)?,
                        "batch":via_btc_client::indexer::replay::semantic_payload(&pubdata_msgs[0]).map_err(anyhow::Error::from)?,
                    });
                    let reader_inclusion_id=storage.via_votes_dal().record_reader_proof(
                        proof_msg.common.tx_id,new_l1_batch_number,&payload,
                        proof_msg.common.inclusion().map_err(anyhow::Error::from)?,
                    ).await?;
                    tracing::info!(
                        "Processing ProofDAReference for batch {} with hash {:?}",
                        new_l1_batch_number,
                        l1_batch_da_ref_inscription.input.l1_batch_hash
                    );
                    storage.via_votes_dal().record_votable_transaction(
                        new_l1_batch_number,l1_batch_da_ref_inscription.input.l1_batch_hash,
                        l1_batch_da_ref_inscription.input.prev_l1_batch_hash,proof_msg.input.da_identifier.clone(),
                        proof_reveal_tx_id,proof_msg.input.blob_id.clone(),proof_msg.input.l1_batch_reveal_txid.to_string(),
                        l1_batch_da_ref_inscription.input.blob_id.clone(),reader_inclusion_id,
                        via_verifier_dal::via_votes_dal::ProofMembership::RetainedOnly,
                    ).await?;

                    match storage.via_votes_dal().admit_recorded_proof(
                        reader_inclusion_id, proof_reveal_tx_id, new_l1_batch_number,
                        l1_batch_da_ref_inscription.input.prev_l1_batch_hash,
                    ).await? {
                        via_verifier_dal::via_votes_dal::ProofAdmission::Ignored => continue,
                        via_verifier_dal::via_votes_dal::ProofAdmission::ContinuityGap { expected_batch, canonical_parent } => {
                            return Err(MessageProcessorError::ProofContinuityGap(Box::new(ProofContinuityGap {
                                expected_batch, canonical_parent,
                                observed_batch: new_l1_batch_number,
                                observed_parent: l1_batch_da_ref_inscription.input.prev_l1_batch_hash,
                                proof_txid: proof_reveal_tx_id,
                                source_height: proof_msg.common.block_height,
                                source_hash,
                            })));
                        }
                        via_verifier_dal::via_votes_dal::ProofAdmission::Admitted { rewound_to } => {
                            if let Some(retained) = rewound_to {
                                METRICS.inscriptions_processed[&InscriptionStage::Reorg].set(retained as usize);
                            }
                            METRICS.inscriptions_processed[&InscriptionStage::IndexedL1Batch].set(new_l1_batch_number as usize);
                        }
                    }

                    tracing::info!("New votable transaction for L1 batch {:?}", new_l1_batch_number);
                }
                // jscpd:ignore-start Raw vote evidence is committed by each concrete role’s source owner.
                FullInscriptionMessage::ValidatorAttestation(ref vote) => {
                    let verifier=vote.common.p2wpkh_address.as_ref()
                        .ok_or_else(|| anyhow::anyhow!("vote is missing its authenticated verifier"))?;
                    storage.via_votes_dal().record_reader_vote(
                        vote.common.tx_id,vote.input.reference_txid,&verifier.to_string(),
                        matches!(vote.input.attestation,via_btc_client::types::Vote::Ok),
                        &via_btc_client::indexer::replay::semantic_payload(&msg).map_err(anyhow::Error::from)?,
                        vote.common.inclusion().map_err(anyhow::Error::from)?,
                    ).await?;
                }
                // jscpd:ignore-end
                _ => (),
            }
        }
        Ok(None)
    }
}
