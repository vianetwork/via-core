use std::{future::Future, sync::Arc, time::Duration};

use anyhow::Context;
use chrono::Utc;
use rand::Rng;
use tokio::sync::watch::Receiver;
use zksync_config::DADispatcherConfig;
use zksync_da_client::{
    types::{DAError, InclusionData},
    DataAvailabilityClient,
};
use zksync_dal::{ConnectionPool, Core, CoreDal};
use zksync_l1_contract_interface::{i_executor::methods::ProveBatches, Tokenize};
use zksync_object_store::{ObjectStore, ObjectStoreError};
use zksync_prover_interface::outputs::L1BatchProofForL1;
use zksync_types::{ethabi::Token, protocol_version::ProtocolSemanticVersion, L1BatchNumber};

use crate::metrics::METRICS;

#[derive(Debug)]
pub struct ViaDataAvailabilityDispatcher {
    client: Box<dyn DataAvailabilityClient>,
    pool: ConnectionPool<Core>,
    config: DADispatcherConfig,
    blob_store: Arc<dyn ObjectStore>,
    dispatch_real_proof: bool,
}

impl ViaDataAvailabilityDispatcher {
    pub fn new(
        pool: ConnectionPool<Core>,
        config: DADispatcherConfig,
        client: Box<dyn DataAvailabilityClient>,
        blob_store: Arc<dyn ObjectStore>,
        dispatch_real_proof: bool,
    ) -> Self {
        Self {
            pool,
            config,
            client,
            blob_store,
            dispatch_real_proof,
        }
    }

    pub async fn run(self, mut stop_receiver: Receiver<bool>) -> anyhow::Result<()> {
        loop {
            if *stop_receiver.borrow() {
                break;
            }

            // Run dispatch, poll_for_inclusion, and dispatch_proofs concurrently
            let subtasks = futures::future::join3(
                async {
                    if let Err(err) = self.dispatch().await {
                        tracing::error!("dispatch error {err:?}");
                    }
                },
                async {
                    if let Err(err) = self.poll_for_inclusion().await {
                        tracing::error!("poll_for_inclusion error {err:?}");
                    }
                },
                async {
                    if let Err(err) = self.dispatch_proofs().await {
                        tracing::error!("dispatch_proofs error {err:?}");
                    }
                },
            );

            tokio::select! {
                _ = subtasks => {},
                _ = stop_receiver.changed() => {
                    break;
                }
            }

            if tokio::time::timeout(self.config.polling_interval(), stop_receiver.changed())
                .await
                .is_ok()
            {
                break;
            }
        }

        tracing::info!("Stop signal received, da_dispatcher is shutting down");
        Ok(())
    }

    /// Dispatches the blobs to the data availability layer, and saves the blob_id in the database.
    async fn dispatch(&self) -> anyhow::Result<()> {
        let mut conn = self.pool.connection_tagged("via_da_dispatcher").await?;
        let batches = conn
            .via_data_availability_dal()
            .get_ready_for_da_dispatch_l1_batches(self.config.max_rows_to_dispatch() as usize)
            .await?;
        drop(conn);

        for batch in batches {
            let dispatch_latency = METRICS.blob_dispatch_latency.start();

            let dispatch_response = retry(self.config.max_retries(), batch.l1_batch_number, || {
                self.client
                    .dispatch_blob(batch.l1_batch_number.0, batch.pubdata.clone())
            })
            .await
            .with_context(|| {
                format!(
                    "failed to dispatch a blob with batch_number: {}, pubdata_len: {}",
                    batch.l1_batch_number,
                    batch.pubdata.len()
                )
            })?;
            let dispatch_latency_duration = dispatch_latency.observe();

            let sent_at = Utc::now().naive_utc();

            let mut conn = self.pool.connection_tagged("da_dispatcher").await?;
            conn.via_data_availability_dal()
                .insert_l1_batch_da(
                    batch.l1_batch_number,
                    dispatch_response.blob_id.as_str(),
                    sent_at,
                )
                .await?;
            drop(conn);

            METRICS
                .last_dispatched_l1_batch
                .set(batch.l1_batch_number.0 as usize);
            METRICS.blob_size.observe(batch.pubdata.len());
            tracing::info!(
                "Dispatched a DA for batch_number: {}, pubdata_size: {}, dispatch_latency: {dispatch_latency_duration:?}",
                batch.l1_batch_number,
                batch.pubdata.len(),
            );
        }

        Ok(())
    }

