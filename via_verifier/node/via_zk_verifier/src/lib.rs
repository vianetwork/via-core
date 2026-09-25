use std::{str::FromStr, sync::Arc};

use anyhow::Context;
use metrics::{NoVerdictReason, METRICS};
use tokio::sync::watch;
use via_btc_client::{
    client::BitcoinClient,
    indexer::BitcoinInscriptionIndexer,
    types::{
        BitcoinTxid, FullInscriptionMessage, L1BatchDAReference, L1BatchDAReferenceInput,
        ProofDAReference,
    },
    utils::bytes_to_txid,
};
use via_consensus::consensus::BATCH_FINALIZATION_THRESHOLD;
use via_da_client::{pubdata::Pubdata, types::L2_BOOTLOADER_CONTRACT_ADDR};
use via_da_dispatcher_lib::blob::find_wrapped_proof;
use via_verification::{decode_prove_batch_data, verify_proof, ProveBatchData};
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
use via_verifier_state::sync::ViaState;
use via_verifier_types::protocol_version::check_if_supported_sequencer_version;
use zksync_config::{ViaBtcWatchConfig, ViaVerifierConfig};
use zksync_da_client::{types::InclusionData, DataAvailabilityClient};
use zksync_object_store::{ObjectStore, ObjectStoreError};
use zksync_types::{
    protocol_version::ProtocolSemanticVersion, via_wallet::SystemWallets, ProtocolVersionId, H160,
    H256,
};

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
            // A proof fetch can wait out every store retry, so stop must not wait for the iteration.
            // Dropping it mid-way rolls back the verdict transaction, and the next start retries the batch.
            let result = tokio::select! {
                result = self.loop_iteration(&mut storage) => result,
                _ = stop_receiver.changed() => break,
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
        if self.state.is_reorg_in_progress().await? {
            return Ok(());
        }

        if self.state.is_sync_in_progress().await? {
            return Ok(());
        }

        self.validate_verifier_address().await?;

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
            let proof_da = match proof_msgs.as_slice() {
                [FullInscriptionMessage::ProofDAReference(a)] => a,
                other => return Err(unexpected_msgs(l1_batch_number, "ProofDAReference", other)),
            };

            let (proof_blob, batch_tx_id) = self
                .process_proof_da_reference(proof_da)
                .await
                .map_err(|err| {
                    no_verdict(NoVerdictReason::PackageUnavailable, l1_batch_number, err)
                })?;

            let batch_msgs = self.indexer.parse_transaction(&batch_tx_id).await?;
            let batch_da = match batch_msgs.as_slice() {
                [FullInscriptionMessage::L1BatchDAReference(a)] => a,
                other => {
                    return Err(unexpected_msgs(
                        l1_batch_number,
                        "L1BatchDAReference",
                        other,
                    ))
                }
            };

            tracing::info!(
                "Fetch l1 batch pubdata for blob id  {}",
                batch_da.input.blob_id
            );

            let (batch_blob, _) =
                self.process_batch_da_reference(batch_da)
                    .await
                    .map_err(|err| {
                        no_verdict(NoVerdictReason::PackageUnavailable, l1_batch_number, err)
                    })?;
            let mut pubdata =
                Pubdata::decode_pubdata(batch_blob.data.clone().to_vec()).map_err(|err| {
                    no_verdict(NoVerdictReason::MalformedPackage, l1_batch_number, err)
                })?;

            let upgrade_tx_hash_opt = self.verify_upgrade_tx_hash(storage, &pubdata).await?;

            let mut protocol_version = storage
                .via_protocol_versions_dal()
                .latest_protocol_semantic_version()
                .await
                .context("Failed to load the latest protocol semantic version")?
                .ok_or_else(|| anyhow::anyhow!("Protocol version is missing"))?;

            if upgrade_tx_hash_opt.is_some() {
                // Discard the first log since it related to protocol upgrade.
                pubdata.user_logs.remove(0);

                protocol_version = storage
                    .via_protocol_versions_dal()
                    .latest_semantic_version()
                    .await
                    .context("Failed to load the latest protocol semantic version")?
                    .ok_or_else(|| anyhow::anyhow!("Protocol version is missing"))?;

                // Check if the new protocol version is supported by the verifier node.
                check_if_supported_sequencer_version(protocol_version)?;
            }

            let allowed_versions = storage
                .via_protocol_versions_dal()
                .semantic_versions_of_minor(protocol_version.minor)
                .await?;
            let approval = verify_batch_proof(
                &*self.blob_store,
                self.config.proof_verification_dev_mode,
                &batch_da.input,
                &allowed_versions,
                protocol_version.minor,
                &proof_blob.data,
            )
            .await?;
            tracing::info!("Proof check for l1 batch {l1_batch_number}: {approval:?}");

            let committed = record_approval(
                storage,
                l1_batch_number,
                db_raw_tx_id,
                approval,
                &pubdata,
                upgrade_tx_hash_opt,
                self.indexer.get_number_of_verifiers(),
            )
            .await?;
            if committed {
                METRICS.last_valid_l1_batch.set(l1_batch_number as usize);
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
    /// Check if the wallet is in the verifier set.
    async fn validate_verifier_address(&self) -> anyhow::Result<()> {
        let mut storage = self.pool.connection().await?;

        let last_processed_l1_block = storage
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await?;

        let Some(wallets_map) = storage
            .via_wallet_dal()
            .get_system_wallets_raw(last_processed_l1_block as i64)
            .await?
        else {
            anyhow::bail!("System wallets not found")
        };

        let wallets = SystemWallets::try_from(wallets_map)?;
        wallets.is_valid_verifier_address(self.config.wallet_address()?)
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
    // A package this node could not decode may have been rebuilt by the serving node, so it waits rather than counts as invalid.
    // The Engine API likewise separates missing data (SYNCING) from completed validation, but Via drops its rule that some malformed payloads are INVALID:
    // https://github.com/ethereum/execution-apis/blob/5bcdc34a477b10af278c079525374e6a4046f291/src/engine/paris.md#L175-L179
    let mut prove_batch_data = decode_prove_batch_data(minor, proof_data)
        .and_then(|data| data.check_shape().map(|()| data))
        .map_err(|err| no_verdict(NoVerdictReason::MalformedPackage, number, err))?;

    // The package must name the inscribed batch under the local protocol version, because that version selects the proof lookup.
    // zkSync's Executor matches the whole submitted batch records against stored hashes, commitments included.
    // The inscription carries no commitment, so Via compares only these fields and the commitments stay unbound:
    // https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol#L484-L500
    let claimed = (
        prove_batch_data.claimed_batch(),
        prove_batch_data.protocol_version_id(),
    );
    let expected = (
        (batch, inscribed.l1_batch_hash, inscribed.prev_l1_batch_hash),
        Some(minor),
    );
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
        // A proof is keyed by the exact patch that produced it, so every patch of the batch's minor version is tried, newest first.
        // Upstream zkSync's eth sender also tries allowed versions in turn, but filters them by the verification key authorized on L1.
        // Via keeps one key per minor version, so every patch is eligible, and a proof made with another key fails and records nothing:
        // https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_sender/src/aggregator.rs#L1030-L1050
        let found = match &mut prove_batch_data {
            ProveBatchData::V27(data) => find_wrapped_proof(blob_store, batch, allowed_versions)
                .await
                .map(|proof| proof.map(|proof| data.proofs.push(proof)).is_some()),
            ProveBatchData::V28(data) => find_wrapped_proof(blob_store, batch, allowed_versions)
                .await
                .map(|proof| proof.map(|proof| data.proofs.push(proof)).is_some()),
        };
        // An absent object may still arrive, so the batch waits.
        // An object that exists but will not decode is unusable.
        // Bitcoin Core's VerifyDB draws the same line between data it lacks and data it cannot read:
        // https://github.com/bitcoin/bitcoin/blob/d82283950f5ff3b2116e705f931c6e89e5fdd0be/src/validation.cpp#L4451-L4462
        match found {
            Ok(true) => {}
            // Development acceptance needs an absent proof, because the external-node DA path rewrites `should_verify` from its own config.
            // zkSync's TestnetVerifier also skips only an empty proof and verifies any other, but its empty proof is fixed calldata.
            // Absence here is a local observation, acceptable only because the store fence confines it to regtest, and a proof arriving later is not checked:
            // https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/TestnetVerifier.sol#L14-L33
            Ok(false) if dev_mode => {
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
                return Err(no_verdict(
                    NoVerdictReason::MalformedPackage,
                    number,
                    err.into(),
                ))
            }
            Err(err) => {
                return Err(no_verdict(
                    NoVerdictReason::ProofStoreError,
                    number,
                    err.into(),
                ))
            }
        }
    }

    // A false result shows only that this proof fails for the package's commitments, which nothing binds to the inscription.
    // Lighthouse's proof engine likewise says a proof that does not verify says nothing about the payload it claims to prove:
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

/// Records an approval with its deposit, vote and upgrade effects in one transaction.
/// Returns false when a reorg started meanwhile, in which case nothing is written.
async fn record_approval(
    storage: &mut Connection<'_, Verifier>,
    l1_batch_number: i64,
    proof_reveal_tx_id: H256,
    approval: Approval,
    pubdata: &Pubdata,
    upgrade_tx_hash: Option<H256>,
    number_of_verifiers: usize,
) -> anyhow::Result<bool> {
    let mut transaction = storage.start_transaction().await?;
    // Read in the transaction that records their statuses, next to the reorg check below.
    let deposits = verify_op_priority_id(&mut transaction, l1_batch_number, pubdata).await?;

    let votable_transaction_id = transaction
        .via_votes_dal()
        .verify_votable_transaction(l1_batch_number, proof_reveal_tx_id, true)
        .await?;

    if approval == Approval::DevAccepted {
        transaction
            .via_votes_dal()
            .mark_unverified_dev(votable_transaction_id)
            .await?;
    }

    transaction
        .via_votes_dal()
        .finalize_transaction_if_needed(
            votable_transaction_id,
            BATCH_FINALIZATION_THRESHOLD,
            number_of_verifiers,
        )
        .await?;

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

    if let Some(upgrade_tx_hash) = upgrade_tx_hash {
        transaction
            .via_protocol_versions_dal()
            .mark_upgrade_as_executed(upgrade_tx_hash.as_bytes())
            .await?;
    }

    // Dropping the transaction on a reorg rolls every write above back.
    if transaction
        .via_l1_block_dal()
        .has_reorg_in_progress()
        .await?
        .is_some()
    {
        return Ok(false);
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
) -> anyhow::Result<Vec<(H256, bool)>> {
    let bootloader = H160::from_str(L2_BOOTLOADER_CONTRACT_ADDR)?;
    let deposit_logs: Vec<_> = pubdata
        .user_logs
        .iter()
        .filter(|log| log.sender == bootloader)
        .collect();

    let txs = storage
        .via_transactions_dal()
        .list_transactions_not_processed(deposit_logs.len() as i64)
        .await?;

    if txs.len() != deposit_logs.len() {
        return Err(no_verdict(
            NoVerdictReason::DepositIndexIncomplete,
            l1_batch_number,
            anyhow::anyhow!(
                "Verifier indexed {} unprocessed deposits, pubdata requires {}",
                txs.len(),
                deposit_logs.len(),
            ),
        ));
    }

    txs.iter()
        .zip(deposit_logs)
        .map(|(raw_tx_id, deposit_log)| {
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
            Ok((deposit_log.key, !deposit_log.value.is_zero()))
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
        write!(
            f,
            "no verdict for l1 batch {}: {:?}",
            self.l1_batch_number, self.reason
        )
    }
}

/// Writes nothing, so the next poll selects the same first unverified batch and progress stops there.
/// Citrea's full node instead advances its L1 scan cursor past a proof it fails to process or discards, and does not retry it:
/// https://github.com/chainwayxyz/citrea/blob/f11527f94344d5dc4576ccb9589d5713fb8f7238/crates/fullnode/src/da_block_handler.rs#L299-L351
fn no_verdict(reason: NoVerdictReason, l1_batch_number: i64, err: anyhow::Error) -> anyhow::Error {
    err.context(NoVerdict {
        reason,
        l1_batch_number,
    })
}

/// A transaction always parses to the same messages, so anything but the one expected message never changes on a later poll.
fn unexpected_msgs(
    l1_batch_number: i64,
    expected: &str,
    got: &[FullInscriptionMessage],
) -> anyhow::Error {
    no_verdict(
        NoVerdictReason::MalformedPackage,
        l1_batch_number,
        anyhow::anyhow!("expected one {expected} message, got {got:?}"),
    )
}
