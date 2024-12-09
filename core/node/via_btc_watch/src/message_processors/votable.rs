use via_btc_client::types::{BitcoinTxid, FullInscriptionMessage};
use zksync_dal::{Connection, Core, CoreDal};
use zksync_types::H256;

use super::{MessageProcessor, MessageProcessorError};

#[derive(Debug)]
pub struct VotableMessageProcessor {
    verifier_count: usize,
    threshold: f64,
}

impl VotableMessageProcessor {
    pub fn new(verifier_count: usize) -> Self {
        Self {
            verifier_count,
            threshold: 0.5,
        }
    }
}

#[async_trait::async_trait]
impl MessageProcessor for VotableMessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
    ) -> Result<(), MessageProcessorError> {
        let mut votes_dal = storage.via_votes_dal();

        for msg in msgs {
            match msg {
                FullInscriptionMessage::L1BatchDAReference(da_msg) => {
                    let tx_id = convert_txid_to_h256(da_msg.common.tx_id);
                    votes_dal
                        .insert_votable_transaction(tx_id, "L1BatchDAReference")
                        .await
                        .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;
                }
                FullInscriptionMessage::ProofDAReference(proof_msg) => {
                    let tx_id = convert_txid_to_h256(proof_msg.common.tx_id);
                    votes_dal
                        .insert_votable_transaction(tx_id, "ProofDAReference")
                        .await
                        .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;
                }
                FullInscriptionMessage::ValidatorAttestation(attestation_msg) => {
                    let reference_txid = convert_txid_to_h256(attestation_msg.input.reference_txid);
                    // Vote = true if attestation_msg.input.attestation == Vote::Ok
                    let is_ok = matches!(
                        attestation_msg.input.attestation,
                        via_btc_client::types::Vote::Ok
                    );
                    votes_dal
                        .insert_vote(
                            reference_txid,
                            &attestation_msg.common.p2wpkh_address.to_string(),
                            is_ok,
                        )
                        .await
                        .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

                    // Check finalization
                    votes_dal
                        .finalize_transaction_if_needed(reference_txid, self.threshold)
                        .await
                        .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;
                }
                // bootstrapping phase is already covered
                FullInscriptionMessage::ProposeSequencer(_) => {
                    continue;
                }
                // Non-votable messages like SystemBootstrapping or L1ToL2Message are ignored by this processor
                FullInscriptionMessage::SystemBootstrapping(_)
                | FullInscriptionMessage::L1ToL2Message(_) => {
                    // do nothing
                }
            }
        }
        Ok(())
    }
}

fn convert_txid_to_h256(txid: BitcoinTxid) -> H256 {
    let mut tx_id_bytes = txid.as_raw_hash()[..].to_vec();
    tx_id_bytes.reverse();
    H256::from_slice(&tx_id_bytes)
}
