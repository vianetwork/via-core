use std::{str::FromStr, sync::Arc};

use anyhow::Context;
use metrics::{NoVerdictReason, METRICS};
use tokio::sync::watch;
use via_btc_client::{
    client::BitcoinClient,
    types::{L1BatchDAReferenceInput, HeaderMembership},
};
use via_btc_storage::{ProofWork, ReaderDal};
use via_da_client::{pubdata::Pubdata, types::L2_BOOTLOADER_CONTRACT_ADDR};
use via_da_dispatcher_lib::blob::find_wrapped_proof;
use via_verification::{decode_prove_batch_data, verify_proof, ProveBatchData};
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use via_verifier_types::protocol_version::check_if_supported_sequencer_version;
use zksync_config::ViaVerifierConfig;
use zksync_da_client::DataAvailabilityClient;
use zksync_object_store::{ObjectStore, ObjectStoreError};
use zksync_types::{protocol_version::ProtocolSemanticVersion, ProtocolVersionId, H160, H256};

mod metrics;
#[cfg(test)]
mod tests;

/// A batch approval recorded as a yes vote.
/// The verifier records nothing else, so a batch it cannot approve stops progress where it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Approval {
    Verified,
    /// Development mode accepted the batch without a proof, and it is persisted as unverified.
    DevAccepted,
}

#[derive(Debug)]
pub struct ViaVerifier {
    config: ViaVerifierConfig,
    pool: ConnectionPool<Verifier>,
    da_client: Box<dyn DataAvailabilityClient>,
    btc_client: Arc<BitcoinClient>,
    blob_store: Arc<dyn ObjectStore>,
}

