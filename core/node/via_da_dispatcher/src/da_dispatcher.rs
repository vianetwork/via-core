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
use zksync_object_store::{bincode, ObjectStore};
use zksync_types::{protocol_version::L1VerifierConfig, L1BatchNumber};

use crate::{metrics::METRICS, proof_manager::ProofManager};

#[derive(Debug)]
pub struct DataAvailabilityDispatcher {
    client: Box<dyn DataAvailabilityClient>,
    pool: ConnectionPool<Core>,
    config: DADispatcherConfig,
    proof_manager: ProofManager,
    l1_verifier_config: L1VerifierConfig,
}

impl DataAvailabilityDispatcher {
    pub fn new(
        pool: ConnectionPool<Core>,
        config: DADispatcherConfig,
        client: Box<dyn DataAvailabilityClient>,
        blob_store: Arc<dyn ObjectStore>,
        l1_verifier_config: L1VerifierConfig,
    ) -> Self {
        let proof_manager = ProofManager::new(blob_store);

        Self {
            pool,
            config,
            client,
            proof_manager,
            l1_verifier_config,
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
        let mut conn = self.pool.connection_tagged("da_dispatcher").await?;
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

    /// Dispatches proofs to the data availability layer, and saves the blob_id in the database.
    async fn dispatch_proofs(&self) -> anyhow::Result<()> {
        let mut conn = self.pool.connection_tagged("da_dispatcher").await?;

        if let Some(prove_batches) = self
            .proof_manager
            .load_real_proof_operation(&mut conn, self.l1_verifier_config)
            .await
        {
            for (proof, batch) in prove_batches
                .proofs
                .iter()
                .zip(prove_batches.l1_batches.iter())
            {
                // Serialize the proof data
                let proof_data = bincode::serialize(proof).context("Failed to serialize proof")?;

                let dispatch_response =
                    retry(self.config.max_retries(), batch.header.number, || {
                        self.client
                            .dispatch_blob(batch.header.number.0, proof_data.clone())
                    })
                    .await
                    .with_context(|| {
                        format!(
                            "failed to dispatch proof blob for batch_number: {}",
                            batch.header.number
                        )
                    })?;
                let sent_at = Utc::now().naive_utc();

                let mut conn = self.pool.connection_tagged("da_dispatcher").await?;
                conn.via_data_availability_dal()
                    .insert_proof_da(
                        batch.header.number,
                        dispatch_response.blob_id.as_str(),
                        sent_at,
                    )
                    .await?;
                drop(conn);

                tracing::info!("Dispatched proof for batch_number: {}", batch.header.number);
            }
        } else {
            tracing::info!("No proofs ready for dispatch");
        }
        Ok(())
    }

    /// Polls the data availability layer for inclusion data, and saves it in the database.
    async fn poll_for_inclusion(&self) -> anyhow::Result<()> {
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
