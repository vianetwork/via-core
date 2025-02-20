use std::str::FromStr;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{
        BitcoinNetwork, BitcoinTxid, FullInscriptionMessage, L1BatchDAReference, NodeAuth,
        ProofDAReference,
    },
    utils::bytes_to_txid,
};
use via_da_client::{pubdata::Pubdata, types::L2_BOOTLOADER_CONTRACT_ADDR};
use via_verification::proof::{
    Bn256, ProofTrait, ViaZKProof, ZkSyncProof, ZkSyncSnarkWrapperCircuit,
};
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use zksync_config::ViaVerifierConfig;
use zksync_da_client::{types::InclusionData, DataAvailabilityClient};
use zksync_types::{
    commitment::L1BatchWithMetadata, protocol_version::ProtocolSemanticVersion, H160, H256,
};

/// Copy of `zksync_l1_contract_interface::i_executor::methods::ProveBatches`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProveBatches {
    pub prev_l1_batch: L1BatchWithMetadata,
    pub l1_batches: Vec<L1BatchWithMetadata>,
    pub proofs: Vec<L1BatchProofForL1>,
    pub should_verify: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L1BatchProofForL1 {
    pub aggregation_result_coords: [[u8; 32]; 4],
    pub scheduler_proof: ZkSyncProof<Bn256, ZkSyncSnarkWrapperCircuit>,
    pub protocol_version: ProtocolSemanticVersion,
}

#[derive(Debug)]
pub struct ViaVerifier {
    pool: ConnectionPool<Verifier>,
    da_client: Box<dyn DataAvailabilityClient>,
    indexer: BitcoinInscriptionIndexer,
    config: ViaVerifierConfig,
}

impl ViaVerifier {
    pub async fn new(
        rpc_url: &str,
        network: BitcoinNetwork,
        node_auth: NodeAuth,
        bootstrap_txids: Vec<BitcoinTxid>,
        pool: ConnectionPool<Verifier>,
        client: Box<dyn DataAvailabilityClient>,
        config: ViaVerifierConfig,
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
        let mut timer = tokio::time::interval(self.config.polling_interval());
        let pool = self.pool.clone();

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            let mut storage = pool.connection_tagged("via_zk_verifier").await?;
            match self.loop_iteration(&mut storage).await {
                Ok(()) => {}
                Err(err) => tracing::error!("Failed to process via_zk_verifier: {err}"),
            }
        }

        tracing::info!("Stop signal received, via_zk_verifier is shutting down");
        Ok(())
    }

    pub async fn loop_iteration(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<()> {
        if let Some((l1_batch_number, mut raw_tx_id)) = storage
            .via_votes_dal()
            .get_first_not_verified_block()
            .await?
        {
            let db_raw_tx_id = H256::from_slice(&raw_tx_id);
            tracing::info!("New non executed block ready to be processed");

            raw_tx_id.reverse();
            let proof_txid = bytes_to_txid(&raw_tx_id).context("Failed to parse tx_id")?;
            tracing::info!("trying to get proof_txid: {}", proof_txid);
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

            tracing::info!(
                "Fetch l1 batch pubdata for blob id  {}",
                batch_da.input.blob_id
            );

            let (batch_blob, batch_hash) = self.process_batch_da_reference(batch_da).await?;

            let mut is_verified = self
                .verify_op_priority_id(storage, l1_batch_number, &batch_blob.data)
                .await?;

            if is_verified {
                is_verified = self.verify_proof(batch_hash, &proof_blob.data).await?;
            }

            storage
                .via_votes_dal()
                .verify_votable_transaction(l1_batch_number as u32, db_raw_tx_id, is_verified)
                .await?;
        }

        Ok(())
    }

    pub async fn verify_op_priority_id(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        l1_batch_number: i64,
        pubdata: &[u8],
    ) -> anyhow::Result<bool> {
        let pubdata = Pubdata::decode_pubdata(pubdata.to_vec())?;
        let mut deposit_logs = Vec::new();

        for log in &pubdata.user_logs {
            if log.sender == H160::from_str(L2_BOOTLOADER_CONTRACT_ADDR)? {
                deposit_logs.push(log);
            }
        }

        let txs = storage
            .via_transactions_dal()
            .list_transactions_not_processed(deposit_logs.len() as i64)
            .await?;

        if txs.len() != deposit_logs.len() {
            tracing::error!(
                "Verifier did not index all the deposits, expected {} found {}",
                deposit_logs.len(),
                txs.len()
            );
            return Ok(false);
        }

        if txs.is_empty() {
            tracing::error!("There is no transactions to validate the op priority id",);
            return Ok(true);
        }

        for (raw_tx_id, deposit_log) in txs.iter().zip(deposit_logs.iter()) {
            let db_raw_tx_id = H256::from_slice(raw_tx_id);
            if db_raw_tx_id != deposit_log.key {
                tracing::error!(
                    "Sequencer did not process the deposit transactions in series for l1 batch {}, \
                    invalid priority id for transaction hash {}", 
                    l1_batch_number,
                    db_raw_tx_id
                );
                return Ok(false);
            }

            let status = !deposit_log.value.is_zero();
            storage
                .via_transactions_dal()
                .update_transaction(&deposit_log.key, status)
                .await?;
        }

        tracing::info!(
            "Priority_id verified successfuly for l1 batch {}",
            l1_batch_number
        );

        Ok(true)
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

    async fn verify_proof(&self, batch_hash: H256, proof_bytes: &[u8]) -> anyhow::Result<bool> {
        tracing::info!(
            ?batch_hash,
            proof_len = proof_bytes.len(),
            "Verifying proof"
        );
        let proof_data: ProveBatches = bincode::deserialize(proof_bytes)?;

        if proof_data.l1_batches.len() != 1 {
            tracing::error!(
                "Expected exactly one L1Batch and one proof, got {} and {}",
                proof_data.l1_batches.len(),
                proof_data.proofs.len()
            );
            return Ok(false);
        }

        // TODO: decide if we need to verify the batch data (already have batch data from ProofDAReference inscription)
        // let batch: PubData... = bincode::deserialize(&batch)
        //     .context("Failed to deserialize L1BatchWithMetadata")?;

        let protocol_version = proof_data.l1_batches[0]
            .header
            .protocol_version
            .unwrap()
            .to_string();

        if !proof_data.should_verify {
            tracing::info!(
                "Proof verification is disabled for proof with batch number : {:?}",
                proof_data.l1_batches[0].header.number
            );
            tracing::info!(
                "Verifying proof with protocol version: {}",
                protocol_version
            );
            tracing::info!("Skipping verification");
            Ok(true)
        } else {
            if proof_data.proofs.len() != 1 {
                tracing::error!(
                    "Expected exactly one proof, got {}",
                    proof_data.proofs.len()
                );
                return Ok(false);
            }

            let (prev_commitment, curr_commitment) = (
                proof_data.prev_l1_batch.metadata.commitment,
                proof_data.l1_batches[0].metadata.commitment,
            );
            let mut proof = proof_data.proofs[0].scheduler_proof.clone();

            // Put correct inputs
            proof.inputs = via_verification::public_inputs::generate_inputs(
                &prev_commitment,
                &curr_commitment,
            );

            // Verify the proof
            let via_proof = ViaZKProof { proof };
            let vk_inner =
                via_verification::utils::load_verification_key_without_l1_check(protocol_version)
                    .await?;

            let is_valid = via_proof.verify(vk_inner)?;

            tracing::info!("Proof verification result: {}", is_valid);

            Ok(is_valid)
        }
    }
}