    async fn dispatch_proofs(&self) -> anyhow::Result<()> {
        match self.dispatch_real_proof {
            true => self.dispatch_real_proofs().await?,
            false => self.dispatch_dummy_proofs().await?,
        }
        Ok(())
    }

    async fn dispatch_dummy_proofs(&self) -> anyhow::Result<()> {
        let mut conn = self.pool.connection_tagged("da_dispatcher").await?;

        let batches = conn
            .via_data_availability_dal()
            .get_ready_for_dummy_proof_dispatch_l1_batches(
                self.config.max_rows_to_dispatch() as usize
            )
            .await?;

        drop(conn);

        for batch in batches {
            let dispatch_latency = METRICS.blob_dispatch_latency.start();

            let dummy_proof = self
                .prepare_dummy_proof_operation(batch)
                .await
                .with_context(|| {
                    format!(
                        "failed to prepare a dummy proof for batch_number: {}",
                        batch
                    )
                })?;

            let dispatch_response = retry(self.config.max_retries(), batch, || {
                self.client.dispatch_blob(batch.0, dummy_proof.clone())
            })
            .await?;

            let dispatch_latency_duration = dispatch_latency.observe();

            let sent_at = Utc::now().naive_utc();

            let mut conn = self.pool.connection_tagged("da_dispatcher").await?;
            conn.via_data_availability_dal()
                .insert_proof_da(batch, dispatch_response.blob_id.as_str(), sent_at)
                .await?;

            METRICS.last_dispatched_proof_batch.set(batch.0 as usize);
            METRICS.blob_size.observe(dummy_proof.len());
            tracing::info!(
                "Dispatched a dummy proof for batch_number: {}, proof_size: {}, dispatch_latency: {dispatch_latency_duration:?}",
                batch,
                dummy_proof.len(),
            );
        }

        Ok(())
    }

    // TODO: fix dal logic for loading real proofs
    /// Dispatches proofs to the data availability layer, and saves the blob_id in the database.
    async fn dispatch_real_proofs(&self) -> anyhow::Result<()> {
        let mut conn = self.pool.connection_tagged("da_dispatcher").await?;

        tracing::error!("dispatch_real_proofs started");
        let proofs = conn
            .via_data_availability_dal()
            .get_ready_for_da_dispatch_proofs(self.config.max_rows_to_dispatch() as usize)
            .await?;

        tracing::error!("dispatch_real_proofs got proofs : {:?}", proofs);
        drop(conn);

        for proof in proofs {
            // fetch the proof from object store
            tracing::error!("dispatch_real_proofs proof : {:?}", proof);
            let proof_data = match self.load_real_proof_operation(proof.l1_batch_number).await {
                Some(proof) => proof,
                None => {
                    tracing::error!("Failed to load proof for batch {}", proof.l1_batch_number.0);
                    continue;
                }
            };

            tracing::error!("proof_data : {:?}", proof_data);

            let serelize_proof = proof_data.into_tokens();

            tracing::error!("serelize_proof : {:?}", serelize_proof);

            // concatenate all bytes
            let final_proof = flatten_tokens(&serelize_proof);

            tracing::error!("final_proof : {:?}", final_proof);

            let dispatch_latency = METRICS.proof_dispatch_latency.start();

            let dispatch_response = retry(self.config.max_retries(), proof.l1_batch_number, || {
                self.client
                    .dispatch_blob(proof.l1_batch_number.0, final_proof.clone())
            })
            .await
            .with_context(|| {
                format!(
                    "failed to dispatch a proof with batch_number: {}, proof_len: {}",
                    proof.l1_batch_number,
                    final_proof.len()
                )
            })?;

            let dispatch_latency_duration = dispatch_latency.observe();

            let sent_at = Utc::now().naive_utc();

            let mut conn = self.pool.connection_tagged("da_dispatcher").await?;
            conn.via_data_availability_dal()
                .insert_proof_da(
                    proof.l1_batch_number,
                    dispatch_response.blob_id.as_str(),
                    sent_at,
                )
                .await?;
            drop(conn);

            METRICS
                .last_dispatched_proof_batch
                .set(proof.l1_batch_number.0 as usize);
            METRICS.blob_size.observe(final_proof.len());
            tracing::info!(
                "Dispatched a proof for batch_number: {}, proof_size: {}, dispatch_latency: {dispatch_latency_duration:?}",
                proof.l1_batch_number,
                final_proof.len(),
            );
        }
        Ok(())
    }

