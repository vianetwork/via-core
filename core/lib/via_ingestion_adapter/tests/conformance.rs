// Database rows contain only contract-bounded values created by these fixtures.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, clippy::cast_sign_loss)]

//! Runs the full ingestion acceptance suite against real Postgres.
//!
//! Each test clones a fresh database from the family's migrated template
//! (the same TEST_DATABASE_URL mechanism the DAL test pools use), then
//! drives the production engine, adapter, reader, and probe end to end.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use bitcoin::{hashes::Hash, BlockHash, Network, OutPoint, ScriptBuf, Txid, Wtxid};
use sqlx::{postgres::PgPoolOptions, PgPool};
use via_btc_client::{ingestion_engine::ViaProtocolEngine, test_message_encoder::TestMessageEncoder};
use via_btc_ingestion::{
    AggregateAdapter, ApplyError, BlockAnchor, BlockPlan, Checkpoint, CoverageReport, EffectId, EffectSubject,
    EventKind, EventOrdinal, HardReorgHalt, InfraError, MessageLocation, ProjectionReceipt, ProtocolContext,
    RawTxVariant, RejectionCode, ReorgImpact, RevertError, RevertReceipt, Role,
};
use via_btc_ingestion_tests::{
    ApplyFaultPoint, BatchFact, DepositFact, InclusionRecord, ProofFact, RejectionRecord, RevertFaultPoint, StateProbe,
    TestHarness, TrackedOutputState, VoteFact, WithdrawalFact,
};
use via_ingestion_adapter::{FaultHook, PgIngestionAdapter, PgObservationReader, Stage};

static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct FaultState {
    armed: Mutex<Option<Stage>>,
    reached: AtomicBool,
}

impl FaultState {
    fn hook(self: &Arc<Self>) -> FaultHook {
        let state = Arc::clone(self);
        Arc::new(move |stage| {
            let armed = *state.armed.lock().unwrap();
            if armed == Some(stage) {
                state.reached.store(true, Ordering::SeqCst);
                return true;
            }
            false
        })
    }
}

fn apply_stage(point: ApplyFaultPoint) -> Stage {
    match point {
        ApplyFaultPoint::AfterRawVariants => Stage::AfterRawVariants,
        ApplyFaultPoint::AfterInclusions => Stage::AfterInclusions,
        ApplyFaultPoint::AfterTrackedOutputs => Stage::AfterTrackedOutputs,
        ApplyFaultPoint::AfterDomainProjections => Stage::AfterDomainProjections,
        ApplyFaultPoint::AfterProtocolContext => Stage::AfterProtocolContext,
        ApplyFaultPoint::AfterCheckpointUpdate => Stage::AfterCheckpointUpdate,
        ApplyFaultPoint::BeforeCommit => Stage::BeforeCommit,
        ApplyFaultPoint::AfterCommitBeforeAck => Stage::AfterCommitBeforeAck,
    }
}

fn revert_stage(point: RevertFaultPoint) -> Stage {
    match point {
        RevertFaultPoint::AfterCanonicalUnwind => Stage::RevertAfterCanonicalUnwind,
        RevertFaultPoint::AfterProjectionUnwind => Stage::RevertAfterProjectionUnwind,
        RevertFaultPoint::AfterContextRestore => Stage::RevertAfterContextRestore,
        RevertFaultPoint::AfterCheckpointRestore => Stage::RevertAfterCheckpointRestore,
        RevertFaultPoint::BeforeCommit => Stage::RevertBeforeCommit,
        RevertFaultPoint::AfterCommitBeforeAck => Stage::RevertAfterCommitBeforeAck,
    }
}

/// A production adapter bound to its own throwaway database.
pub struct DbAdapter {
    inner: PgIngestionAdapter,
    url: String,
    role: Role,
}

