use std::{str::FromStr, sync::Arc};

use anyhow::Context;
use metrics::METRICS;
use tokio::sync::watch;
use via_btc_client::{
    client::BitcoinClient,
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinTxid, FullInscriptionMessage, L1BatchDAReference, ProofDAReference},
    utils::bytes_to_txid,
};
use via_consensus::consensus::BATCH_FINALIZATION_THRESHOLD;
use via_da_client::{pubdata::Pubdata, types::L2_BOOTLOADER_CONTRACT_ADDR};
use via_verification::{decode_prove_batch_data, verify_proof, ProveBatchData};
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use via_verifier_state::sync::ViaState;
use via_verifier_types::protocol_version::check_if_supported_sequencer_version;
use zksync_config::{ViaBtcWatchConfig, ViaVerifierConfig};
use zksync_da_client::{types::InclusionData, DataAvailabilityClient};
use zksync_object_store::ObjectStore;
use zksync_types::{via_wallet::SystemWallets, L1BatchNumber, H160, H256};

mod metrics;

#[derive(Debug)]
pub struct ViaVerifier {
    config: ViaVerifierConfig,
    pool: ConnectionPool<Verifier>,
    da_client: Box<dyn DataAvailabilityClient>,
    indexer: BitcoinInscriptionIndexer,
    state: ViaState,
    blob_store: Arc<dyn ObjectStore>,
}

