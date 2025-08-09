use std::{fmt, sync::Arc, time::Duration};

use anyhow::Context as _;
use bitcoin::{hashes::Hash, Address as BitcoinAddress, Txid};
use serde::Serialize;
use tokio::sync::watch;
use via_btc_client::{
    client::BitcoinClient, indexer::MessageParser, traits::BitcoinOps,
    types::FullInscriptionMessage,
};
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_eth_client::{ContractCallError, EnrichedClientError};
use zksync_health_check::{Health, HealthStatus, HealthUpdater, ReactiveHealthCheck};
use zksync_shared_metrics::{CheckerComponent, EN_METRICS};
use zksync_types::{L1BatchNumber, H256};

#[derive(Debug, thiserror::Error)]
enum CheckError {
    #[error("Web3 error communicating with L1")]
    Web3(#[from] EnrichedClientError),
    #[error("error calling L1 contract")]
    ContractCall(#[from] ContractCallError),
    /// Error that is caused by the main node providing incorrect information etc.
    #[error("failed validating commit transaction")]
    Validation(anyhow::Error),
    /// Error that is caused by violating invariants internal to *this* node (e.g., not having expected data in Postgres).
    #[error("internal error")]
    Internal(anyhow::Error),
}

impl CheckError {
    fn is_retriable(&self) -> bool {
        match self {
            Self::Web3(err) | Self::ContractCall(ContractCallError::EthereumGateway(err)) => {
                err.is_retriable()
            }
            _ => false,
        }
    }
}

/// Handler of life cycle events emitted by [`ConsistencyChecker`].
trait HandleConsistencyCheckerEvent: fmt::Debug + Send + Sync {
    fn initialize(&mut self);

    fn set_first_batch_to_check(&mut self, first_batch_to_check: L1BatchNumber);

    fn update_checked_batch(&mut self, last_checked_batch: L1BatchNumber);

    fn report_inconsistent_batch(&mut self, number: L1BatchNumber, err: &anyhow::Error);
}

/// Health details reported by [`ConsistencyChecker`].
#[derive(Debug, Default, Serialize)]
struct ConsistencyCheckerDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    first_checked_batch: Option<L1BatchNumber>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_checked_batch: Option<L1BatchNumber>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    inconsistent_batches: Vec<L1BatchNumber>,
}

impl ConsistencyCheckerDetails {
    fn health(&self) -> Health {
        let status = if self.inconsistent_batches.is_empty() {
            HealthStatus::Ready
        } else {
            HealthStatus::Affected
        };
        Health::from(status).with_details(self)
    }
}

/// Default [`HandleConsistencyCheckerEvent`] implementation that reports the batch number as a metric and via health check details.
#[derive(Debug)]
struct ConsistencyCheckerHealthUpdater {
    inner: HealthUpdater,
    current_details: ConsistencyCheckerDetails,
}

impl ConsistencyCheckerHealthUpdater {
    fn new() -> (ReactiveHealthCheck, Self) {
        let (health_check, health_updater) = ReactiveHealthCheck::new("consistency_checker");
        let this = Self {
            inner: health_updater,
            current_details: ConsistencyCheckerDetails::default(),
        };
        (health_check, this)
    }
}

impl HandleConsistencyCheckerEvent for ConsistencyCheckerHealthUpdater {
    fn initialize(&mut self) {
        self.inner.update(self.current_details.health());
    }

    fn set_first_batch_to_check(&mut self, first_batch_to_check: L1BatchNumber) {
        self.current_details.first_checked_batch = Some(first_batch_to_check);
        self.inner.update(self.current_details.health());
    }

    fn update_checked_batch(&mut self, last_checked_batch: L1BatchNumber) {
        tracing::info!("L1 batch #{last_checked_batch} is consistent with L1");
        EN_METRICS.last_correct_batch[&CheckerComponent::ConsistencyChecker]
            .set(last_checked_batch.0.into());
        self.current_details.last_checked_batch = Some(last_checked_batch);
        self.inner.update(self.current_details.health());
    }