#[async_trait::async_trait]
impl AggregateAdapter for DbAdapter {
    async fn load_checkpoint_and_context(&self) -> Result<Option<(Checkpoint, ProtocolContext)>, InfraError> {
        self.inner.load_checkpoint_and_context().await
    }
    async fn apply_block(
        &self, expected_checkpoint: Option<Checkpoint>, plan: &BlockPlan,
    ) -> Result<ProjectionReceipt, ApplyError> {
        self.inner.apply_block(expected_checkpoint, plan).await
    }
    async fn revert_to(
        &self, expected_checkpoint: Checkpoint, ancestor: BlockAnchor,
    ) -> Result<RevertReceipt, RevertError> {
        self.inner.revert_to(expected_checkpoint, ancestor).await
    }
    async fn downstream_consumed(&self, effects: &[EffectId]) -> Result<Vec<EffectId>, InfraError> {
        self.inner.downstream_consumed(effects).await
    }
    async fn record_hard_reorg_halt(&self, halt: HardReorgHalt) -> Result<(), InfraError> {
        self.inner.record_hard_reorg_halt(halt).await
    }
    async fn hard_reorg_halt(&self) -> Result<Option<HardReorgHalt>, InfraError> {
        self.inner.hard_reorg_halt().await
    }
    async fn audit_coverage(&self, from_height: u64, to_height: u64) -> Result<CoverageReport, InfraError> {
        self.inner.audit_coverage(from_height, to_height).await
    }
}

pub struct PgHarness {
    fault: Arc<FaultState>,
}

fn template_url(role: Role) -> String {
    let specific = match role {
        Role::CoreSequencer => std::env::var("TEST_DATABASE_URL"),
        Role::Verifier => std::env::var("TEST_DATABASE_VERIFIER_URL"),
        Role::StandaloneIndexer => std::env::var("TEST_DATABASE_INDEXER_URL"),
    };
    specific
        .or_else(|_| std::env::var("TEST_DATABASE_URL"))
        .expect("set TEST_DATABASE_URL (and optionally the verifier/indexer variants) to migrated template databases")
}

fn split_db(url: &str) -> (String, String) {
    let (base, db) = url.rsplit_once('/').expect("database URL has a path");
    (base.to_string(), db.to_string())
}

async fn admin_pool(base: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .connect(&format!("{base}/postgres"))
        .await
        .expect("connect to admin database")
}