impl ViaVerifier {
    pub async fn new(
        config: ViaVerifierConfig,
        pool: ConnectionPool<Verifier>,
        da_client: Box<dyn DataAvailabilityClient>,
        btc_client: Arc<BitcoinClient>,
        blob_store: Arc<dyn ObjectStore>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            config,
            pool,
            da_client,
            btc_client,
            blob_store,
        })
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.config.polling_interval());
        let pool = self.pool.clone();

        // jscpd:ignore-start Each actor owns polling and cancellation. Verifier work adds another cancellation boundary.
        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }
            // jscpd:ignore-end

            let mut storage = tokio::select! {
                biased;
                _ = stop_receiver.changed() => break,
                storage = pool.connection_tagged("via_zk_verifier") => storage?,
            };
            // Cancellation before commit rolls back verdict transaction.
            // Commit already in flight may finish, and next start reads its durable result.
            let result = tokio::select! {
                biased;
                _ = stop_receiver.changed() => break,
                result = self.loop_iteration(&mut storage) => result,
            };
            match result {
                Ok(()) => {}
                Err(err) => {
                    METRICS.errors.inc();
                    let reason = err
                        .downcast_ref::<NoVerdict>()
                        .map_or(NoVerdictReason::Unclassified, |nv| nv.reason);
                    METRICS.non_verdicts[&reason].inc();
                    tracing::error!("Failed to process via_zk_verifier: {err:#}")
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
        report_watermarks(storage).await?;

        if let Some(work) = storage.via_votes_dal().pending_reader_proof().await? {
            if !ReaderDal::new(storage).proof_inputs_complete(&work).await?
                || !self.proof_is_mature(&work).await?
            {
                return Ok(());
            }
            let latency = METRICS.verification_time.start();
            let batch_da = &work.proof.payload.input.pubdata.payload.input;
            let l1_batch_number = i64::from(batch_da.l1_batch_index.0);
            let Some(wallets) =
                ReaderDal::new(storage).wallets_before(work.proof.origin.height).await?
            else {
                return Ok(());
            };
            wallets.is_valid_verifier_address(self.config.wallet_address()?)?;
            let proof_blob =
                self.fetch_blob(&work.proof.payload.input.proof.blob_id, l1_batch_number).await?;
            let batch_blob = self.fetch_blob(&batch_da.blob_id, l1_batch_number).await?;
            let mut pubdata =
                Pubdata::decode_pubdata(batch_blob.data.clone().to_vec()).map_err(|err| {
                    no_verdict(NoVerdictReason::MalformedPackage, l1_batch_number, err)
                })?;

            let upgrade_tx_hash_opt =
                self.verify_upgrade_tx_hash(storage, &pubdata, work.proof.origin.height).await?;

            if upgrade_tx_hash_opt.is_some() {
                pubdata.user_logs.remove(0);
            }
            let allowed_versions = storage
                .via_protocol_versions_dal()
                .versions_for_proof(work.proof.origin.height, upgrade_tx_hash_opt)
                .await?;
            let protocol_version =
                *allowed_versions.first().context("protocol prefix is unavailable")?;
            check_if_supported_sequencer_version(protocol_version)?;
            let approval = verify_batch_proof(
                &*self.blob_store,
                self.config.proof_verification_dev_mode,
                batch_da,
                &allowed_versions,
                protocol_version.minor,
                &proof_blob.data,
            )
            .await?;
            tracing::info!("Proof check for l1 batch {l1_batch_number}: {approval:?}");

            if !self.proof_is_mature(&work).await? {
                return Ok(());
            }
            let committed = record_approval(
                storage,
                &work,
                approval,
                &pubdata,
                upgrade_tx_hash_opt,
            )
            .await?;
            if committed {
                if approval == Approval::DevAccepted {
                    METRICS.dev_accepted_batches.inc();
                }
                latency.observe();
                tracing::info!("Approved l1 batch {l1_batch_number}: {approval:?}");
            }
        }

        Ok(())
    }

    /// Check whether the first user_log corresponds to an upgrade transaction.
    pub async fn verify_upgrade_tx_hash(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        pubdata: &Pubdata,
        height: u32,
    ) -> anyhow::Result<Option<H256>> {
        if let Some(upgrade_tx_hash) = storage
            .via_protocol_versions_dal()
            .pending_upgrade_before(height)
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

    async fn fetch_blob(
        &self,
        id: &str,
        batch: i64,
    ) -> anyhow::Result<zksync_da_client::types::InclusionData> {
        self.da_client
            .get_inclusion_data(id)
            .await
            .map_err(|err| no_verdict(NoVerdictReason::PackageUnavailable, batch, err.into()))?
            .ok_or_else(|| {
                no_verdict(
                    NoVerdictReason::PackageUnavailable,
                    batch,
                    anyhow::anyhow!("DA blob {id} unavailable"),
                )
            })
    }

    async fn proof_is_mature(&self, work: &ProofWork) -> anyhow::Result<bool> {
        let Some(header) = self.btc_client.header_by_hash(&work.proof.origin.hash).await? else {
            return Ok(false);
        };
        Ok(header.height == work.proof.origin.height
            && matches!(header.membership,
            HeaderMembership::Canonical(confirmations) if confirmations.get() >= self.btc_client.reorg_safe_depth()?.get()))
    }
}

/// Approves one batch from its proof package, or returns a `NoVerdict` error.
///
/// Steps:
/// 1. Decode the package, require one batch with at most one proof, and match it to the inscription.
/// 2. Fetch an omitted proof under every registered patch of the batch's protocol version.
/// 3. Approve a proof that verifies, or in development mode a proof found nowhere.
///    The caller's store must be designated for development before it records that approval.
///
/// Nothing here is conclusive evidence against the batch, because the proof's public inputs come from the package unauthenticated.
/// Every failure therefore records nothing, and the next poll retries the same batch.
async fn verify_batch_proof(
    blob_store: &dyn ObjectStore,
    dev_mode: bool,
    inscribed: &L1BatchDAReferenceInput,
    allowed_versions: &[ProtocolSemanticVersion],
    minor: ProtocolVersionId,
    proof_data: &[u8],
) -> anyhow::Result<Approval> {
    let batch = inscribed.l1_batch_index;
    let number = i64::from(batch.0);
    // Package this node could not decode may have been rebuilt by serving node, hence it waits rather than counts as invalid.
    // Engine API likewise separates missing data (SYNCING) from validation, but Via drops its INVALID-for-malformed rule:
    // https://github.com/ethereum/execution-apis/blob/5bcdc34a477b10af278c079525374e6a4046f291/src/engine/paris.md#L175-L179
    let mut prove_batch_data = decode_prove_batch_data(minor, proof_data)
        .and_then(|data| data.check_shape().map(|()| data))
        .map_err(|err| no_verdict(NoVerdictReason::MalformedPackage, number, err))?;

    // Package must name inscribed batch under local protocol version, because that version selects proof lookup.
    // zkSync's Executor matches whole batch records, commitments included, but inscription carries no commitment to bind:
    // https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol#L484-L500
    let claimed = (prove_batch_data.claimed_batch(), prove_batch_data.protocol_version_id());
    let expected = ((batch, inscribed.l1_batch_hash, inscribed.prev_l1_batch_hash), Some(minor));
    if claimed != expected {
        return Err(no_verdict(
            NoVerdictReason::MalformedPackage,
            number,
            anyhow::anyhow!("package claims {claimed:?}, inscription names {expected:?}"),
        ));
    }

    if !prove_batch_data.has_proof() {
        if allowed_versions.is_empty() {
            return Err(no_verdict(
                NoVerdictReason::ProofUnavailable,
                number,
                anyhow::anyhow!("no registered patch for protocol version {minor}"),
            ));
        }
        // Proofs are stored per exact patch, and each minor has one verification key, hence all patches are tried, newest first.
        // zkSync's eth sender also tries versions in turn, filtered by L1-authorized key. Wrong-key proof fails without record:
        // https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_sender/src/aggregator.rs#L1030-L1050
        let found = match &mut prove_batch_data {
            ProveBatchData::V27(data) => find_wrapped_proof(blob_store, batch, allowed_versions)
                .await
                .map(|proof| proof.map(|proof| data.proofs.push(proof)).is_some()),
            ProveBatchData::V28(data) => find_wrapped_proof(blob_store, batch, allowed_versions)
                .await
                .map(|proof| proof.map(|proof| data.proofs.push(proof)).is_some()),
        };
        // Batch waits on absent object, which may arrive later. Object that exists but will not decode is unusable.
        // Bitcoin Core's VerifyDB draws same line between data it lacks and data it cannot read:
        // https://github.com/bitcoin/bitcoin/blob/d82283950f5ff3b2116e705f931c6e89e5fdd0be/src/validation.cpp#L4451-L4462
        match found {
            Ok(true) => {}
            // Development acceptance needs absent proof, because external-node DA path rewrites `should_verify` from serving config.
            // zkSync's TestnetVerifier also skips only empty proof and verifies any other, but its empty proof is fixed calldata:
            // https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/TestnetVerifier.sol#L14-L33
            Ok(false) if dev_mode => {
                // Local absence is acceptable only because store fence confines it to regtest.
                // Proof arriving later is never checked.
                tracing::warn!(
                    "No proof found for l1 batch {batch}, and development mode skips verification"
                );
                return Ok(Approval::DevAccepted);
            }
            Ok(false) => {
                return Err(no_verdict(
                    NoVerdictReason::ProofUnavailable,
                    number,
                    anyhow::anyhow!("no proof stored for batch {batch} under {allowed_versions:?}"),
                ))
            }
            Err(err @ ObjectStoreError::Serialization(_)) => {
                return Err(no_verdict(NoVerdictReason::MalformedPackage, number, err.into()))
            }
            Err(err) => {
                return Err(no_verdict(NoVerdictReason::ProofStoreError, number, err.into()))
            }
        }
    }

    // False result shows only that this proof fails for package's commitments, which nothing binds to inscription.
    // Lighthouse's proof engine likewise says failed proof says nothing about payload it claims to prove:
    // https://github.com/sigp/lighthouse/blob/de4ef4002115b6f94a42868114cabd66f7273328/beacon_node/proof_engine/src/lib.rs#L24-L30
    match verify_proof(prove_batch_data).await {
        Ok(true) => Ok(Approval::Verified),
        Ok(false) => Err(no_verdict(
            NoVerdictReason::ProofFailed,
            number,
            anyhow::anyhow!("proof does not verify"),
        )),
        Err(err) => Err(no_verdict(NoVerdictReason::VerificationError, number, err)),
    }
}

/// Reports how far indexing and verification have reached, read from the database on every poll.
/// Gauges set by the worker would reset on restart and stay ahead after a reorg, hiding a stall.
/// zkSync's house keeper likewise reports stage watermarks from the database, not from the workers:
/// https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/house_keeper/src/blocks_state_reporter.rs#L34-L81
async fn report_watermarks(storage: &mut Connection<'_, Verifier>) -> anyhow::Result<(u32, u32)> {
    let (indexed, valid) = storage.via_votes_dal().get_verification_watermarks().await?;
    METRICS.last_indexed_l1_batch.set(indexed as usize);
    METRICS.last_valid_l1_batch.set(valid as usize);
    Ok((indexed, valid))
}

/// record_approval records local approval with its deposit and upgrade effects in one transaction.
/// Returns false, with nothing written, for replaced row or header, incomplete prefix or concurrent reorg.
async fn record_approval(
    storage: &mut Connection<'_, Verifier>,
    work: &ProofWork,
    approval: Approval,
    pubdata: &Pubdata,
    upgrade_tx_hash: Option<H256>,
) -> anyhow::Result<bool> {
    let mut transaction = storage.start_transaction().await?;
    transaction.via_indexer_dal().lock_btc_watch().await?;
    transaction.via_indexer_dal().lock_withdrawal_invalidation().await?;
    // Replay readiness waits for rotation decisions, which wait for local proof verdicts.
    // This local save breaks that cycle only after this proof's complete, mature inputs are fixed.
    // Votes, signatures, L2 execution, finalization and withdrawals keep requiring global readiness.
    if !ReaderDal::new(&mut transaction).proof_inputs_complete(work).await? {
        return Ok(false);
    }
    let l1_batch_number =
        i64::from(work.proof.payload.input.pubdata.payload.input.l1_batch_index.0);
    if !transaction.via_votes_dal().complete_reader_proof(work).await? {
        return Ok(false);
    }
    let deposits =
        verify_op_priority_id(&mut transaction, l1_batch_number, pubdata, work.proof.origin.height)
            .await?;
    let votable_transaction_id = work.id;

    if approval == Approval::DevAccepted {
        transaction.via_votes_dal().mark_unverified_dev(votable_transaction_id).await?;
    }

    for (hash, status) in deposits {
        transaction
            .via_transactions_dal()
            .update_transaction(&hash, status, l1_batch_number)
            .await?;
    }

    if let Some(upgrade_tx_hash) = upgrade_tx_hash {
        transaction
            .via_protocol_versions_dal()
            .mark_upgrade_as_executed(upgrade_tx_hash.as_bytes())
            .await?;
    }

    transaction.commit().await?;
    Ok(true)
}

/// Returns the deposit statuses the batch settles, in priority order.
/// A mismatch with the first unprocessed deposits may be this node's indexing, so it records nothing.
/// Bitcoin Core treats missing inputs as block invalidity only after asserting its view is the verified parent state.
/// This node's deposit index is not such a state:
/// https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/validation.cpp#L2330-L2332
async fn verify_op_priority_id(
    storage: &mut Connection<'_, Verifier>,
    l1_batch_number: i64,
    pubdata: &Pubdata,
    height: u32,
) -> anyhow::Result<Vec<(H256, bool)>> {
    let bootloader = H160::from_str(L2_BOOTLOADER_CONTRACT_ADDR)?;
    let deposit_logs: Vec<_> =
        pubdata.user_logs.iter().filter(|log| log.sender == bootloader).collect();

    let txs = storage
        .via_transactions_dal()
        .deposits_for_proof(l1_batch_number,deposit_logs.len() as i64,height)
        .await?;

    if txs.len() != deposit_logs.len() {
        return Err(no_verdict(
            NoVerdictReason::DepositIndexIncomplete,
            l1_batch_number,
            anyhow::anyhow!(
                "Verifier has {} source-bound deposits, pubdata requires {}",
                txs.len(),
                deposit_logs.len(),
            ),
        ));
    }

    txs.iter()
        .zip(deposit_logs)
        .map(|((raw_tx_id, settled, canonical), deposit_log)| {
            if !canonical { return Err(no_verdict(NoVerdictReason::DepositIndexIncomplete,l1_batch_number,anyhow::anyhow!("deposit source is incomplete"))); }
            let status = !deposit_log.value.is_zero();
            if settled.is_some_and(|saved| saved!=status) {
                return Err(no_verdict(NoVerdictReason::DepositMismatch,l1_batch_number,anyhow::anyhow!("deposit settlement conflicts with the completed batch")));
            }
            let indexed = H256::from_slice(raw_tx_id);
            if indexed != deposit_log.key {
                return Err(no_verdict(
                    NoVerdictReason::DepositMismatch,
                    l1_batch_number,
                    anyhow::anyhow!(
                        "next indexed deposit is {indexed}, pubdata settles {}",
                        deposit_log.key
                    ),
                ));
            }
            Ok((deposit_log.key, status))
        })
        .collect()
}

/// Marks an error as leaving a batch without a verdict, which `run` counts once by reason.
#[derive(Debug)]
struct NoVerdict {
    reason: NoVerdictReason,
    l1_batch_number: i64,
}

impl std::fmt::Display for NoVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no verdict for l1 batch {}: {:?}", self.l1_batch_number, self.reason)
    }
}

/// Writes nothing, so the next poll selects the same first unverified batch and progress stops there.
/// Citrea separates its advancing scan cursor from pending proof processing.
/// Via retries through the existing first-unverified selection without a separate queue:
/// https://github.com/chainwayxyz/citrea/blob/f11527f94344d5dc4576ccb9589d5713fb8f7238/crates/fullnode/src/da_block_handler.rs#L941-L1000
fn no_verdict(reason: NoVerdictReason, l1_batch_number: i64, err: anyhow::Error) -> anyhow::Error {
    err.context(NoVerdict { reason, l1_batch_number })
}
