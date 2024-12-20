use anyhow::Context;
use bitcoin::{hashes::Hash, Txid};
use tokio::sync::watch;
use via_btc_client::{
    traits::Serializable,
    types::{InscriptionMessage, ValidatorAttestationInput, Vote},
};
use zksync_config::ViaBtcSenderConfig;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_types::{
    via_verifier_btc_inscription_operations::ViaVerifierBtcInscriptionRequestType, L1BatchNumber,
};

#[derive(Debug)]
pub struct ViaVoteInscription {
    pool: ConnectionPool<Core>,
    config: ViaBtcSenderConfig,
}

impl ViaVoteInscription {
    pub async fn new(
        pool: ConnectionPool<Core>,
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
                Ok(()) => {
                    tracing::info!("Verifier vote inscription task finished");
                }
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
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<()> {
        if let Some((l1_batch_number, vote, tx_id)) = self.get_voting_operation(storage).await? {
            tracing::info!("New voting operation ready to be processed");
            let mut transaction = storage.start_transaction().await?;
            let inscription_message = self.construct_voting_inscription_message(vote, tx_id)?;

            let inscription_request = transaction
                .btc_sender_dal()
                .via_save_btc_inscriptions_request(
                    ViaVerifierBtcInscriptionRequestType::VoteOnchain.to_string(),
                    InscriptionMessage::to_bytes(&inscription_message),
                    0,
                )
                .await
                .context("Via save btc inscriptions request")?;

            transaction
                .via_verifier_block_dal()
                .insert_vote_l1_batch_inscription_request_id(
                    l1_batch_number,
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
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<Option<(L1BatchNumber, bool, Vec<u8>)>> {
        if let Some(batch_number) = storage
            .via_votes_dal()
            .get_first_not_finilized_block()
            .await?
        {
            // Check if already created a voting inscription
            let exists = storage
                .via_verifier_block_dal()
                .check_vote_l1_batch_inscription_request_if_exists(batch_number)
                .await?;
            if exists {
                return Ok(None);
            }

            if let Some((vote, tx_id)) = storage
                .via_votes_dal()
                .get_verifier_vote_status(batch_number)
                .await?
            {
                return Ok(Some((
                    L1BatchNumber::from(batch_number as u32),
                    vote,
                    tx_id,
                )));
            }
        }
        Ok(None)
    }

    pub fn construct_voting_inscription_message(
        &self,
        vote: bool,
        tx_id: Vec<u8>,
    ) -> anyhow::Result<InscriptionMessage> {
        let mut attestation = Vote::NotOk;

        if vote {
            attestation = Vote::Ok;
        }
        let input = ValidatorAttestationInput {
            reference_txid: Txid::from_slice(&tx_id)?,
            attestation,
        };
        return Ok(InscriptionMessage::ValidatorAttestation(input));
    }
}