impl ViaVerifier {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        config: ViaVerifierConfig,
        indexer: BitcoinInscriptionIndexer,
        pool: ConnectionPool<Verifier>,
        da_client: Box<dyn DataAvailabilityClient>,
        btc_client: Arc<BitcoinClient>,
        via_btc_watch_config: ViaBtcWatchConfig,
        blob_store: Arc<dyn ObjectStore>,
    ) -> anyhow::Result<Self> {
        let state = ViaState::new(pool.clone(), btc_client.clone(), via_btc_watch_config);

        Ok(Self {
            config: config.clone(),
            pool,
            da_client,
            indexer,
            state,
            blob_store,
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
                Err(err) => {
                    METRICS.errors.inc();
                    tracing::error!("Failed to process via_zk_verifier: {err}")
                }
            }
        }

        tracing::info!("Stop signal received, via_zk_verifier is shutting down");
        Ok(())
    }

    pub async fn loop_iteration(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<()> {
        if self.state.is_reorg_in_progress().await? {
            return Ok(());
        }

        if self.state.is_sync_in_progress().await? {
            return Ok(());
        }

        if let Some((l1_batch_number, mut raw_tx_id)) = storage
            .via_votes_dal()
            .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
            .await?
        {
            let latency = METRICS.verification_time.start();
            let db_raw_tx_id = H256::from_slice(&raw_tx_id);
            tracing::info!("New non executed l1_batch {l1_batch_number}, ready to be processed");

            raw_tx_id.reverse();
            let proof_txid = bytes_to_txid(&raw_tx_id).with_context(|| "Failed to parse tx_id")?;
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

            let (batch_blob, _) = self.process_batch_da_reference(batch_da).await?;
            let mut pubdata = Pubdata::decode_pubdata(batch_blob.data.clone().to_vec())?;

            let upgrade_tx_hash_opt = self.verify_upgrade_tx_hash(storage, &pubdata).await?;

            let mut protocol_version = storage
                .via_protocol_versions_dal()
                .latest_protocol_semantic_version()
                .await
                .expect("Failed to load the latest protocol semantic version")
                .ok_or_else(|| anyhow::anyhow!("Protocol version is missing"))?;

            if upgrade_tx_hash_opt.is_some() {
                // Discard the first log since it related to protocol upgrade.
                pubdata.user_logs.remove(0);

                protocol_version = storage
                    .via_protocol_versions_dal()
                    .latest_semantic_version()
                    .await
                    .expect("Failed to load the latest protocol semantic version")
                    .ok_or_else(|| anyhow::anyhow!("Protocol version is missing"))?;

                // Check if the new protocol version is supported by the verifier node.
                check_if_supported_sequencer_version(protocol_version)?;
            }

            let (mut is_verified, deposits) = self
                .verify_op_priority_id(storage, l1_batch_number, &pubdata)
                .await?;

            if is_verified {
                tracing::info!("Successfully verified the op priority id");

                let mut prover_batch_data =
                    decode_prove_batch_data(protocol_version.minor, &proof_blob.data)
                        .with_context(|| {
                            format!(
                                "Failed to decode prove batch data for l1 batch {l1_batch_number}"
                            )
                        })?;

                // Check if proofs vector is empty
                let should_fetch_proof = match &prover_batch_data {
                    ProveBatchData::V27(data) => data.proofs.is_empty() && data.should_verify,
                    ProveBatchData::V28(data) => data.proofs.is_empty() && data.should_verify,
                };

                // Fetch the proof from DA in the case of using EN as DA provider.
                if should_fetch_proof {
                    tracing::info!("Proof missing from DA, fetching from blob store");

                    match &mut prover_batch_data {
                        ProveBatchData::V27(data) => {
                            let proof = self
                                .blob_store
                                .get((L1BatchNumber(l1_batch_number as u32), protocol_version))
                                .await
                                .map_err(|e| anyhow::anyhow!("Error fetching the proof: {e}"))?;

                            data.proofs.push(proof)
                        }
                        ProveBatchData::V28(data) => {
                            let proof = self
                                .blob_store
                                .get((L1BatchNumber(l1_batch_number as u32), protocol_version))
                                .await
                                .map_err(|e| anyhow::anyhow!("Error fetching the proof: {e}"))?;

                            data.proofs.push(proof)
                        }
                    };
                }

                is_verified = verify_proof(prover_batch_data).await?;

                tracing::info!(
                    "Proof verification result for l1 batch {}: {}",
                    l1_batch_number,
                    is_verified
                );
            }
            let mut transaction = storage.start_transaction().await?;

            let votable_transaction_id = transaction
                .via_votes_dal()
                .verify_votable_transaction(l1_batch_number, db_raw_tx_id, is_verified)
                .await?;

            transaction
                .via_votes_dal()
                .finalize_transaction_if_needed(
                    votable_transaction_id,
                    BATCH_FINALIZATION_THRESHOLD,
                    self.indexer.get_number_of_verifiers(),
                )
                .await?;

            if is_verified {
                // Update the transaction status only if the l1 batch is valid.
                for (hash, status) in deposits {
                    transaction
                        .via_transactions_dal()
                        .update_transaction(&hash, status, l1_batch_number)
                        .await?;
                }

                transaction
                    .via_votes_dal()
                    .delete_invalid_votable_transactions_if_exists()
                    .await?;

                if let Some(upgrade_tx_hash) = upgrade_tx_hash_opt {
                    transaction
                        .via_protocol_versions_dal()
                        .mark_upgrade_as_executed(upgrade_tx_hash.as_bytes())
                        .await?;
                }

                METRICS.last_valid_l1_batch.set(l1_batch_number as usize);
            } else {
                METRICS.last_invalid_l1_batch.set(l1_batch_number as usize);
            }

            // Before commit the verification make sure that no reorg was detected during he ZK verification.
            if transaction
                .via_l1_block_dal()
                .has_reorg_in_progress()
                .await?
                .is_some()
            {
                return Ok(());
            }

            transaction.commit().await?;

            latency.observe();

            tracing::info!("Zk verification finished");
        }

        Ok(())
    }

    /// Check whether the first user_log corresponds to an upgrade transaction.
    pub async fn verify_upgrade_tx_hash(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        pubdata: &Pubdata,
    ) -> anyhow::Result<Option<H256>> {
        if let Some(upgrade_tx_hash) = storage
            .via_protocol_versions_dal()
            .get_in_progress_upgrade_tx_hash()
            .await?
        {
            if let Some(log) = pubdata.user_logs.first() {
                if log.sender == H160::from_str(L2_BOOTLOADER_CONTRACT_ADDR)?
                    && log.key == upgrade_tx_hash
                {
                    tracing::info!("Found upgrade transaction in pubdata: {}", upgrade_tx_hash);
                    return Ok(Some(upgrade_tx_hash));
                }
            }
            return Ok(None);
        }
        Ok(None)
    }

    pub async fn verify_op_priority_id(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        l1_batch_number: i64,
        pubdata: &Pubdata,
    ) -> anyhow::Result<(bool, Vec<(H256, bool)>)> {
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
                txs.len(),
                deposit_logs.len(),
            );
            return Ok((false, vec![]));
        }

        if txs.is_empty() {
            tracing::info!("There is no transactions to validate the op priority id",);
            return Ok((true, vec![]));
        }

        let mut deposits: Vec<(H256, bool)> = Vec::new();

        for (raw_tx_id, deposit_log) in txs.iter().zip(deposit_logs.iter()) {
            let db_raw_tx_id = H256::from_slice(raw_tx_id);
            if db_raw_tx_id != deposit_log.key {
                tracing::error!(
                    "Sequencer did not process the deposit transactions in series for l1 batch {}, \
                    invalid priority id for transaction hash {}", 
                    l1_batch_number,
                    db_raw_tx_id
                );
                return Ok((false, vec![]));
            }

            let status = !deposit_log.value.is_zero();
            deposits.push((deposit_log.key, status));
        }

        tracing::info!(
            "Priority_id verified successfully for l1 batch {}",
            l1_batch_number
        );

        Ok((true, deposits))
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
            .with_context(|| "Failed to fetch the blob")?
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
            .with_context(|| "Failed to fetch the blob")?
            .ok_or_else(|| anyhow::anyhow!("Blob not found"))?;
        let hash = batch_msg.input.l1_batch_hash;

        Ok((blob, hash))
    }
}