    /// Loads a real proof operation for a given L1 batch number.
    async fn load_real_proof_operation(
        &self,
        batch_to_prove: L1BatchNumber,
    ) -> Option<ProveBatches> {
        let mut storage = self.pool.connection_tagged("da_dispatcher").await.ok()?;

        tracing::error!("load_real_proof_operation started");
        let previous_proven_batch_number =
            match storage.blocks_dal().get_last_l1_batch_with_prove_tx().await {
                Ok(batch_number) => batch_number,
                Err(e) => {
                    tracing::error!("Failed to retrieve the last L1 batch with proof tx: {}", e);
                    return None;
                }
            };

        tracing::error!(
            "previous_proven_batch_number : {:?}",
            previous_proven_batch_number
        );
        let minor_version = match storage
            .blocks_dal()
            .get_batch_protocol_version_id(batch_to_prove)
            .await
        {
            Ok(Some(version)) => version,
            Ok(None) | Err(_) => {
                tracing::error!(
                    "Failed to retrieve protocol version for batch {}",
                    batch_to_prove
                );
                return None;
            }
        };

        tracing::error!("minor version : {:?}", minor_version);

        let latest_semantic_version = match storage
            .protocol_versions_dal()
            .latest_semantic_version()
            .await
        {
            Ok(Some(version)) => version,
            Ok(None) | Err(_) => {
                tracing::error!("Failed to retrieve the latest semantic version");
                return None;
            }
        };

        tracing::error!("latest_semantic_version : {:?}", latest_semantic_version);

        let l1_verifier_config = storage
            .protocol_versions_dal()
            .l1_verifier_config_for_version(latest_semantic_version)
            .await?;

        tracing::error!("l1 verifier config: {:?}", l1_verifier_config);

        let allowed_patch_versions = match storage
            .protocol_versions_dal()
            .get_patch_versions_for_vk(
                minor_version,
                l1_verifier_config.recursion_scheduler_level_vk_hash,
            )
            .await
        {
            Ok(versions) if !versions.is_empty() => versions,
            Ok(_) | Err(_) => {
                tracing::warn!(
                    "No patch version corresponds to the verification key on L1: {:?}",
                    l1_verifier_config.recursion_scheduler_level_vk_hash
                );
                return None;
            }
        };

        let allowed_versions: Vec<_> = allowed_patch_versions
            .into_iter()
            .map(|patch| ProtocolSemanticVersion {
                minor: minor_version,
                patch,
            })
            .collect();

        tracing::error!("allowed_versions : {:?}", allowed_versions);

        let proof = match self
            .load_wrapped_fri_proofs_for_range(batch_to_prove, &allowed_versions)
            .await
        {
            Some(proof) => proof,
            None => {
                tracing::error!("Failed to load proof for batch {}", batch_to_prove);
                return None;
            }
        };

        tracing::error!("proof : {:?}", proof);

        let previous_proven_batch_metadata = match storage
            .blocks_dal()
            .get_l1_batch_metadata(previous_proven_batch_number)
            .await
        {
            Ok(Some(metadata)) => metadata,
            Ok(None) => {
                tracing::error!(
                    "L1 batch #{} with submitted proof is not complete in the DB",
                    previous_proven_batch_number
                );
                return None;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to retrieve L1 batch #{} metadata: {}",
                    previous_proven_batch_number,
                    e
                );
                return None;
            }
        };

        tracing::error!(
            "previous_proven_batch_metadata : {:?}",
            previous_proven_batch_metadata
        );

