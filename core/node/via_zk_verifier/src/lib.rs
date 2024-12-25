use anyhow::Context;
use tokio::sync::watch;
use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{
        BitcoinNetwork, BitcoinTxid, FullInscriptionMessage, L1BatchDAReference, NodeAuth,
        ProofDAReference,
    },
    utils::bytes_to_txid,
};
use zksync_config::ViaBtcWatchConfig;
use zksync_da_client::{types::InclusionData, DataAvailabilityClient};
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_prover_interface::outputs::L1BatchProofForL1;
use zksync_types::{commitment::L1BatchWithMetadata, H256};

/// Copy of `zksync_l1_contract_interface::i_executor::methods::ProveBatches`
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProveBatches {
    pub prev_l1_batch: L1BatchWithMetadata,
    pub l1_batches: Vec<L1BatchWithMetadata>,
    pub proofs: Vec<L1BatchProofForL1>,
    pub should_verify: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct L1BatchData {
    pub pubdata: Vec<u8>,
    pub l1_batch_number: u32,
}

#[derive(Debug)]
pub struct ViaVerifier {
    pool: ConnectionPool<Core>,
    da_client: Box<dyn DataAvailabilityClient>,
    indexer: BitcoinInscriptionIndexer,
    // TODO: Add config
    config: ViaBtcWatchConfig,
}

impl ViaVerifier {
    pub async fn new(
        rpc_url: &str,
        network: BitcoinNetwork,
        node_auth: NodeAuth,
        bootstrap_txids: Vec<BitcoinTxid>,
        pool: ConnectionPool<Core>,
        client: Box<dyn DataAvailabilityClient>,
        config: ViaBtcWatchConfig,
    ) -> anyhow::Result<Self> {
        let indexer =
            BitcoinInscriptionIndexer::new(rpc_url, network, node_auth, bootstrap_txids).await?;
        Ok(Self {
            pool,
            da_client: client,
            indexer,
            config,
        })
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.config.poll_interval());
        let pool = self.pool.clone();

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            let mut storage = pool.connection_tagged("via_zk_verifier").await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => tracing::info!(""),
                Err(err) => tracing::error!("Failed to process via_zk_verifier: {err}"),
            }
        }

        tracing::info!("Stop signal received, via_zk_verifier is shutting down");
        Ok(())
    }

    pub async fn loop_iteration(
        &mut self,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<()> {
        if let Some((l1_batch_number, mut raw_tx_id)) = storage
            .via_votes_dal()
            .get_first_not_executed_block()
            .await?
        {
            let db_raw_tx_id = H256::from_slice(&raw_tx_id);
            tracing::info!("New non executed block ready to be processed");

            raw_tx_id.reverse();
            let proof_txid = bytes_to_txid(&raw_tx_id).context("Failed to parse tx_id")?;
            tracing::warn!("trying to get proof_txid: {}", proof_txid);
            let proof_msgs = self.indexer.parse_transaction(&proof_txid).await?;
            let proof_msg = self.expect_single_msg(&proof_msgs, "ProofDAReference")?;

            let proof_da = match proof_msg {
                FullInscriptionMessage::ProofDAReference(ref a) => a,
                _ => {
                    tracing::error!("Expected ProofDAReference, got something else");
                    return Ok(());
                }
            };

            let (proof_blob, batch_tx_id) = self.process_proof_da_reference(proof_da).await?;

            let batch_msgs = self.indexer.parse_transaction(&batch_tx_id).await?;
            let batch_msg = self.expect_single_msg(&batch_msgs, "L1BatchDAReference")?;

            let batch_da = match batch_msg {
                FullInscriptionMessage::L1BatchDAReference(ref a) => a,
                _ => {
                    tracing::error!("Expected L1BatchDAReference, got something else");
                    return Ok(());
                }
            };

            let (batch_blob, batch_hash) = self.process_batch_da_reference(batch_da).await?;

            let is_verified = self
                .verify_proof(batch_hash, proof_blob.data, batch_blob.data)
                .await?;

            storage
                .via_votes_dal()
                .mark_transaction_executed(l1_batch_number, db_raw_tx_id)
                .await?;
            storage
                .via_votes_dal()
                .verify_votable_transaction(l1_batch_number as u32, db_raw_tx_id, is_verified)
                .await?;
        }

        Ok(())
    }

    /// Helper to ensure there's exactly one message in the array, or log an error.
    fn expect_single_msg<'a>(
        &self,
        msgs: &'a [FullInscriptionMessage],
        expected_type: &str,
    ) -> anyhow::Result<&'a FullInscriptionMessage> {
        match msgs.len() {
            1 => Ok(&msgs[0]),
            n => {
                tracing::error!("Expected 1 {expected_type} message, got {n}");
                Err(anyhow::anyhow!("Expected exactly 1 message, got {n}"))
            }
        }
    }

    /// Processes a `ProofDAReference` message by retrieving the DA blob
    async fn process_proof_da_reference(
        &mut self,
        proof_msg: &ProofDAReference,
    ) -> anyhow::Result<(InclusionData, BitcoinTxid)> {
        let blob = self
            .da_client
            .get_inclusion_data(&proof_msg.input.blob_id)
            .await
            .context("Failed to get blob")?
            .ok_or_else(|| anyhow::anyhow!("Blob not found"))?;
        let batch_tx_id = proof_msg.input.l1_batch_reveal_txid;

        Ok((blob, batch_tx_id))
    }

    /// Processes an `L1BatchDAReference` message by retrieving the DA blob
    async fn process_batch_da_reference(
        &mut self,
        batch_msg: &L1BatchDAReference,
    ) -> anyhow::Result<(InclusionData, H256)> {
        let blob = self
            .da_client
            .get_inclusion_data(&batch_msg.input.blob_id)
            .await
            .context("Failed to get blob")?
            .ok_or_else(|| anyhow::anyhow!("Blob not found"))?;
        let hash = batch_msg.input.l1_batch_hash;

        Ok((blob, hash))
    }

    async fn verify_proof(
        &self,
        batch_hash: H256,
        proof: Vec<u8>,
        batch: Vec<u8>,
    ) -> anyhow::Result<bool> {
        tracing::info!(
            ?batch_hash,
            proof_len = proof.len(),
            batch_len = batch.len(),
            "Verifying proof"
        );
        let proof: ProveBatches = bincode::deserialize(&proof)?;

        // TODO: fetch and verify pubdata
        // let batch: L1BatchData = bincode::deserialize(&batch)
        //     .context("Failed to deserialize L1BatchWithMetadata")?;

        if !proof.should_verify {
            Ok(true)
        } else {
            // TODO: Verify the proof
            Ok(false)
        }
    }
}
