use via_btc_client::{
    indexer::{replay::semantic_payload, BitcoinInscriptionIndexer},
    types::FullInscriptionMessage,
};
use zksync_dal::{Connection, Core, CoreDal};

use super::{MessageProcessor, MessageProcessorError};

#[derive(Default,Debug)]
pub struct VotableMessageProcessor {}

#[async_trait::async_trait]
impl MessageProcessor for VotableMessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        messages: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        for message in messages {
            match &message {
                FullInscriptionMessage::ProofDAReference(proof) => {
                    let batch = indexer
                        .parse_transaction(&proof.input.l1_batch_reveal_txid)
                        .await
                        .map_err(anyhow::Error::from)?;
                    let [batch_message @ FullInscriptionMessage::L1BatchDAReference(batch)] =
                        batch.as_slice()
                    else {
                        return Err(anyhow::anyhow!(
                            "proof must reference exactly one immutable batch message"
                        )
                        .into());
                    };
                    let payload = serde_json::json!({
                        "proof":semantic_payload(&message).map_err(anyhow::Error::from)?,
                        "batch":semantic_payload(batch_message).map_err(anyhow::Error::from)?,
                    });
                    storage
                        .via_votes_dal()
                        .record_reader_proof(
                            proof.common.tx_id,
                            batch.input.l1_batch_index.0,
                            &payload,
                            proof.common.inclusion().map_err(anyhow::Error::from)?,
                        )
                        .await?;
                }
                // jscpd:ignore-start Raw vote evidence is committed by each concrete role’s source owner.
                FullInscriptionMessage::ValidatorAttestation(vote) => {
                    let verifier = vote.common.p2wpkh_address.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("vote is missing its authenticated verifier")
                    })?;
                    storage
                        .via_votes_dal()
                        .record_reader_vote(
                            vote.common.tx_id,
                            vote.input.reference_txid,
                            &verifier.to_string(),
                            matches!(vote.input.attestation, via_btc_client::types::Vote::Ok),
                            &semantic_payload(&message).map_err(anyhow::Error::from)?,
                            vote.common.inclusion().map_err(anyhow::Error::from)?,
                        )
                        .await?;
                }
                // jscpd:ignore-end
                _ => {}
            }
        }
        Ok(None)
    }
}
