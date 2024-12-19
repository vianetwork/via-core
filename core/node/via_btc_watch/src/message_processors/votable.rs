use sqlx::types::chrono::{DateTime, Utc};
use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinTxid, FullInscriptionMessage},
};
use zksync_dal::{Connection, Core, CoreDal};
use zksync_types::{aggregated_operations::AggregatedActionType, L1BatchNumber, H256};

use super::{MessageProcessor, MessageProcessorError};

const DEFAULT_THRESHOLD: f64 = 0.5;

#[derive(Debug)]
pub struct VotableMessageProcessor {
    threshold: f64,
}

impl VotableMessageProcessor {
    pub fn new() -> Self {
        Self {
            threshold: DEFAULT_THRESHOLD,
        }
    }
}

#[async_trait::async_trait]
impl MessageProcessor for VotableMessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<(), MessageProcessorError> {
        // Get the current timestamp
        let dt = Utc::now();
        let naive_utc = dt.naive_utc();
        let offset = dt.offset().clone();
        let dt = DateTime::<Utc>::from_naive_utc_and_offset(naive_utc, offset);

        for msg in msgs {
            if let Some(l1_batch_number) = indexer.get_l1_batch_number(&msg).await {
                match msg {
                    FullInscriptionMessage::ProofDAReference(proof_msg) => {
                        let tx_id = convert_txid_to_h256(proof_msg.common.tx_id);
                        let mut votes_dal = storage.via_votes_dal();

                        votes_dal
                            .insert_votable_transaction(l1_batch_number.0, tx_id)
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;

                        let mut eth_sender_dal = storage.eth_sender_dal();

                        eth_sender_dal
                            .insert_bogus_confirmed_eth_tx(
                                l1_batch_number,
                                AggregatedActionType::PublishProofOnchain,
                                tx_id,
                                dt,
                            )
                            .await?;
                    }
                    FullInscriptionMessage::ValidatorAttestation(attestation_msg) => {
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
                            )
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?
                        {
                            let mut eth_sender_dal = storage.eth_sender_dal();

                            eth_sender_dal
                                .insert_bogus_confirmed_eth_tx(
                                    l1_batch_number,
                                    AggregatedActionType::Execute,
                                    tx_id,
                                    dt,
                                )
                                .await?;
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
            } else {
                tracing::warn!("L1 batch number is not found for message: {:?}", msg);
                continue;
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