    fn report_inconsistent_batch(&mut self, number: L1BatchNumber, err: &anyhow::Error) {
        tracing::warn!("L1 batch #{number} is inconsistent with L1: {err:?}");
        self.current_details.inconsistent_batches.push(number);
        self.inner.update(self.current_details.health());
    }
}

#[derive(Debug)]
enum L1DataMismatchBehavior {
    Log,
}

/// L1 commit data loaded from Postgres.
#[derive(Debug)]
struct LocalL1BatchCommitData {
    // l1_batch: L1BatchWithMetadata,
    commit_tx_hash: H256,
}

impl LocalL1BatchCommitData {
    /// Returns `Ok(None)` if Postgres doesn't contain all data necessary to check L1 commitment
    /// for the specified batch.
    async fn new(
        storage: &mut Connection<'_, Core>,
        batch_number: L1BatchNumber,
    ) -> anyhow::Result<Option<Self>> {
        let Some(commit_tx_id) = storage
            .blocks_dal()
            .get_eth_commit_tx_id(batch_number)
            .await?
        else {
            return Ok(None);
        };

        let commit_tx_hash = storage
            .eth_sender_dal()
            .get_confirmed_tx_hash_by_eth_tx_id(commit_tx_id as u32)
            .await?
            .with_context(|| {
                format!("Commit tx hash not found in the database for tx id {commit_tx_id}")
            })?;

        Ok(Some(Self { commit_tx_hash }))
    }
}

#[derive(Debug)]
pub struct ConsistencyChecker {
    sequencer_address: BitcoinAddress,
    da_identifier: String,
    btc_client: Arc<BitcoinClient>,
    max_batches_to_recheck: u32,
    sleep_interval: Duration,
    event_handler: Box<dyn HandleConsistencyCheckerEvent>,
    l1_data_mismatch_behavior: L1DataMismatchBehavior,
    pool: ConnectionPool<Core>,
    health_check: ReactiveHealthCheck,
}

impl ConsistencyChecker {
    const DEFAULT_SLEEP_INTERVAL: Duration = Duration::from_secs(5);

    pub fn new(
        sequencer_address: BitcoinAddress,
        da_identifier: String,
        btc_client: Arc<BitcoinClient>,
        max_batches_to_recheck: u32,
        pool: ConnectionPool<Core>,
    ) -> anyhow::Result<Self> {
        let (health_check, health_updater) = ConsistencyCheckerHealthUpdater::new();
        Ok(Self {
            sequencer_address,
            da_identifier,
            btc_client,
            max_batches_to_recheck,
            sleep_interval: Self::DEFAULT_SLEEP_INTERVAL,
            event_handler: Box::new(health_updater),
            l1_data_mismatch_behavior: L1DataMismatchBehavior::Log,
            pool,
            health_check,
        })
    }

    /// Returns health check associated with this checker.
    pub fn health_check(&self) -> &ReactiveHealthCheck {
        &self.health_check
    }

    async fn check_commitments(
        &self,
        batch_number: L1BatchNumber,
        local: &LocalL1BatchCommitData,
    ) -> Result<(), CheckError> {
        let mut hash_bytes = local.commit_tx_hash.0;
        hash_bytes.reverse();
        let commit_tx_hash = Txid::from_byte_array(hash_bytes);
        tracing::info!("Checking commit tx {commit_tx_hash} for L1 batch #{batch_number}");

        let tx = self
            .btc_client
            .get_transaction(&commit_tx_hash)
            .await
            .with_context(|| format!("receipt for tx {commit_tx_hash:?} not found on L1"))
            .map_err(CheckError::Internal)?;

        let mut parser = MessageParser::new(self.btc_client.config.network());
        let inscriptions = parser.parse_system_transaction(&tx, 0, None);

        for inscription in inscriptions {
            if let FullInscriptionMessage::L1BatchDAReference(msg) = inscription {
                if msg.common.p2wpkh_address != Some(self.sequencer_address.clone()) {
                    let err = anyhow::anyhow!(
                        "Commit transaction {:?} was not signed by the expected sequencer address, batchNumber={}", commit_tx_hash, batch_number
                        );
                    return Err(CheckError::Validation(err));
                }

                if msg.input.l1_batch_index != batch_number {
                    let err = anyhow::anyhow!(
                        "Commit transaction {:?} does not contain the expected batchNumber, expected={}, found={}", commit_tx_hash, batch_number, msg.input.l1_batch_index
                        );
                    return Err(CheckError::Validation(err));
                }

                if msg.input.da_identifier != self.da_identifier {
                    let err = anyhow::anyhow!(
                        "Commit transaction {:?} does not contain expected `da_identifier`, expected={}, found={}", commit_tx_hash, self.da_identifier, msg.input.da_identifier
                        );
                    return Err(CheckError::Validation(err));
                }

                return Ok(());
            }

            let err = anyhow::anyhow!(
                "Commit transaction {:?} does is not valid, data not found",
                commit_tx_hash
            );
            return Err(CheckError::Validation(err));
        }

        Ok(())
    }

