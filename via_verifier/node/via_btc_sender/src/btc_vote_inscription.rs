use anyhow::Context;
use bitcoin::{hashes::Hash, Txid};
use via_btc_client::{
    traits::{BitcoinOps, Serializable},
    types::{InscriptionMessage, ValidatorAttestationInput, Vote},
};
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use zksync_types::via_verifier_btc_inscription_operations::ViaVerifierBtcInscriptionRequestType;

pub async fn create_request(
    pool: &ConnectionPool<Verifier>,
    client: &dyn BitcoinOps,
    voter: &bitcoin::Address,
) -> anyhow::Result<()> {
    let mut transaction = via_btc_storage::transition::begin_authority(pool, client, &[]).await?;
    if let Some((votable_transaction_id, vote, tx_id)) =
        transaction.via_votes_dal().pending_reader_vote().await?
    {
        let inscription_message = construct_voting_inscription_message(vote, tx_id)?;

        via_btc_storage::ReaderDal::new(&mut transaction)
            .authorize_vote_message(&inscription_message, voter)
            .await?;
            let inscription_request = transaction
                .via_btc_sender_dal()
                .via_save_btc_inscriptions_request(
                    ViaVerifierBtcInscriptionRequestType::VoteOnchain.to_string(),
                    InscriptionMessage::to_bytes(&inscription_message),
                    0,
                )
                .await?;

            transaction
                .via_block_dal()
                .insert_vote_l1_batch_inscription_request_id(
                    votable_transaction_id,
                    inscription_request.id,
                    ViaVerifierBtcInscriptionRequestType::VoteOnchain,
                )
                .await?;
            transaction.commit().await?;
    }
    Ok(())
}

pub fn construct_voting_inscription_message(
    vote: bool,
    tx_id: Vec<u8>,
) -> anyhow::Result<InscriptionMessage> {
        let attestation = if vote { Vote::Ok } else { Vote::NotOk };

        // Convert H256 bytes to Txid
    let txid = h256_to_txid(&tx_id)?;

        let input = ValidatorAttestationInput { reference_txid: txid, attestation };
        Ok(InscriptionMessage::ValidatorAttestation(input))
    }

/// Converts H256 bytes (from the DB) to a Txid by reversing the byte order.
fn h256_to_txid(h256_bytes: &[u8]) -> anyhow::Result<Txid> {
        if h256_bytes.len() != 32 {
            return Err(anyhow::anyhow!("H256 must be 32 bytes"));
        }
        let mut reversed_bytes = h256_bytes.to_vec();
        reversed_bytes.reverse();
        Txid::from_slice(&reversed_bytes).with_context(|| "Failed to convert H256 to Txid")
    }