        let metadata_for_batch_being_proved = match storage
            .blocks_dal()
            .get_l1_batch_metadata(batch_to_prove)
            .await
        {
            Ok(Some(metadata)) => metadata,
            Ok(None) => {
                tracing::error!(
                    "L1 batch #{} with generated proof is not complete in the DB",
                    batch_to_prove
                );
                return None;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to retrieve L1 batch #{} metadata: {}",
                    batch_to_prove,
                    e
                );
                return None;
            }
        };

        tracing::error!(
            "metadata_for_batch_being_proved : {:?}",
            metadata_for_batch_being_proved
        );

        Some(ProveBatches {
            prev_l1_batch: previous_proven_batch_metadata,
            l1_batches: vec![metadata_for_batch_being_proved],
            proofs: vec![proof],
            should_verify: true,
        })
    }

    async fn prepare_dummy_proof_operation(
        &self,
        batch_to_prove: L1BatchNumber,
    ) -> Option<Vec<u8>> {
        let mut storage = self.pool.connection_tagged("da_dispatcher").await.ok()?;

        let previous_proven_batch_number =
            match storage.blocks_dal().get_last_l1_batch_with_prove_tx().await {
                Ok(batch_number) => batch_number,
                Err(e) => {
                    tracing::error!("Failed to retrieve the last L1 batch with proof tx: {}", e);
                    return None;
                }
            };

        let previous_proven_batch_metadata = match storage
            .blocks_dal()
            .get_l1_batch_metadata(previous_proven_batch_number)
            .await
        {
            Ok(Some(metadata)) => metadata,
            Ok(None) => {
                tracing::error!(
                    "L1 batch #{} with submitted proof is not complete in the DB",
                    previous_proven_batch_number
                );
                return None;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to retrieve L1 batch #{} metadata: {}",
                    previous_proven_batch_number,
                    e
                );
                return None;
            }
        };

        let metadata_for_batch_being_proved = match storage
            .blocks_dal()
            .get_l1_batch_metadata(batch_to_prove)
            .await
        {
            Ok(Some(metadata)) => metadata,
            Ok(None) => {
                tracing::error!(
                    "L1 batch #{} with generated proof is not complete in the DB",
                    batch_to_prove
                );
                return None;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to retrieve L1 batch #{} metadata: {}",
                    batch_to_prove,
                    e
                );
                return None;
            }
        };

        let res = ProveBatches {
            prev_l1_batch: previous_proven_batch_metadata,
            l1_batches: vec![metadata_for_batch_being_proved],
            proofs: vec![],
            should_verify: false,
        };

        let prev_l1_batch_bytes = bincode::serialize(&res.prev_l1_batch)
            .map_err(|e| {
                tracing::error!("Failed to serialize prev_l1_batch: {}", e);
                None::<Vec<u8>>
            })
            .ok()?;
        let l1_batches_bytes = bincode::serialize(&res.l1_batches)
            .map_err(|e| {
                tracing::error!("Failed to serialize l1_batches: {}", e);
                None::<Vec<u8>>
            })
            .ok()?;
        let proofs_bytes = bincode::serialize(&res.proofs)
            .map_err(|e| {
                tracing::error!("Failed to serialize proofs: {}", e);
                None::<Vec<u8>>
            })
            .ok()?;
        let should_verify = bincode::serialize(&res.should_verify)
            .map_err(|e| {
                tracing::error!("Failed to serialize should_verify: {}", e);
                None::<Vec<u8>>
            })
            .ok()?;

        let final_proof = [
            prev_l1_batch_bytes,
            l1_batches_bytes,
            proofs_bytes,
            should_verify,
        ]
        .concat();

        Some(final_proof)
    }
    /// Loads wrapped FRI proofs for a given L1 batch number and allowed protocol versions.
    pub async fn load_wrapped_fri_proofs_for_range(
        &self,
        l1_batch_number: L1BatchNumber,
        allowed_versions: &[ProtocolSemanticVersion],
    ) -> Option<L1BatchProofForL1> {
        tracing::error!("load_wrapped_fri_proofs_for_range started");
        for version in allowed_versions {
            match self.blob_store.get((l1_batch_number, *version)).await {
                Ok(proof) => {
                    tracing::error!("load_wrapped_fri_proofs_for_range proof : {:?}", proof);
                    return Some(proof);
                }
                Err(ObjectStoreError::KeyNotFound(_)) => {
                    tracing::error!(
                        "KeyNotFound for loading proof for batch {} and version {:?}",
                        l1_batch_number.0,
                        version
                    );
                    continue;
                } // Proof is not ready yet.
                Err(err) => {
                    tracing::error!(
                        "Failed to load proof for batch {} and version {:?}: {}",
                        l1_batch_number.0,
                        version,
                        err
                    );
                    return None;
                }
            }
        }

        tracing::error!("load_wrapped_fri_proofs_for_range failed");

        // Check for deprecated file naming if patch 0 is allowed.
        let is_patch_0_present = allowed_versions.iter().any(|v| v.patch.0 == 0);
        tracing::error!("is_patch_0_present : {:?}", is_patch_0_present);
        if is_patch_0_present {
            match self
                .blob_store
                .get_by_encoded_key(format!("l1_batch_proof_{}.bin", l1_batch_number))
                .await
            {
                Ok(proof) => {
                    tracing::error!("load_wrapped_fri_proofs_for_range proof : {:?}", proof);
                    return Some(proof);
                }
                Err(ObjectStoreError::KeyNotFound(_)) => {
                    tracing::error!(
                        "KeyNotFound for loading proof for batch {}",
                        l1_batch_number.0
                    );
                }
                Err(err) => {
                    tracing::error!(
                        "Failed to load proof for batch {} from deprecated naming: {}",
                        l1_batch_number.0,
                        err
                    );
                    return None;
                }
            }
        }

        None
    }

    /// Polls the data availability layer for inclusion data, and saves it in the database.
    async fn poll_for_inclusion_l1_batch(&self) -> anyhow::Result<()> {
        let mut conn = self.pool.connection_tagged("da_dispatcher").await?;
        let blob_info = conn
            .via_data_availability_dal()
            .get_first_da_blob_awaiting_inclusion()
            .await?;
        drop(conn);

        let Some(blob_info) = blob_info else {
            return Ok(());
        };

        let inclusion_data = if self.config.use_dummy_inclusion_data() {
            self.client
                .get_inclusion_data(blob_info.blob_id.as_str())
                .await
                .with_context(|| {
                    format!(
                        "failed to get inclusion data for blob_id: {}, batch_number: {}",
                        blob_info.blob_id, blob_info.l1_batch_number
                    )
                })?
        } else {
            // If the inclusion verification is disabled, we don't need to wait for the inclusion
            // data before committing the batch, so simply return an empty vector.
            Some(InclusionData { data: vec![] })
        };

        let Some(inclusion_data) = inclusion_data else {
            return Ok(());
        };

        let mut conn = self.pool.connection_tagged("da_dispatcher").await?;
        conn.via_data_availability_dal()
            .save_l1_batch_inclusion_data(
                L1BatchNumber(blob_info.l1_batch_number.0),
                inclusion_data.data.as_slice(),
            )
            .await?;
        drop(conn);

        let inclusion_latency = Utc::now().signed_duration_since(blob_info.sent_at);
        if let Ok(latency) = inclusion_latency.to_std() {
            METRICS.inclusion_latency.observe(latency);
        }
        METRICS
            .last_included_l1_batch
            .set(blob_info.l1_batch_number.0 as usize);

        tracing::info!(
            "Received inclusion data for batch_number: {}, inclusion_latency_seconds: {}",
            blob_info.l1_batch_number,
            inclusion_latency.num_seconds()
        );

        Ok(())
    }

    async fn poll_for_inclusion(&self) -> anyhow::Result<()> {
        self.poll_for_inclusion_l1_batch().await?;
        self.poll_for_inclusion_proof().await?;
        Ok(())
    }
    async fn poll_for_inclusion_proof(&self) -> anyhow::Result<()> {
        let mut conn = self.pool.connection_tagged("da_dispatcher").await?;

        let proof_info = conn
            .via_data_availability_dal()
            .get_first_proof_blob_awaiting_inclusion()
            .await?;
        drop(conn);

        let Some(proof_info) = proof_info else {
            return Ok(());
        };

        let inclusion_data = if self.config.use_dummy_inclusion_data() {
            self.client
                .get_inclusion_data(proof_info.blob_id.as_str())
                .await
                .with_context(|| {
                    format!(
                        "failed to get inclusion data for proof_blob_url: {}, batch_number: {}",
                        proof_info.blob_id, proof_info.l1_batch_number
                    )
                })?
        } else {
            // If the inclusion verification is disabled, we don't need to wait for the inclusion
            // data before committing the batch, so simply return an empty vector.
            Some(InclusionData { data: vec![] })
        };

        let Some(inclusion_data) = inclusion_data else {
            return Ok(());
        };

        let mut conn = self.pool.connection_tagged("da_dispatcher").await?;
        conn.via_data_availability_dal()
            .save_proof_inclusion_data(
                L1BatchNumber(proof_info.l1_batch_number.0),
                inclusion_data.data.as_slice(),
            )
            .await?;

        drop(conn);

        let inclusion_latency = Utc::now().signed_duration_since(proof_info.sent_at);
        if let Ok(latency) = inclusion_latency.to_std() {
            METRICS.inclusion_latency.observe(latency);
        }

        METRICS
            .last_included_proof_batch
            .set(proof_info.l1_batch_number.0 as usize);

        tracing::info!(
            "Received inclusion data for proof batch_number: {}, inclusion_latency_seconds: {}",
            proof_info.l1_batch_number,
            inclusion_latency.num_seconds()
        );

        Ok(())
    }
}