    async fn last_committed_batch(&self) -> anyhow::Result<Option<L1BatchNumber>> {
        Ok(self
            .pool
            .connection()
            .await?
            .blocks_dal()
            .get_number_of_last_l1_batch_committed_on_eth()
            .await?)
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        tracing::info!(
            "Starting consistency checker with Bitcoin network: {:?}, sleep interval: {:?}, \
             max historic L1 batches to check: {}",
            self.btc_client.config.network,
            self.sleep_interval,
            self.max_batches_to_recheck
        );
        self.event_handler.initialize();

        // It doesn't make sense to start the checker until we have at least one L1 batch with metadata.
        let earliest_l1_batch_number =
            wait_for_l1_batch_with_metadata(&self.pool, self.sleep_interval, &mut stop_receiver)
                .await?;

        let Some(earliest_l1_batch_number) = earliest_l1_batch_number else {
            return Ok(());
        };

        let last_committed_batch = self
            .last_committed_batch()
            .await?
            .unwrap_or(earliest_l1_batch_number);
        let first_batch_to_check: L1BatchNumber = last_committed_batch
            .0
            .saturating_sub(self.max_batches_to_recheck)
            .into();

        let last_processed_batch = self
            .pool
            .connection()
            .await?
            .blocks_dal()
            .get_consistency_checker_last_processed_l1_batch()
            .await?;

        // We shouldn't check batches not present in the storage, and skip the genesis batch since
        // it's not committed on L1.
        let first_batch_to_check = first_batch_to_check
            .max(earliest_l1_batch_number)
            .max(L1BatchNumber(last_processed_batch.0 + 1));
        tracing::info!(
            "Last committed L1 batch is #{last_committed_batch}; starting checks from L1 batch #{first_batch_to_check}"
        );
        self.event_handler
            .set_first_batch_to_check(first_batch_to_check);

        let mut batch_number = first_batch_to_check;
        while !*stop_receiver.borrow_and_update() {
            let mut storage = self.pool.connection().await?;
            // The batch might be already committed but not yet processed by the external node's tree
            // OR the batch might be processed by the external node's tree but not yet committed.
            // We need both.
            let local = LocalL1BatchCommitData::new(&mut storage, batch_number).await?;
            let Some(local) = local else {
                if tokio::time::timeout(self.sleep_interval, stop_receiver.changed())
                    .await
                    .is_ok()
                {
                    break;
                }
                continue;
            };
            drop(storage);

            match self.check_commitments(batch_number, &local).await {
                Ok(()) => {
                    let mut storage = self.pool.connection().await?;
                    storage
                        .blocks_dal()
                        .set_consistency_checker_last_processed_l1_batch(batch_number)
                        .await?;
                    self.event_handler.update_checked_batch(batch_number);
                    batch_number += 1;
                }
                Err(CheckError::Validation(err)) => {
                    self.event_handler
                        .report_inconsistent_batch(batch_number, &err);
                    match &self.l1_data_mismatch_behavior {
                        L1DataMismatchBehavior::Log => {
                            batch_number += 1; // We don't want to infinitely loop failing the check on the same batch
                        }
                    }
                }
                Err(err) if err.is_retriable() => {
                    tracing::warn!(
                        "Transient error while verifying L1 batch #{batch_number}; will retry after a delay: {:#}",
                        anyhow::Error::from(err)
                    );
                    if tokio::time::timeout(self.sleep_interval, stop_receiver.changed())
                        .await
                        .is_ok()
                    {
                        break;
                    }
                }
                Err(other_err) => {
                    let context =
                        format!("failed verifying consistency of L1 batch #{batch_number}");
                    return Err(anyhow::Error::from(other_err).context(context));
                }
            }
        }

        tracing::info!("Stop signal received, consistency_checker is shutting down");
        Ok(())
    }
}

/// Repeatedly polls the DB until there is an L1 batch with metadata. We may not have such a batch initially
/// if the DB is recovered from an application-level snapshot.
///
/// Returns the number of the *earliest* L1 batch with metadata, or `None` if the stop signal is received.
async fn wait_for_l1_batch_with_metadata(
    pool: &ConnectionPool<Core>,
    poll_interval: Duration,
    stop_receiver: &mut watch::Receiver<bool>,
) -> anyhow::Result<Option<L1BatchNumber>> {
    loop {
        if *stop_receiver.borrow() {
            return Ok(None);
        }

        let mut storage = pool.connection().await?;
        let sealed_l1_batch_number = storage
            .blocks_dal()
            .get_earliest_l1_batch_number_with_metadata()
            .await?;
        drop(storage);

        if let Some(number) = sealed_l1_batch_number {
            return Ok(Some(number));
        }
        tracing::debug!(
            "No L1 batches with metadata are present in DB; trying again in {poll_interval:?}"
        );
        tokio::time::timeout(poll_interval, stop_receiver.changed())
            .await
            .ok();
    }
}