async fn clone_template(role: Role) -> String {
    let url = template_url(role);
    let (base, template) = split_db(&url);
    let name = format!("via_ing_conf_{}_{}", std::process::id(), DB_COUNTER.fetch_add(1, Ordering::SeqCst));
    let admin = admin_pool(&base).await;
    for attempt in 0..20 {
        let res = sqlx::query(&format!("CREATE DATABASE {name} TEMPLATE {template}")).execute(&admin).await;
        match res {
            Ok(_) => break,
            Err(e) if attempt < 19 && e.to_string().contains("being accessed") => {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            Err(e) => panic!("CREATE DATABASE from template failed: {e}"),
        }
    }
    format!("{base}/{name}")
}

async fn pool_for(url: &str) -> PgPool {
    PgPoolOptions::new().max_connections(4).connect(url).await.expect("connect to test database")
}

#[async_trait::async_trait]
impl TestHarness for PgHarness {
    type Engine = ViaProtocolEngine;
    type Adapter = DbAdapter;
    type Reader = PgObservationReader;
    type Probe = PgProbe;
    type Builder = TestMessageEncoder;

    async fn engine(&self) -> Self::Engine {
        ViaProtocolEngine::new(Network::Regtest)
    }

    async fn fresh_adapter(&self, role: Role) -> Self::Adapter {
        let url = clone_template(role).await;
        let pool = pool_for(&url).await;
        DbAdapter { inner: PgIngestionAdapter::with_fault_hook(pool, role, self.fault.hook()), url, role }
    }

    async fn restart(&self, adapter: Self::Adapter) -> Self::Adapter {
        let DbAdapter { inner, url, role } = adapter;
        drop(inner);
        let pool = pool_for(&url).await;
        DbAdapter { inner: PgIngestionAdapter::with_fault_hook(pool, role, self.fault.hook()), url, role }
    }

    async fn observation_reader(&self, adapter: &Self::Adapter) -> Self::Reader {
        PgObservationReader::new(adapter.inner.pool().clone())
    }

    async fn probe(&self, adapter: &Self::Adapter) -> Self::Probe {
        PgProbe { pool: adapter.inner.pool().clone() }
    }

    fn message_builder(&self) -> Self::Builder {
        TestMessageEncoder::new(Network::Regtest)
    }

    async fn arm_apply_fault(&self, point: ApplyFaultPoint) {
        self.fault.reached.store(false, Ordering::SeqCst);
        *self.fault.armed.lock().unwrap() = Some(apply_stage(point));
    }

    async fn arm_revert_fault(&self, point: RevertFaultPoint) {
        self.fault.reached.store(false, Ordering::SeqCst);
        *self.fault.armed.lock().unwrap() = Some(revert_stage(point));
    }

    async fn clear_fault(&self) {
        *self.fault.armed.lock().unwrap() = None;
    }

    async fn fault_stage_reached(&self) -> bool {
        self.fault.reached.load(Ordering::SeqCst)
    }

    async fn historical_rpc_count(&self) -> u64 {
        // The production engine holds no Bitcoin client at all; a historical
        // RPC is unrepresentable, so the spy count is truthfully zero.
        0
    }

    async fn reset_historical_rpc_count(&self) {}

    async fn delete_raw_variants(&self, adapter: &Self::Adapter, txid: &Txid) {
        sqlx::query("DELETE FROM via_ingestion_raw_variants WHERE txid = $1")
            .bind(txid.as_byte_array().to_vec())
            .execute(adapter.inner.pool())
            .await
            .expect("delete raw variants");
    }

    async fn corrupt_raw_variants(&self, adapter: &Self::Adapter, txid: &Txid) {
        sqlx::query("UPDATE via_ingestion_raw_variants SET raw = raw || '\\x00'::bytea WHERE txid = $1")
            .bind(txid.as_byte_array().to_vec())
            .execute(adapter.inner.pool())
            .await
            .expect("corrupt raw variants");
    }

    async fn mark_downstream_consumed(&self, adapter: &Self::Adapter, effect: EffectId) {
        via_ingestion_adapter::mark_consumed(adapter.inner.pool(), &effect).await.expect("mark consumed");
    }

    async fn run_reorg_coordinator(&self, adapter: &Self::Adapter, replacement_branch: &[BlockAnchor]) -> ReorgImpact {
        let (checkpoint, _) = adapter
            .load_checkpoint_and_context()
            .await
            .expect("load checkpoint")
            .expect("coordinator needs a processed checkpoint");
        let first = replacement_branch.first().expect("non-empty replacement branch");
        if first.height > checkpoint.height {
            return ReorgImpact::NoProcessedImpact;
        }
        let ancestor_row: Option<(i64, Vec<u8>, Vec<u8>, i64)> = sqlx::query_as(
            "SELECT height, block_hash, prev_hash, header_time FROM via_ingestion_chain \
             WHERE block_hash = $1",
        )
        .bind(first.prev_hash.as_byte_array().to_vec())
        .fetch_optional(adapter.inner.pool())
        .await
        .expect("ancestor lookup");
        let Some((height, hash, prev, time)) = ancestor_row else {
            return ReorgImpact::AncestorUnavailableOrBeyondPolicy;
        };
        let ancestor = BlockAnchor {
            height: height as u64,
            hash: BlockHash::from_byte_array(hash.try_into().expect("hash")),
            prev_hash: BlockHash::from_byte_array(prev.try_into().expect("hash")),
            time: time as u32,
        };

        let consumed_rows: Vec<(Vec<u8>,)> = sqlx::query_as(
            "SELECT e.effect_key FROM via_ingestion_consumed_effects e \
             JOIN via_ingestion_chain c ON c.block_hash = e.block_hash WHERE c.height > $1",
        )
        .bind(ancestor.height as i64)
        .fetch_all(adapter.inner.pool())
        .await
        .expect("consumed lookup");
        let candidates: Vec<EffectId> = consumed_rows.iter().map(|(key,)| effect_from_key(key)).collect();
        let affected = adapter.downstream_consumed(&candidates).await.expect("boundary check");
        if !affected.is_empty() {
            let halt = HardReorgHalt { divergence: *first, affected: affected.clone() };
            adapter.record_hard_reorg_halt(halt).await.expect("record halt");
            return ReorgImpact::DownstreamConsumed { ancestor, affected };
        }
        adapter.revert_to(checkpoint, ancestor).await.expect("projection-only revert");
        ReorgImpact::ProjectionOnly { ancestor }
    }
}

pub struct PgProbe {
    pool: PgPool,
}

fn ordinal_from(tx_index: i64, tag: i16, index: i64) -> EventOrdinal {
    let location = match tag {
        0 => MessageLocation::Input(index as u32),
        _ => MessageLocation::Output(index as u32),
    };
    EventOrdinal { tx_index: tx_index as u32, location }
}

fn rejection_code_from(tag: i16) -> RejectionCode {
    match tag {
        0 => RejectionCode::MalformedMessage,
        1 => RejectionCode::ConflictingReceiverEncodings,
        2 => RejectionCode::ConflictingRoleUpdate,
        3 => RejectionCode::Unauthorized,
        4 => RejectionCode::InvalidBootstrap,
        5 => RejectionCode::NonMonotonicUpgrade,
        _ => RejectionCode::InvalidReference,
    }
}

fn bh(bytes: Vec<u8>) -> BlockHash {
    BlockHash::from_byte_array(bytes.try_into().expect("32-byte hash"))
}

fn tid(bytes: Vec<u8>) -> Txid {
    Txid::from_byte_array(bytes.try_into().expect("32-byte txid"))
}

fn effect_from_key(mut key: &[u8]) -> EffectId {
    fn take_u32(input: &mut &[u8]) -> u32 {
        let (value, rest) = input.split_at(4);
        *input = rest;
        u32::from_be_bytes(value.try_into().unwrap())
    }
    fn take_hash(input: &mut &[u8]) -> [u8; 32] {
        let (value, rest) = input.split_at(32);
        *input = rest;
        value.try_into().unwrap()
    }

    assert_eq!(take_u32(&mut key), 1, "effect key version");
    let block_hash = BlockHash::from_byte_array(take_hash(&mut key));
    let tx_index = take_u32(&mut key);
    let location_tag = take_u32(&mut key);
    let location_index = take_u32(&mut key);
    let location = match location_tag {
        0 => MessageLocation::Input(location_index),
        1 => MessageLocation::Output(location_index),
        other => panic!("unknown message location tag {other}"),
    };
    let kind = match take_u32(&mut key) {
        0 => EventKind::Deposit,
        1 => EventKind::L1BatchDAReference,
        2 => EventKind::ProofDAReference,
        3 => EventKind::ValidatorAttestation,
        4 => EventKind::SystemBootstrapping,
        5 => EventKind::SystemContractUpgradeProposal,
        6 => EventKind::SystemContractUpgradeActivation,
        7 => EventKind::BridgeWithdrawal,
        8 => EventKind::UpdateBridgeProposal,
        9 => EventKind::WalletRotation,
        other => panic!("unknown event kind tag {other}"),
    };
    let subject_tag = take_u32(&mut key);
    let subject_txid = Txid::from_byte_array(take_hash(&mut key));
    let subject = match subject_tag {
        0 => EffectSubject::Output(OutPoint { txid: subject_txid, vout: take_u32(&mut key) }),
        1 => EffectSubject::Tx(subject_txid),
        other => panic!("unknown effect subject tag {other}"),
    };
    assert!(key.is_empty(), "trailing effect key bytes");
    EffectId { block_hash, ordinal: EventOrdinal { tx_index, location }, kind, subject }
}

#[async_trait::async_trait]
impl StateProbe for PgProbe {
    async fn checkpoint(&self) -> Option<Checkpoint> {
        let row: Option<(i64, Vec<u8>, i32, i32, Vec<u8>, Vec<u8>)> = sqlx::query_as(
            "SELECT height, block_hash, kernel_version, observation_rule_version, \
             context_hash, last_plan_hash FROM via_ingestion_checkpoint WHERE id",
        )
        .fetch_optional(&self.pool)
        .await
        .expect("checkpoint probe");
        row.map(|(height, hash, kv, orv, ctx, plan)| Checkpoint {
            height: height as u64,
            hash: bh(hash),
            kernel_version: via_btc_ingestion::KernelVersion(kv as u32),
            observation_rule_version: via_btc_ingestion::ObservationRuleVersion(orv as u32),
            context_hash: ctx.try_into().expect("32 bytes"),
            last_plan_hash: plan.try_into().expect("32 bytes"),
        })
    }

    async fn protocol_context(&self) -> Option<ProtocolContext> {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT context_blob FROM via_ingestion_checkpoint WHERE id")
                .fetch_optional(&self.pool)
                .await
                .expect("context probe");
        row.map(|(blob,)| serde_json::from_value(blob).expect("context decodes"))
    }

    async fn raw_variants(&self, txid: &Txid) -> Vec<RawTxVariant> {
        let rows: Vec<(Vec<u8>,)> = sqlx::query_as("SELECT raw FROM via_ingestion_raw_variants WHERE txid = $1")
            .bind(txid.as_byte_array().to_vec())
            .fetch_all(&self.pool)
            .await
            .expect("raw variants probe");
        rows.into_iter().filter_map(|(raw,)| RawTxVariant::from_raw(raw).ok()).collect()
    }

    async fn inclusions(&self, txid: &Txid) -> Vec<InclusionRecord> {
        let rows: Vec<(Vec<u8>, i64, i64, Vec<u8>, bool)> = sqlx::query_as(
            "SELECT block_hash, height, tx_index, wtxid, canonical \
             FROM via_ingestion_inclusions WHERE txid = $1 ORDER BY height, tx_index",
        )
        .bind(txid.as_byte_array().to_vec())
        .fetch_all(&self.pool)
        .await
        .expect("inclusions probe");
        rows.into_iter()
            .map(|(block, height, tx_index, wtxid, canonical)| InclusionRecord {
                inclusion: via_btc_ingestion::Inclusion {
                    block_hash: bh(block),
                    height: height as u64,
                    tx_index: tx_index as u32,
                    txid: *txid,
                    wtxid: Wtxid::from_byte_array(wtxid.try_into().expect("32 bytes")),
                },
                canonical,
            })
            .collect()
    }

    async fn tracked_output(&self, outpoint: &OutPoint) -> Option<TrackedOutputState> {
        let row: Option<(i64, Vec<u8>, i16, Option<Vec<u8>>)> = sqlx::query_as(
            "SELECT value_sat, script, role, canonical_spender_txid \
             FROM via_ingestion_tracked_outputs WHERE txid = $1 AND vout = $2",
        )
        .bind(outpoint.txid.as_byte_array().to_vec())
        .bind(outpoint.vout as i64)
        .fetch_optional(&self.pool)
        .await
        .expect("tracked output probe");
        let (value, script, role, spender) = row?;
        let spenders: Vec<(Vec<u8>,)> = sqlx::query_as(
            "SELECT DISTINCT spending_txid FROM via_ingestion_tracked_spends \
             WHERE txid = $1 AND vout = $2",
        )
        .bind(outpoint.txid.as_byte_array().to_vec())
        .bind(outpoint.vout as i64)
        .fetch_all(&self.pool)
        .await
        .expect("spenders probe");
        Some(TrackedOutputState {
            create: via_btc_ingestion::TrackedOutputCreate {
                outpoint: *outpoint,
                value: bitcoin::Amount::from_sat(value as u64),
                script_pubkey: ScriptBuf::from_bytes(script),
                role: match role {
                    0 => via_btc_ingestion::TrackedRole::Bridge,
                    1 => via_btc_ingestion::TrackedRole::Sequencer,
                    _ => via_btc_ingestion::TrackedRole::Governance,
                },
            },
            canonical_spender: spender.map(tid),
            observed_spenders: spenders.into_iter().map(|(s,)| tid(s)).collect(),
        })
    }

    async fn canonical_deposits(&self) -> Vec<DepositFact> {
        let rows: Vec<(
            Vec<u8>,
            i64,
            i64,
            i64,
            Vec<u8>,
            i64,
            i64,
            Vec<u8>,
            Vec<u8>,
            Vec<u8>,
            Option<Vec<u8>>,
            Vec<u8>,
        )> = sqlx::query_as(
            "SELECT block_hash, height, header_time, ordinal_tx_index, subject_txid, \
                 subject_vout, amount_sat, receiver, l2_contract, call_data, sender_script, \
                 source_wtxid FROM via_ingestion_deposits ORDER BY height, ordinal_tx_index, subject_vout",
        )
        .fetch_all(&self.pool)
        .await
        .expect("deposits probe");
        rows.into_iter()
            .map(|(block, height, time, tx_index, subject_txid, vout, amount, receiver, l2c, cd, sender, wtxid)| {
                DepositFact {
                    subject: OutPoint { txid: tid(subject_txid), vout: vout as u32 },
                    amount_sat: amount as u64,
                    receiver,
                    l2_contract: l2c.try_into().expect("20 bytes"),
                    call_data: cd,
                    sender_script: sender.map(ScriptBuf::from_bytes),
                    block_height: height as u64,
                    block_hash: bh(block),
                    tx_index: tx_index as u32,
                    header_time: time as u32,
                    source_wtxid: Wtxid::from_byte_array(wtxid.try_into().expect("32 bytes")),
                }
            })
            .collect()
    }

    async fn rejections(&self) -> Vec<RejectionRecord> {
        let rows: Vec<(Vec<u8>, i64, i16, i64, i16)> = sqlx::query_as(
            "SELECT block_hash, ordinal_tx_index, ordinal_location_tag, ordinal_location_index, \
             code FROM via_ingestion_rejections",
        )
        .fetch_all(&self.pool)
        .await
        .expect("rejections probe");
        rows.into_iter()
            .map(|(block, tx_index, tag, index, code)| RejectionRecord {
                block_hash: bh(block),
                ordinal: ordinal_from(tx_index, tag, index),
                code: rejection_code_from(code),
            })
            .collect()
    }

    async fn attestation_votes(&self) -> Vec<VoteFact> {
        let rows: Vec<(Vec<u8>, i64, i16, i64, Vec<u8>, bool, i64)> = sqlx::query_as(
            "SELECT block_hash, ordinal_tx_index, ordinal_location_tag, ordinal_location_index, \
             attester_script, ok, l1_batch_index FROM via_ingestion_votes \
             ORDER BY ordinal_tx_index, ordinal_location_tag, ordinal_location_index",
        )
        .fetch_all(&self.pool)
        .await
        .expect("votes probe");
        rows.into_iter()
            .map(|(block, tx_index, tag, location_index, attester, ok, index)| VoteFact {
                ordinal: ordinal_from(tx_index, tag, location_index),
                l1_batch_index: index as u64,
                attester_script: ScriptBuf::from_bytes(attester),
                ok,
                block_hash: bh(block),
            })
            .collect()
    }

    async fn proof_references(&self) -> Vec<ProofFact> {
        let rows: Vec<(Vec<u8>, i64, String)> =
            sqlx::query_as("SELECT subject_txid, l1_batch_index, blob_id FROM via_ingestion_proof_refs")
                .fetch_all(&self.pool)
                .await
                .expect("proof refs probe");
        rows.into_iter()
            .map(|(subject, index, blob)| ProofFact {
                subject_txid: tid(subject),
                l1_batch_index: index as u64,
                blob_id: blob,
            })
            .collect()
    }

    async fn batch_references(&self) -> Vec<BatchFact> {
        let rows: Vec<(Vec<u8>, i64, Vec<u8>)> =
            sqlx::query_as("SELECT subject_txid, l1_batch_index, l1_batch_hash FROM via_ingestion_batch_refs")
                .fetch_all(&self.pool)
                .await
                .expect("batch refs probe");
        rows.into_iter()
            .map(|(subject, index, hash)| BatchFact {
                subject_txid: tid(subject),
                l1_batch_index: index as u64,
                l1_batch_hash: hash.try_into().expect("32 bytes"),
            })
            .collect()
    }

    async fn applied_protocol_versions(&self) -> Vec<via_btc_ingestion::ProtocolVersionTag> {
        let rows: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT pv.version_minor, pv.version_patch FROM via_ingestion_protocol_versions pv \
             JOIN via_ingestion_chain c ON c.block_hash = pv.block_hash ORDER BY c.height",
        )
        .fetch_all(&self.pool)
        .await
        .expect("versions probe");
        rows.into_iter()
            .map(|(minor, patch)| via_btc_ingestion::ProtocolVersionTag { minor: minor as u32, patch: patch as u32 })
            .collect()
    }

    async fn bridge_withdrawals(&self) -> Vec<WithdrawalFact> {
        let rows: Vec<(Vec<u8>, Vec<u8>, i32, Vec<u8>, i64)> = sqlx::query_as(
            "SELECT subject_txid, l2_id, l2_tx_event_index, receiver_script, amount_sat \
             FROM via_ingestion_withdrawals ORDER BY subject_txid, withdrawal_index",
        )
        .fetch_all(&self.pool)
        .await
        .expect("withdrawals probe");
        rows.into_iter()
            .map(|(subject, l2_id, _index, receiver, amount)| WithdrawalFact {
                subject_txid: tid(subject),
                l2_id: l2_id.try_into().expect("8 bytes"),
                receiver_script: ScriptBuf::from_bytes(receiver),
                amount_sat: amount as u64,
            })
            .collect()
    }

    async fn canonical_revision(&self) -> u64 {
        let row: Option<(i64,)> = sqlx::query_as("SELECT canonical_revision FROM via_ingestion_checkpoint WHERE id")
            .fetch_optional(&self.pool)
            .await
            .expect("revision probe");
        row.map(|(r,)| r as u64).unwrap_or(0)
    }
}

