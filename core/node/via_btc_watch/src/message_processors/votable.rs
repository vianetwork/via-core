use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinTxid, FullInscriptionMessage},
};
use via_consensus::consensus::BATCH_FINALIZATION_THRESHOLD;
use zksync_dal::{Connection, Core, CoreDal};
use zksync_types::L1BatchNumber;

use super::{convert_txid_to_h256, load_tx_locator, MessageProcessor, MessageProcessorError};
use crate::metrics::{InscriptionStage, METRICS};

#[derive(Default, Debug)]
pub struct VotableMessageProcessor {}

#[async_trait::async_trait]
impl MessageProcessor for VotableMessageProcessor {
    async fn process_messages(
        &mut self, storage: &mut Connection<'_, Core>, msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        for msg in msgs {
            match msg {
                FullInscriptionMessage::ValidatorAttestation(ref attestation_msg) => {
                    if let Some(l1_batch_number) =
                        resolve_l1_batch_number(storage, indexer, &attestation_msg.input.reference_txid).await?
                    {
                        let proof_reveal_txid = attestation_msg.input.reference_txid[..].to_vec();

                        // Vote = true if attestation_msg.input.attestation == Vote::Ok
                        let is_ok = matches!(attestation_msg.input.attestation, via_btc_client::types::Vote::Ok);

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
                            .insert_vote(l1_batch_number.0, &proof_reveal_txid, &p2wpkh_address.to_string(), is_ok)
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

                        tracing::info!("New vote found for L1 batch {:?}", l1_batch_number);

                        METRICS.inscriptions_processed[&InscriptionStage::Vote].set(l1_batch_number.0 as usize);
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

                        transaction.commit().await.map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;
                    }
                }
                _ => (),
            }
        }
        Ok(None)
    }
}

async fn resolve_l1_batch_number(
    storage: &mut Connection<'_, Core>, indexer: &mut BitcoinInscriptionIndexer, proof_txid: &BitcoinTxid,
) -> Result<Option<L1BatchNumber>, MessageProcessorError> {
    let Some(proof_locator) = load_tx_locator(storage, proof_txid).await? else {
        tracing::warn!("Skipping verifier attestation, missing Bitcoin locator for proof txid {}", proof_txid);
        return Ok(None);
    };

    let proof_msgs = indexer.parse_transaction_with_locator(&proof_locator).await?;
    let Some(proof_msg) = proof_msgs.iter().find_map(|msg| match msg {
        FullInscriptionMessage::ProofDAReference(proof_msg) => Some(proof_msg),
        _ => None,
    }) else {
        tracing::warn!("Skipping verifier attestation, proof txid {} did not parse as ProofDAReference", proof_txid);
        return Ok(None);
    };

    let Some(pubdata_locator) = load_tx_locator(storage, &proof_msg.input.l1_batch_reveal_txid).await? else {
        tracing::warn!(
            "Skipping verifier attestation, missing Bitcoin locator for pubdata txid {}",
            proof_msg.input.l1_batch_reveal_txid
        );
        return Ok(None);
    };

    let pubdata_msgs = indexer.parse_transaction_with_locator(&pubdata_locator).await?;
    let Some(da_msg) = pubdata_msgs.iter().find_map(|msg| match msg {
        FullInscriptionMessage::L1BatchDAReference(da_msg) => Some(da_msg),
        _ => None,
    }) else {
        tracing::warn!(
            "Skipping verifier attestation, pubdata txid {} did not parse as L1BatchDAReference",
            proof_msg.input.l1_batch_reveal_txid
        );
        return Ok(None);
    };

    Ok(Some(da_msg.input.l1_batch_index))
}