/// Recursively flattens all `Token` variants into a single `Vec<u8>`.
fn flatten_tokens(tokens: &[Token]) -> Vec<u8> {
    let mut out = Vec::new();
    for token in tokens {
        match token {
            Token::Uint(u) => {
                let mut buf = [0u8; 32];
                u.to_big_endian(&mut buf);
                out.extend_from_slice(&buf);
            }
            Token::FixedBytes(fixed_bytes) => {
                out.extend_from_slice(fixed_bytes);
            }
            Token::Array(arr) | Token::FixedArray(arr) | Token::Tuple(arr) => {
                out.extend(flatten_tokens(arr));
            }
            // not used in the prover
            Token::Bool(b) => {
                out.push(if *b { 1 } else { 0 });
            }
            Token::Address(a) => {
                out.extend_from_slice(a.as_bytes());
            }
            Token::Int(i) => {
                let mut buf = [0u8; 32];
                i.to_big_endian(&mut buf);
                out.extend_from_slice(&buf);
            }
            Token::Bytes(bytes) => {
                out.extend_from_slice(bytes);
            }
            Token::String(s) => {
                let str_bytes = s.as_bytes();
                let len_u32 = str_bytes.len() as u32;
                out.extend_from_slice(&len_u32.to_be_bytes());
                out.extend_from_slice(str_bytes);
            }
        }
    }
    out
}

async fn retry<T, Fut, F>(
    max_retries: u16,
    batch_number: L1BatchNumber,
    mut f: F,
) -> Result<T, DAError>
where
    Fut: Future<Output = Result<T, DAError>>,
    F: FnMut() -> Fut,
{
    let mut retries = 1;
    let mut backoff_secs = 1;
    loop {
        match f().await {
            Ok(result) => {
                METRICS.dispatch_call_retries.observe(retries as usize);
                return Ok(result);
            }
            Err(err) => {
                if !err.is_retriable() || retries > max_retries {
                    return Err(err);
                }

                retries += 1;
                let sleep_duration = Duration::from_secs(backoff_secs)
                    .mul_f32(rand::thread_rng().gen_range(0.8..1.2));
                tracing::warn!(%err, "Failed DA dispatch request {retries}/{max_retries} for batch {batch_number}, retrying in {} milliseconds.", sleep_duration.as_millis());
                tokio::time::sleep(sleep_duration).await;

                backoff_secs = (backoff_secs * 2).min(128); // cap the back-off at 128 seconds
            }
        }
    }
}