async fn harness() -> PgHarness {
    PgHarness { fault: Arc::new(FaultState::default()) }
}

#[tokio::test]
async fn two_concurrent_genesis_applies() {
    let harness = harness().await;
    let database = harness.fresh_adapter(Role::CoreSequencer).await;
    let first_envelope = via_btc_ingestion_tests::envelopes::envelope(
        via_btc_ingestion_tests::envelopes::anchor(via_btc_ingestion_tests::START_HEIGHT, via_btc_ingestion_tests::T0),
        vec![],
    );
    let second_envelope = via_btc_ingestion_tests::envelopes::envelope(
        via_btc_ingestion_tests::envelopes::fork_anchor(
            7,
            via_btc_ingestion_tests::START_HEIGHT + 7,
            via_btc_ingestion_tests::envelopes::branch_hash(7, via_btc_ingestion_tests::START_HEIGHT + 6),
            via_btc_ingestion_tests::T0 + 4200,
        ),
        vec![],
    );
    let context = via_btc_ingestion_tests::default_context();
    let first_plan = via_btc_ingestion_tests::plan_block(&harness, &database, &first_envelope, &context).await;
    let second_plan = via_btc_ingestion_tests::plan_block(&harness, &database, &second_envelope, &context).await;

    let pool = database.inner.pool().clone();
    let first = PgIngestionAdapter::new(pool.clone(), Role::CoreSequencer);
    let second = PgIngestionAdapter::new(pool, Role::CoreSequencer);
    let (first_result, second_result) =
        tokio::join!(first.apply_block(None, &first_plan), second.apply_block(None, &second_plan),);

    let winner = match (&first_result, &second_result) {
        (Ok(_), Err(ApplyError::StaleCheckpoint { .. })) => &first_plan,
        (Err(ApplyError::StaleCheckpoint { .. }), Ok(_)) => &second_plan,
        other => panic!("exactly one genesis apply must win and the loser must be stale: {other:?}"),
    };
    let (checkpoint, _) =
        first.load_checkpoint_and_context().await.expect("load checkpoint").expect("winner checkpoint");
    assert_eq!((checkpoint.height, checkpoint.hash), (winner.anchor.height, winner.anchor.hash));
    assert_eq!(checkpoint.last_plan_hash, winner.plan_hash().expect("winner plan hash"));

    let chain: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT height, block_hash FROM via_ingestion_chain ORDER BY height")
            .fetch_all(first.pool())
            .await
            .expect("query chain");
    assert_eq!(chain.len(), 1, "the losing genesis apply must leave no chain row");
    assert_eq!(chain[0].0 as u64, winner.anchor.height);
    assert_eq!(chain[0].1, winner.anchor.hash.as_byte_array().to_vec());
}

via_btc_ingestion_tests::ingestion_conformance_suite!(harness);
