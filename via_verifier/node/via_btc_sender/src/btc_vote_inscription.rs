use anyhow::Context;
use bitcoin::{hashes::Hash, Txid};
use tokio::sync::watch;
use via_btc_client::{
    traits::Serializable,
    types::{InscriptionMessage, ValidatorAttestationInput, Vote},
};
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use zksync_config::ViaBtcSenderConfig;
use zksync_types::via_verifier_btc_inscription_operations::ViaVerifierBtcInscriptionRequestType;

#[derive(Debug)]
pub struct ViaVoteInscription {
    pool: ConnectionPool<Verifier>,
    config: ViaBtcSenderConfig,
}

impl ViaVoteInscription {
    pub async fn new(
        pool: ConnectionPool<Verifier>,
        config: ViaBtcSenderConfig,
    ) -> anyhow::Result<Self> {
        Ok(Self { pool, config })
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.config.poll_interval());
        let pool = self.pool.clone();

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            let mut storage = pool
                .connection_tagged("via_btc_inscription_creator")
                .await?;

            match self.loop_iteration(&mut storage).await {
                Ok(()) => {}
                Err(err) => {
                    tracing::error!("Failed to process Verifier btc_sender_inscription: {err}");
                }
            }
        }

        tracing::info!("Stop signal received, Verifier btc_sender_inscription is shutting down");
        Ok(())
    }

    pub async fn loop_iteration(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<()> {
        if let Some((votable_transaction_id, vote, tx_id)) =
            self.get_voting_operation(storage).await?
        {
            tracing::info!("New voting operation ready to be processed");
            let mut transaction = storage.start_transaction().await?;
            let inscription_message = self.construct_voting_inscription_message(vote, tx_id)?;

            let inscription_request = transaction
                .via_btc_sender_dal()
                .via_save_btc_inscriptions_request(
                    ViaVerifierBtcInscriptionRequestType::VoteOnchain.to_string(),
                    InscriptionMessage::to_bytes(&inscription_message),
                    0,
                )
                .await
                .context("Via save btc inscriptions request")?;

            transaction
                .via_block_dal()
                .insert_vote_l1_batch_inscription_request_id(
                    votable_transaction_id,
                    inscription_request.id,
                    ViaVerifierBtcInscriptionRequestType::VoteOnchain,
                )
                .await
                .context("Via set inscription request id")?;
            transaction.commit().await?;
        }
        Ok(())
    }

    pub async fn get_voting_operation(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<Option<(i64, bool, Vec<u8>)>> {
        if let Some(votable_transaction_id) = storage
            .via_votes_dal()
            .get_first_non_finalized_l1_batch_in_canonical_inscription_chain()
            .await?
        {
            // Check if already created a voting inscription
            if storage
                .via_block_dal()
                .check_vote_l1_batch_inscription_request_if_exists(votable_transaction_id)
                .await?
            {
                return Ok(None);
            }

            if let Some((vote, tx_id)) = storage
                .via_votes_dal()
                .get_verifier_vote_status(votable_transaction_id)
                .await?
            {
                return Ok(Some((votable_transaction_id, vote, tx_id)));
            }
        }
        Ok(None)
    }

    pub fn construct_voting_inscription_message(
        &self,
        vote: bool,
        tx_id: Vec<u8>,
    ) -> anyhow::Result<InscriptionMessage> {
        let attestation = if vote { Vote::Ok } else { Vote::NotOk };

        // Convert H256 bytes to Txid
        let txid = Self::h256_to_txid(&tx_id)?;

        let input = ValidatorAttestationInput {
            reference_txid: txid,
            attestation,
        };
        Ok(InscriptionMessage::ValidatorAttestation(input))
    }

    /// Converts H256 bytes (from the DB) to a Txid by reversing the byte order.
    fn h256_to_txid(h256_bytes: &[u8]) -> anyhow::Result<Txid> {
        if h256_bytes.len() != 32 {
            return Err(anyhow::anyhow!("H256 must be 32 bytes"));
        }
        let mut reversed_bytes = h256_bytes.to_vec();
        reversed_bytes.reverse();
        Txid::from_slice(&reversed_bytes).context("Failed to convert H256 to Txid")
    }
}
