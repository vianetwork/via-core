use std::{future::Future, sync::Arc, time::Duration};

use anyhow::Context;
use chrono::Utc;
use rand::Rng;
use tokio::sync::watch::Receiver;
use via_da_dispatcher_lib::blob::load_wrapped_fri_proofs_for_range;
use zksync_config::DADispatcherConfig;
use zksync_da_client::{
    types::{DAError, InclusionData},
    DataAvailabilityClient,
};
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_l1_contract_interface::i_executor::methods::ProveBatches;
use zksync_object_store::ObjectStore;
use zksync_types::{
    via_da_dispatcher::{serialize_blob_ids, ViaDaBlob},
    L1BatchNumber,
};

use crate::metrics::METRICS;

/// The max blob size posted to DA layer.
const BLOB_CHUNK_SIZE: usize = 500 * 1024;

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
                        METRICS.errors.inc();
                        tracing::error!("dispatch error {err:?}");
                    }
                },
                async {
                    if let Err(err) = self.poll_for_inclusion().await {
                        METRICS.errors.inc();
                        tracing::error!("poll_for_inclusion error {err:?}");
                    }
                },
                async {
                    if let Err(err) = self.dispatch_proofs().await {
                        METRICS.errors.inc();
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
        if self.is_rollback_required(&mut conn).await? {
            return Ok(());
        }

        let batches = conn
            .via_data_availability_dal()
            .get_ready_for_da_dispatch_l1_batches(self.config.max_rows_to_dispatch() as usize)
            .await?;
        drop(conn);

        for batch in batches {
            let chunks: Vec<Vec<u8>> = batch
                .pubdata
                .clone()
                .chunks(BLOB_CHUNK_SIZE)
                .map(|chunk| chunk.to_vec())
                .collect();

            self._dispatch_chunks(batch.l1_batch_number, chunks, false, "da_dispatcher")
                .await?;

            METRICS
                .last_dispatched_l1_batch
                .set(batch.l1_batch_number.0 as usize);
            METRICS.blob_size.observe(batch.pubdata.len());
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
        if self.is_rollback_required(&mut conn).await? {
            return Ok(());
        }

        let batches = conn
            .via_data_availability_dal()
            .get_ready_for_dummy_proof_dispatch_l1_batches(
                self.config.max_rows_to_dispatch() as usize
            )
            .await?;

        drop(conn);

        for l1_batch_number in batches {
            let dummy_proof = self
                .prepare_dummy_proof_operation(l1_batch_number)
                .await
                .with_context(|| {
                    format!(
                        "failed to prepare a dummy proof for batch_number: {}",
                        l1_batch_number
                    )
                })?;

            let chunks: Vec<Vec<u8>> = dummy_proof
                .chunks(BLOB_CHUNK_SIZE)
                .map(|chunk| chunk.to_vec())
                .collect();

            self._dispatch_chunks(l1_batch_number, chunks, true, "dummy_proof_dispatcher")
                .await?;

            METRICS
                .last_dispatched_proof_batch
                .set(l1_batch_number.0 as usize);

            METRICS.blob_size.observe(dummy_proof.len());
        }

        Ok(())
    }

    async fn dispatch_real_proofs(&self) -> anyhow::Result<()> {
        let mut conn = self.pool.connection_tagged("da_dispatcher").await?;

        if self.is_rollback_required(&mut conn).await? {
            return Ok(());
        }

        let proofs = conn
            .via_data_availability_dal()
            .get_ready_for_da_dispatch_proofs(self.config.max_rows_to_dispatch() as usize)
            .await?;

        drop(conn);

        for proof in proofs {
            // fetch the proof from object store
            let final_proof = match self.load_real_proof_operation(proof.l1_batch_number).await {
                Some(proof) => proof,
                None => {
                    tracing::error!("Failed to load proof for batch {}", proof.l1_batch_number.0);
                    continue;
                }
            };

            let chunks: Vec<Vec<u8>> = final_proof
                .chunks(BLOB_CHUNK_SIZE)
                .map(|chunk| chunk.to_vec())
                .collect();

            self._dispatch_chunks(proof.l1_batch_number, chunks, true, "real_proof_dispatcher")
                .await?;

            METRICS
                .last_dispatched_proof_batch
                .set(proof.l1_batch_number.0 as usize);
            METRICS.blob_size.observe(final_proof.len());
        }
        Ok(())
    }

    async fn _dispatch_chunks(
        &self,
        l1_batch_number: L1BatchNumber,
        chunks: Vec<Vec<u8>>,
        is_proof: bool,
        tag: &'static str,
    ) -> anyhow::Result<()> {
        let mut blobs: Vec<String> = vec![];
        let mut index = 0;

        loop {
            let data = if index == chunks.len() && chunks.len() > 1 {
                let data = serialize_blob_ids(&blobs)?;
                let blob = ViaDaBlob::new(chunks.len(), data);
                blob.to_bytes()
            } else if index == 0 && chunks.len() == 1 {
                let blob = ViaDaBlob::new(chunks.len(), chunks[index].clone());
                blob.to_bytes()
            } else if index >= chunks.len() {
                break;
            } else {
                chunks[index].clone()
            };

            let dispatch_latency = METRICS.blob_dispatch_latency.start();
            let dispatch_response = retry(self.config.max_retries(), l1_batch_number, || {
                self.client.dispatch_blob(l1_batch_number.0, data.clone())
            })
            .await
            .with_context(|| {
                format!(
                    "failed to dispatch a blob with batch_number: {}, pubdata_len: {}, is_proof: {}",
                    l1_batch_number,
                    data.len(),
                    is_proof,
                )
            })?;

            let dispatch_latency_duration = dispatch_latency.observe();

            tracing::info!(
                    "Dispatched a DA for batch_number: {}, pubdata_size: {}, dispatch_latency: {:?}, index {}/{}, is_proof: {}",
                    l1_batch_number,
                    data.len(),
                    dispatch_latency_duration,
                    index + 1,
                    chunks.len(),
                    is_proof
                );

            blobs.push(dispatch_response.blob_id);

            index += 1;
        }

        let sent_at = Utc::now().naive_utc();

        let mut storage = self.pool.connection_tagged(tag).await?;
        let mut transaction = storage.start_transaction().await?;

        for (i, blob_id) in blobs.iter().enumerate() {
            if is_proof {
                transaction
                    .via_data_availability_dal()
                    .insert_proof_da(l1_batch_number, blob_id.as_str(), sent_at, i as i32)
                    .await?;
            } else {
                transaction
                    .via_data_availability_dal()
                    .insert_l1_batch_da(l1_batch_number, blob_id.as_str(), sent_at, i as i32)
                    .await?;
            }
        }

        transaction.commit().await?;

        Ok(())
    }

    /// Loads a real proof operation for a given L1 batch number.
    async fn load_real_proof_operation(&self, batch_to_prove: L1BatchNumber) -> Option<Vec<u8>> {
        let mut storage = self.pool.connection_tagged("da_dispatcher").await.ok()?;

        let (mut prove_batches, allowed_versions) = storage
            .via_data_availability_dal()
            .get_proof_data(batch_to_prove)
            .await?;

        let proof = match load_wrapped_fri_proofs_for_range(
            self.blob_store.clone(),
            batch_to_prove,
            &allowed_versions,
        )
        .await
        {
            Some(proof) => proof,
            None => {
                tracing::error!("Failed to load proof for batch {}", batch_to_prove);
                return None;
            }
        };

        prove_batches.proofs.push(proof);
        prove_batches.should_verify = true;

        serialize_prove_batches(&prove_batches)
    }

    async fn prepare_dummy_proof_operation(
        &self,
        batch_to_prove: L1BatchNumber,
    ) -> Option<Vec<u8>> {
        let mut storage = self.pool.connection_tagged("da_dispatcher").await.ok()?;

        let (mut prove_batches, _) = storage
            .via_data_availability_dal()
            .get_proof_data(batch_to_prove)
            .await?;
        prove_batches.should_verify = false;

        serialize_prove_batches(&prove_batches)
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
                blob_info.index,
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
                proof_info.index,
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

    async fn is_rollback_required(&self, conn: &mut Connection<'_, Core>) -> anyhow::Result<bool> {
        if conn
            .via_l1_block_dal()
            .has_reorg_in_progress()
            .await?
            .is_some()
        {
            return Ok(true);
        }

        if let Some(l1_batch_number) = conn
            .via_blocks_dal()
            .get_reverted_batch_by_verifier_network()
            .await?
        {
            tracing::warn!(
                "Can not dispatch data to DA, the l1 batch: {} is invalid. Roll back process should be executed on the Sequencer",
                l1_batch_number.0,
            );
            return Ok(true);
        }
        Ok(false)
    }
}

fn serialize_prove_batches(prove_batches: &ProveBatches) -> Option<Vec<u8>> {
    let prev_l1_batch_bytes = bincode::serialize(&prove_batches.prev_l1_batch)
        .map_err(|e| {
            tracing::error!("Failed to serialize prev_l1_batch: {}", e);
            None::<Vec<u8>>
        })
        .ok()?;
    let l1_batches_bytes = bincode::serialize(&prove_batches.l1_batches)
        .map_err(|e| {
            tracing::error!("Failed to serialize l1_batches: {}", e);
            None::<Vec<u8>>
        })
        .ok()?;
    let proofs_bytes = bincode::serialize(&prove_batches.proofs)
        .map_err(|e| {
            tracing::error!("Failed to serialize proofs: {}", e);
            None::<Vec<u8>>
        })
        .ok()?;
    let should_verify = bincode::serialize(&prove_batches.should_verify)
        .map_err(|e| {
            tracing::error!("Failed to serialize should_verify: {}", e);
            None::<Vec<u8>>
        })
        .ok()?;

    Some(
        [
            prev_l1_batch_bytes,
            l1_batches_bytes,
            proofs_bytes,
            should_verify,
        ]
        .concat(),
    )
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
