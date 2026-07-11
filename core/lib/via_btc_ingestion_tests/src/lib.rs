//! Acceptance fixture suite for the Via ingestion kernel.
//!
//! This crate is the acceptance contract described in
//! `docs/ingestion/ingestion-kernel.md` (the suite is listed there under these
//! same function names). An implementation under test provides a
//! [`TestHarness`]; each fixture is a generic async function driving the
//! implementation exclusively through the `via_btc_ingestion` traits plus the
//! read-back [`StateProbe`] defined here. Fixtures assert logical visibility
//! and state invariants, never a particular SQL statement order.
//!
//! Register the suite with [`ingestion_conformance_suite!`]: an
//! implementation that merely compiles against these functions without
//! registering them has not passed anything.
//!
//! Scope notes (documented simplifications, tightened as later slices land):
//! - The deposit wire format pinned here is: an output paying the bridge
//!   script, plus one OP_RETURN output carrying a 20-byte L2 receiver.
//!   `deposit_per_output_and_op_return_rules` pins the per-output subject
//!   rule and OP_RETURN agreement/disagreement only; the real
//!   inscription+OP_RETURN precedence fixture becomes mandatory with the
//!   inscription parser slice and is NOT covered here.
//! - Witness-payload divergence is exercised at identity and provenance
//!   level (`same_txid_different_witness_remine` checks the canonical
//!   deposit's `source_wtxid`); parse-level divergence activates with
//!   inscription parsing.
//! - `unsupported valid event` has no constructible representative in the
//!   deposit slice's wire format; its composition fixture lands with the
//!   first versioned event family.
//! - Real process-kill / connection-drop faults and the end-to-end
//!   `txindex=0` pruned-regtest run belong to the per-adapter conformance
//!   layer and the source-shell slice respectively; this generic suite
//!   cannot express them and does not claim them.

use std::collections::BTreeMap;

use async_trait::async_trait;
use bitcoin::{hashes::Hash, Amount, OutPoint, Txid, Wtxid};
use via_btc_ingestion::{
    AggregateAdapter, ApplyError, BitcoinBlockEnvelope, BlockAnchor, BlockPlan, Checkpoint, DependencyKey,
    DispositionKind, EffectId, FinalizeFailure, FinalizeOutcome, Hash32, Inclusion, MessageLocation,
    ObservationReadError, ObservationReader, ProtocolContext, ProtocolEngine, RejectionCode, ReorgImpact, Resolution,
    ResolvedDependency, Role, TrackedOutputCreate,
};

/// Semantic crash boundaries of `apply_block` every adapter must be able to
/// fault-inject. Includes the boundary an injected `Err` alone cannot
/// exercise: commit succeeded, acknowledgement lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ApplyFaultPoint {
    AfterRawVariants,
    AfterInclusions,
    AfterTrackedOutputs,
    AfterDomainProjections,
    AfterProtocolContext,
    AfterCheckpointUpdate,
    BeforeCommit,
    /// The database transaction commits, but the caller never learns it.
    AfterCommitBeforeAck,
}

pub const ALL_APPLY_FAULT_POINTS: [ApplyFaultPoint; 8] = [
    ApplyFaultPoint::AfterRawVariants,
    ApplyFaultPoint::AfterInclusions,
    ApplyFaultPoint::AfterTrackedOutputs,
    ApplyFaultPoint::AfterDomainProjections,
    ApplyFaultPoint::AfterProtocolContext,
    ApplyFaultPoint::AfterCheckpointUpdate,
    ApplyFaultPoint::BeforeCommit,
    ApplyFaultPoint::AfterCommitBeforeAck,
];

/// Semantic crash boundaries of `revert_to` (its stages differ from apply's).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RevertFaultPoint {
    AfterCanonicalUnwind,
    AfterProjectionUnwind,
    AfterContextRestore,
    AfterCheckpointRestore,
    BeforeCommit,
    AfterCommitBeforeAck,
}

pub const ALL_REVERT_FAULT_POINTS: [RevertFaultPoint; 6] = [
    RevertFaultPoint::AfterCanonicalUnwind,
    RevertFaultPoint::AfterProjectionUnwind,
    RevertFaultPoint::AfterContextRestore,
    RevertFaultPoint::AfterCheckpointRestore,
    RevertFaultPoint::BeforeCommit,
    RevertFaultPoint::AfterCommitBeforeAck,
];

/// One inclusion occurrence as stored, with its canonicity flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InclusionRecord {
    pub inclusion: Inclusion,
    pub canonical: bool,
}

/// Stored state of a tracked output, including spend history: orphaned
/// spend observations remain visible even though only one spender is
/// canonical.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackedOutputState {
    pub create: TrackedOutputCreate,
    /// Set iff a canonical transaction spends it.
    pub canonical_spender: Option<Txid>,
    /// Every transaction ever observed spending it, on any branch.
    pub observed_spenders: Vec<Txid>,
}

/// Normalized deposit fact. [`three_adapter_semantic_equivalence`] compares
/// these across the sequencer, verifier, and indexer adapters: same committed
/// plans must yield the same facts, whatever each schema looks like.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DepositFact {
    pub subject: OutPoint,
    pub amount_sat: u64,
    pub receiver: Vec<u8>,
    /// Target L2 contract; all-zero means the protocol default.
    pub l2_contract: [u8; 20],
    /// L2 call data; empty for plain value transfers.
    pub call_data: Vec<u8>,
    /// The depositor's identifiable P2WPKH input script, when there is one.
    pub sender_script: Option<bitcoin::ScriptBuf>,
    pub block_height: u64,
    pub block_hash: bitcoin::BlockHash,
    pub tx_index: u32,
    /// Bitcoin header time of the containing block, never wall-clock.
    pub header_time: u32,
    /// The wtxid of the raw variant this projection was parsed from. It
    /// proves an orphaned witness variant's parse was never reused.
    pub source_wtxid: Wtxid,
}

/// A durably stored rejection record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectionRecord {
    pub block_hash: bitcoin::BlockHash,
    pub ordinal: via_btc_ingestion::EventOrdinal,
    pub code: RejectionCode,
}

/// One recorded attestation vote, normalized across schema families.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct VoteFact {
    pub ordinal: via_btc_ingestion::EventOrdinal,
    pub l1_batch_index: u64,
    pub attester_script: bitcoin::ScriptBuf,
    pub ok: bool,
    pub block_hash: bitcoin::BlockHash,
}

/// One committed proof DA reference, normalized.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProofFact {
    pub subject_txid: Txid,
    pub l1_batch_index: u64,
    pub blob_id: String,
}

/// One committed batch DA reference, normalized.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BatchFact {
    pub subject_txid: Txid,
    pub l1_batch_index: u64,
    pub l1_batch_hash: Hash32,
}

/// One projected bridge withdrawal, normalized.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WithdrawalFact {
    pub subject_txid: Txid,
    pub l2_id: [u8; 8],
    pub receiver_script: bitcoin::ScriptBuf,
    pub amount_sat: u64,
}

/// Read-back surface over one adapter's durable state. Implementations back
/// this with plain queries against their own schema; fixtures use it for
/// assertions only, never to drive state.
#[async_trait]
pub trait StateProbe: Send + Sync {
    async fn checkpoint(&self) -> Option<Checkpoint>;
    async fn protocol_context(&self) -> Option<ProtocolContext>;
    async fn raw_variants(&self, txid: &Txid) -> Vec<via_btc_ingestion::RawTxVariant>;
    async fn inclusions(&self, txid: &Txid) -> Vec<InclusionRecord>;
    async fn tracked_output(&self, outpoint: &OutPoint) -> Option<TrackedOutputState>;
    async fn canonical_deposits(&self) -> Vec<DepositFact>;
    async fn rejections(&self) -> Vec<RejectionRecord>;
    async fn attestation_votes(&self) -> Vec<VoteFact>;
    async fn proof_references(&self) -> Vec<ProofFact>;
    async fn batch_references(&self) -> Vec<BatchFact>;
    /// Every protocol version ever applied, in application order.
    async fn applied_protocol_versions(&self) -> Vec<via_btc_ingestion::ProtocolVersionTag>;
    async fn bridge_withdrawals(&self) -> Vec<WithdrawalFact>;
    /// Monotonic counter bumped by every completed revert (the reader-visible
    /// revision signal; must match the last `RevertReceipt`).
    async fn canonical_revision(&self) -> u64;
}

/// Concrete Via message transactions for the family fixtures. The suite
/// stays free of inscription encoding; each harness supplies real encodings
/// through this trait. Every method returns a transaction spending `prev`
/// whose first message carrier decodes to the described message.
pub trait MessageTxBuilder: Send + Sync {
    /// The wallet set this builder can produce signatures for. Family
    /// fixtures bootstrap the chain with exactly these wallets.
    fn wallet_set(&self) -> via_btc_ingestion::WalletSet;
    /// Genesis message establishing `wallets` and `version`.
    fn bootstrap_tx(
        &self, wallets: &via_btc_ingestion::WalletSet, version: via_btc_ingestion::ProtocolVersionTag, prev: OutPoint,
    ) -> bitcoin::Transaction;
    /// Sequencer-signed batch DA reference.
    fn batch_da_reference_tx(
        &self, l1_batch_index: u64, l1_batch_hash: Hash32, blob_id: &str, prev: OutPoint,
    ) -> bitcoin::Transaction;
    /// Sequencer-signed proof DA reference naming the batch reveal tx.
    fn proof_da_reference_tx(&self, l1_batch_reveal_txid: Txid, blob_id: &str, prev: OutPoint) -> bitcoin::Transaction;
    /// Attestation signed by the verifier at `attester_index` in the wallet set.
    fn attestation_tx(
        &self, reference_txid: Txid, ok: bool, attester_index: usize, prev: OutPoint,
    ) -> bitcoin::Transaction;
    /// Several verifier attestations carried by one transaction. Each tuple
    /// supplies the vote, verifier index, and preceding signer input.
    fn attestations_tx(&self, reference_txid: Txid, attestations: &[(bool, usize, OutPoint)]) -> bitcoin::Transaction;
    /// Attestation signed by a wallet outside the verifier set.
    fn unauthorized_attestation_tx(&self, reference_txid: Txid, prev: OutPoint) -> bitcoin::Transaction;
    /// Upgrade proposal for `version`.
    fn upgrade_proposal_tx(
        &self, version: via_btc_ingestion::ProtocolVersionTag, system_contracts: Vec<([u8; 20], Hash32)>,
        prev: OutPoint,
    ) -> bitcoin::Transaction;
    /// Governance activation of `proposal_txid`; `gov_prev` must spend a
    /// governance-tracked output for the activation to be authorized.
    fn upgrade_activation_tx(&self, proposal_txid: Txid, gov_prev: OutPoint) -> bitcoin::Transaction;
    /// Governance-authorized sequencer rotation to `new_script`.
    fn sequencer_rotation_tx(&self, new_script: &bitcoin::ScriptBuf, gov_prev: OutPoint) -> bitcoin::Transaction;
    /// Bridge withdrawal spending `bridge_prev`, paying `withdrawals`.
    fn withdrawal_tx(
        &self, withdrawals: &[(bitcoin::ScriptBuf, u64, [u8; 8])], bridge_prev: OutPoint,
    ) -> bitcoin::Transaction;
}

/// What an implementation under test must provide to run the suite.
#[async_trait]
pub trait TestHarness: Send + Sync {
    type Engine: ProtocolEngine + Send + Sync;
    type Adapter: AggregateAdapter;
    /// The PRODUCTION local-observation resolver over this adapter's store,
    /// the same type the runner uses, not a test double. Fixtures resolve
    /// engine dependencies through it so the production read path is what
    /// gets judged.
    type Reader: ObservationReader;
    type Probe: StateProbe;
    type Builder: MessageTxBuilder;

    async fn engine(&self) -> Self::Engine;
    async fn fresh_adapter(&self, role: Role) -> Self::Adapter;
    /// Drop every in-memory cache and hand back fresh instances over the same
    /// durable state.
    async fn restart(&self, adapter: Self::Adapter) -> Self::Adapter;
    async fn observation_reader(&self, adapter: &Self::Adapter) -> Self::Reader;
    async fn probe(&self, adapter: &Self::Adapter) -> Self::Probe;
    /// Real message encodings for the family fixtures.
    fn message_builder(&self) -> Self::Builder;

    /// Make the next `apply_block` fail at the given semantic stage.
    async fn arm_apply_fault(&self, point: ApplyFaultPoint);
    /// Make the next `revert_to` fail at the given semantic stage.
    async fn arm_revert_fault(&self, point: RevertFaultPoint);
    async fn clear_fault(&self);
    /// Whether the armed stage was actually reached before the injected
    /// failure fired (reachability evidence: failing at entry is not a pass).
    async fn fault_stage_reached(&self) -> bool;

    /// Forbidden historical Bitcoin RPC calls observed since the last reset,
    /// as counted by the harness's transport spy under every Bitcoin client.
    /// Current-chain header checks are allowlisted and not counted. Zero is
    /// the only passing value in `pruned_restart_has_zero_historical_fallback`.
    async fn historical_rpc_count(&self) -> u64;
    async fn reset_historical_rpc_count(&self);

    /// Simulate local data loss: raw variants for this txid disappear.
    async fn delete_raw_variants(&self, adapter: &Self::Adapter, txid: &Txid);
    /// Simulate local data corruption: stored raw bytes for this txid no
    /// longer match their identities; production reads must surface
    /// [`ObservationReadError::Corrupt`].
    async fn corrupt_raw_variants(&self, adapter: &Self::Adapter, txid: &Txid);

    /// Mark an applied effect as consumed downstream for this role (sealed
    /// L2 batch for core, emitted attestation for verifier). No-op for the
    /// standalone indexer. Must feed the same durable state the production
    /// `AggregateAdapter::downstream_consumed` reads.
    async fn mark_downstream_consumed(&self, adapter: &Self::Adapter, effect: EffectId);

    /// Run the harness's test coordinator end to end against a replacement
    /// branch (anchors from the divergence point upward, lowest first) and
    /// return its classification. It applies the production coordinator
    /// contract: `ProjectionOnly` reverts, `DownstreamConsumed` records a
    /// hard-reorg halt via the adapter, and other outcomes leave state
    /// untouched.
    async fn run_reorg_coordinator(&self, adapter: &Self::Adapter, replacement_branch: &[BlockAnchor]) -> ReorgImpact;
}

/// Registers the full acceptance suite as `#[tokio::test]` cases for one
/// harness. Call at module scope with a path to an `async fn` (or other
/// callable) returning the harness; every conformance module of the three
/// production adapters must invoke this: the suite is a judge only when
/// registered.
///
/// ```ignore
/// async fn harness() -> MyHarness { /* ... */ }
/// via_btc_ingestion_tests::ingestion_conformance_suite!(harness);
/// ```
#[macro_export]
macro_rules! ingestion_conformance_suite {
    ($harness_fn:path) => {
        mod ingestion_conformance {
            use super::*;

            $crate::__conformance_case!($harness_fn, deposit_plan_golden);
            $crate::__conformance_case!($harness_fn, deposit_block_commits_atomically);
            $crate::__conformance_case!($harness_fn, tracked_funding_and_same_block_spend);
            $crate::__conformance_case!($harness_fn, deposit_per_output_and_op_return_rules);
            $crate::__conformance_case!($harness_fn, apply_fault_matrix_and_commit_ack_loss);
            $crate::__conformance_case!($harness_fn, checkpoint_parent_version_and_adjacent_block_guard);
            $crate::__conformance_case!($harness_fn, mixed_failure_taxonomy);
            $crate::__conformance_case!($harness_fn, concurrent_apply_vs_revert_serializes);
            $crate::__conformance_case!($harness_fn, revert_fault_matrix);
            $crate::__conformance_case!($harness_fn, exact_transaction_remine);
            $crate::__conformance_case!($harness_fn, same_txid_different_witness_remine);
            $crate::__conformance_case!($harness_fn, alternate_branch_spender);
            $crate::__conformance_case!($harness_fn, revert_restores_surviving_spender);
            $crate::__conformance_case!($harness_fn, deposit_reorg_safety_frontier);
            $crate::__conformance_case!($harness_fn, pruned_restart_has_zero_historical_fallback);
            $crate::__conformance_case!($harness_fn, three_adapter_semantic_equivalence);
            $crate::__conformance_case!($harness_fn, bootstrap_exactly_once);
            $crate::__conformance_case!($harness_fn, attestation_chain_commits_batch_identity);
            $crate::__conformance_case!($harness_fn, multiple_attestations_in_one_transaction_preserve_occurrences);
            $crate::__conformance_case!($harness_fn, rotation_lifecycle_and_conflicts);
            $crate::__conformance_case!($harness_fn, upgrade_activation_is_monotonic);
            $crate::__conformance_case!($harness_fn, withdrawal_requires_bridge_input);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __conformance_case {
    ($harness_fn:path, $name:ident) => {
        #[tokio::test]
        async fn $name() {
            let harness = $harness_fn().await;
            $crate::$name(&harness).await;
        }
    };
}

pub mod envelopes {
    //! Deterministic envelope builders shared by all fixtures, so competing
    //! implementations are judged on byte-identical inputs.

    use bitcoin::{
        absolute::LockTime, hashes::Hash, opcodes::all::OP_RETURN, script::Builder, transaction::Version, Amount,
        BlockHash, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
    };
    use via_btc_ingestion::{BitcoinBlockEnvelope, BlockAnchor};

    /// Synthetic block hash, unique per (branch, height).
    pub fn branch_hash(branch: u8, height: u64) -> BlockHash {
        let mut bytes = [0u8; 32];
        bytes[0] = branch;
        bytes[1..9].copy_from_slice(&height.to_be_bytes());
        BlockHash::from_byte_array(bytes)
    }

    /// Anchor on the main fixture branch (branch 0), parent on the same branch.
    pub fn anchor(height: u64, time: u32) -> BlockAnchor {
        BlockAnchor { height, hash: branch_hash(0, height), prev_hash: branch_hash(0, height - 1), time }
    }

    /// Anchor on a fork branch whose parent is an explicit hash (use the
    /// branch-0 hash of the common ancestor for the first fork block).
    pub fn fork_anchor(branch: u8, height: u64, prev: BlockHash, time: u32) -> BlockAnchor {
        BlockAnchor { height, hash: branch_hash(branch, height), prev_hash: prev, time }
    }

    pub fn envelope(anchor: BlockAnchor, transactions: Vec<Transaction>) -> BitcoinBlockEnvelope {
        BitcoinBlockEnvelope { network: Network::Regtest, anchor, transactions }
    }

    pub fn seed_outpoint(n: u8) -> OutPoint {
        OutPoint { txid: Txid::from_byte_array([n; 32]), vout: 0 }
    }

    fn input(prev: OutPoint) -> TxIn {
        TxIn { previous_output: prev, script_sig: ScriptBuf::new(), sequence: Sequence::MAX, witness: Witness::new() }
    }

    fn tx(inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Transaction {
        Transaction { version: Version::TWO, lock_time: LockTime::ZERO, input: inputs, output: outputs }
    }

    pub fn op_return_output(receiver: [u8; 20]) -> TxOut {
        TxOut {
            value: Amount::ZERO,
            script_pubkey: Builder::new().push_opcode(OP_RETURN).push_slice(receiver).into_script(),
        }
    }

    /// Plain payment (non-protocol): one output to `script`, no OP_RETURN.
    pub fn payment_tx(prev: OutPoint, value: Amount, script: ScriptBuf) -> Transaction {
        tx(vec![input(prev)], vec![TxOut { value, script_pubkey: script }])
    }

    /// Canonical fixture deposit: output 0 pays the bridge, output 1 is the
    /// OP_RETURN receiver.
    pub fn deposit_tx(prev: OutPoint, value: Amount, bridge: ScriptBuf, receiver: [u8; 20]) -> Transaction {
        tx(vec![input(prev)], vec![TxOut { value, script_pubkey: bridge }, op_return_output(receiver)])
    }

    /// Deposit paying the bridge in several outputs (per-output rule:
    /// each is its own deposit subject).
    pub fn multi_output_deposit_tx(
        prev: OutPoint, values: &[Amount], bridge: ScriptBuf, receiver: [u8; 20],
    ) -> Transaction {
        let mut outputs: Vec<TxOut> =
            values.iter().map(|v| TxOut { value: *v, script_pubkey: bridge.clone() }).collect();
        outputs.push(op_return_output(receiver));
        tx(vec![input(prev)], outputs)
    }

    /// Deposit with two OP_RETURN receiver encodings. Agreeing receivers are
    /// valid; disagreeing receivers must become an invalid-input disposition.
    pub fn dual_op_return_deposit_tx(
        prev: OutPoint, value: Amount, bridge: ScriptBuf, receiver_a: [u8; 20], receiver_b: [u8; 20],
    ) -> Transaction {
        tx(
            vec![input(prev)],
            vec![TxOut { value, script_pubkey: bridge }, op_return_output(receiver_a), op_return_output(receiver_b)],
        )
    }

    /// Same transaction with different witness bytes: txid unchanged, wtxid
    /// changes (witness data is not committed by txid).
    pub fn with_witness(mut t: Transaction, bytes: &[u8]) -> Transaction {
        t.input[0].witness = Witness::from_slice(&[bytes]);
        t
    }
}

pub const RECEIVER_A: [u8; 20] = [0xAA; 20];
pub const RECEIVER_B: [u8; 20] = [0xBB; 20];
pub const START_HEIGHT: u64 = 100;
pub const T0: u32 = 1_700_000_000;

pub fn bridge_script() -> bitcoin::ScriptBuf {
    bitcoin::ScriptBuf::new_p2wsh(&bitcoin::WScriptHash::from_raw_hash(bitcoin::hashes::sha256::Hash::from_byte_array(
        [0x42; 32],
    )))
}

pub fn default_context() -> ProtocolContext {
    ProtocolContext {
        version: 1,
        wallets: via_btc_ingestion::WalletSet {
            sequencer: bitcoin::ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::from_byte_array([0x11; 20])),
            bridge: bridge_script(),
            governance: bitcoin::ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::from_byte_array([0x22; 20])),
            verifiers: vec![],
        },
        protocol_version: via_btc_ingestion::ProtocolVersionTag { minor: 26, patch: 0 },
    }
}

fn plan_hash(plan: &BlockPlan) -> Hash32 {
    plan.plan_hash().expect("fixture plans must be structurally valid")
}

/// Resolve the engine's dependency requests through the PRODUCTION
/// observation reader. A store answer of `None` maps to `KnownAbsent`;
/// a read error fails the resolution (the runner must halt, not guess).
pub async fn resolve_deps<R: ObservationReader>(
    reader: &R, keys: &[DependencyKey],
) -> Result<BTreeMap<DependencyKey, ResolvedDependency>, ObservationReadError> {
    let mut out = BTreeMap::new();
    for key in keys {
        let resolved = match key {
            DependencyKey::RawTx(txid) => ResolvedDependency::RawTx(match reader.canonical_observed_tx(txid).await? {
                Some(obs) => Resolution::Present(obs),
                None => Resolution::KnownAbsent,
            }),
            DependencyKey::TrackedOutput(op) => {
                ResolvedDependency::TrackedOutput(match reader.tracked_output(op).await? {
                    Some(c) => Resolution::Present(c),
                    None => Resolution::KnownAbsent,
                })
            }
        };
        out.insert(key.clone(), resolved);
    }
    Ok(out)
}

/// Resolve every requested dependency as `KnownAbsent`: the local store
/// definitively answering "not observed / not tracked". Used where a fixture
/// wants planning over a store with no relevant prior state.
pub fn all_known_absent(keys: &[DependencyKey]) -> BTreeMap<DependencyKey, ResolvedDependency> {
    keys.iter()
        .map(|k| {
            let r = match k {
                DependencyKey::RawTx(_) => ResolvedDependency::RawTx(Resolution::KnownAbsent),
                DependencyKey::TrackedOutput(_) => ResolvedDependency::TrackedOutput(Resolution::KnownAbsent),
            };
            (k.clone(), r)
        })
        .collect()
}

fn all_unavailable(keys: &[DependencyKey]) -> BTreeMap<DependencyKey, ResolvedDependency> {
    keys.iter()
        .map(|k| {
            let r = match k {
                DependencyKey::RawTx(_) => ResolvedDependency::RawTx(Resolution::Unavailable),
                DependencyKey::TrackedOutput(_) => ResolvedDependency::TrackedOutput(Resolution::Unavailable),
            };
            (k.clone(), r)
        })
        .collect()
}

/// Cap on finalize rounds. The protocol's reference depth is 2; anything
/// deeper is an engine bug, not a longer chain.
pub const MAX_FINALIZE_ROUNDS: usize = 4;

/// Drive iterative finalization to completion against the production
/// reader. Panics if the engine exceeds the round cap or re-requests a key
/// it was already given.
pub async fn finalize_with_reader<E: ProtocolEngine, R: ObservationReader>(
    engine: &E, reader: &R, draft: E::Draft, initial_keys: &[DependencyKey],
) -> Result<BlockPlan, FinalizeFailure> {
    let mut resolved =
        resolve_deps(reader, initial_keys).await.expect("fixture dependency resolution must not fail here");
    let mut draft = draft;
    for _ in 0..MAX_FINALIZE_ROUNDS {
        match engine.finalize(draft, &resolved)? {
            FinalizeOutcome::Complete(plan) => return Ok(*plan),
            FinalizeOutcome::NeedDependencies { draft: d, keys } => {
                assert!(!keys.is_empty(), "NeedDependencies with no keys");
                assert!(keys.iter().all(|k| !resolved.contains_key(k)), "engine re-requested a resolved key");
                let more = resolve_deps(reader, &keys).await.expect("fixture dependency resolution must not fail here");
                resolved.extend(more);
                draft = d;
            }
        }
    }
    panic!("finalize did not converge within {MAX_FINALIZE_ROUNDS} rounds");
}

/// Plan a block, resolving dependencies through the production reader.
/// Panics on finalize failure (fixture inputs are expected to be plannable
/// unless a fixture states otherwise).
pub async fn plan_block<H: TestHarness>(
    harness: &H, adapter: &H::Adapter, envelope: &BitcoinBlockEnvelope, context: &ProtocolContext,
) -> BlockPlan {
    let engine = harness.engine().await;
    let reader = harness.observation_reader(adapter).await;
    let (draft, keys) = engine.inspect(envelope, context);
    let plan = finalize_with_reader(&engine, &reader, draft, &keys).await.expect("fixture envelope must finalize");
    assert_eq!(plan.input_context_hash, context.context_hash(), "plan must record the context it was built from");
    plan
}

/// Apply a block end to end, panicking on any error; validates the receipt
/// against the plan (a receipt that does not partition the plan's effects is
/// an implementation bug regardless of the fixture).
pub async fn apply_ok<H: TestHarness>(harness: &H, adapter: &H::Adapter, envelope: &BitcoinBlockEnvelope) -> BlockPlan {
    let (checkpoint, context) = current_state(adapter).await;
    let plan = plan_block(harness, adapter, envelope, &context).await;
    let receipt = adapter
        .apply_block(checkpoint, &plan)
        .await
        .unwrap_or_else(|e| panic!("apply_block failed at height {}: {e}", envelope.anchor.height));
    receipt.validate(&plan).unwrap_or_else(|e| panic!("invalid projection receipt: {e}"));
    assert_eq!(receipt.plan_hash, plan_hash(&plan));
    plan
}

/// Context before any bootstrap: empty wallet scripts match nothing.
pub fn unbootstrapped_context() -> ProtocolContext {
    ProtocolContext {
        version: 1,
        wallets: via_btc_ingestion::WalletSet {
            sequencer: bitcoin::ScriptBuf::new(),
            bridge: bitcoin::ScriptBuf::new(),
            governance: bitcoin::ScriptBuf::new(),
            verifiers: vec![],
        },
        protocol_version: via_btc_ingestion::ProtocolVersionTag { minor: 0, patch: 0 },
    }
}

/// Apply one block against an explicit context (used for genesis, where the
/// adapter has no stored context yet). Panics on any error.
pub async fn apply_with_context<H: TestHarness>(
    harness: &H, adapter: &H::Adapter, envelope: &BitcoinBlockEnvelope, context: &ProtocolContext,
) -> BlockPlan {
    let (checkpoint, _) = current_state(adapter).await;
    let plan = plan_block(harness, adapter, envelope, context).await;
    let receipt = adapter
        .apply_block(checkpoint, &plan)
        .await
        .unwrap_or_else(|e| panic!("apply_block failed at height {}: {e}", envelope.anchor.height));
    receipt.validate(&plan).unwrap_or_else(|e| panic!("invalid projection receipt: {e}"));
    plan
}

/// Bootstrap a fresh chain with the builder's wallet set at START_HEIGHT.
pub async fn apply_genesis<H: TestHarness>(harness: &H, adapter: &H::Adapter) -> BlockPlan {
    let builder = harness.message_builder();
    let tx = builder.bootstrap_tx(
        &builder.wallet_set(),
        via_btc_ingestion::ProtocolVersionTag { minor: 26, patch: 0 },
        envelopes::seed_outpoint(0xB0),
    );
    let env = envelopes::envelope(envelopes::anchor(START_HEIGHT, T0), vec![tx]);
    let plan = apply_with_context(harness, adapter, &env, &unbootstrapped_context()).await;
    assert!(plan.next_context.wallets.is_bootstrapped(), "genesis bootstrap must establish the wallet set");
    plan
}

async fn current_state<A: AggregateAdapter>(adapter: &A) -> (Option<Checkpoint>, ProtocolContext) {
    match adapter.load_checkpoint_and_context().await.expect("load_checkpoint_and_context") {
        Some((cp, ctx)) => (Some(cp), ctx),
        None => (None, default_context()),
    }
}

async fn checkpoint_of<H: TestHarness>(harness: &H, adapter: &H::Adapter) -> Checkpoint {
    harness.probe(adapter).await.checkpoint().await.expect("checkpoint expected")
}

/// Planning is deterministic: the same block, protocol state, and
/// dependencies always produce byte-identical plans and hashes, and any
/// change to block position, transaction order, witness bytes, or protocol
/// state changes the hash.
pub async fn deposit_plan_golden<H: TestHarness>(harness: &H) {
    let engine = harness.engine().await;
    let context = default_context();
    let deposit =
        envelopes::deposit_tx(envelopes::seed_outpoint(1), Amount::from_sat(50_000), bridge_script(), RECEIVER_A);
    let envelope = envelopes::envelope(envelopes::anchor(START_HEIGHT, T0), vec![deposit.clone()]);

    // Dependency discovery is conservative (see the contract), so external
    // inputs may be listed even in a self-contained envelope; the store's
    // answer here is a definite "not tracked". The key LIST itself must be
    // deterministic.
    let plan = |env: &BitcoinBlockEnvelope, ctx: &ProtocolContext| {
        let (draft, keys) = engine.inspect(env, ctx);
        let (_, keys_again) = engine.inspect(env, ctx);
        assert_eq!(keys, keys_again, "dependency discovery must be deterministic");
        let mut resolved = all_known_absent(&keys);
        let mut draft = draft;
        for _ in 0..MAX_FINALIZE_ROUNDS {
            match engine.finalize(draft, &resolved).expect("finalize") {
                FinalizeOutcome::Complete(plan) => return *plan,
                FinalizeOutcome::NeedDependencies { draft: d, keys } => {
                    resolved.extend(all_known_absent(&keys));
                    draft = d;
                }
            }
        }
        panic!("finalize did not converge");
    };

    let a = plan(&envelope, &context);
    let b = plan(&envelope, &context);
    a.validate().expect("engine must emit structurally valid plans");
    assert_eq!(a.canonical_bytes().unwrap(), b.canonical_bytes().unwrap());
    assert_eq!(plan_hash(&a), plan_hash(&b));
    assert_eq!(a.events.len(), 1, "exactly one deposit event expected");
    assert_eq!(a.input_context_hash, context.context_hash());

    let mut moved = envelope.clone();
    moved.anchor = envelopes::anchor(START_HEIGHT + 1, T0);
    assert_ne!(plan_hash(&a), plan_hash(&plan(&moved, &context)));

    let mut reordered = envelope.clone();
    reordered.transactions.insert(
        0,
        envelopes::payment_tx(envelopes::seed_outpoint(9), Amount::from_sat(1_000), bitcoin::ScriptBuf::new()),
    );
    assert_ne!(plan_hash(&a), plan_hash(&plan(&reordered, &context)), "tx position must be hashed");

    let mut witnessed = envelope.clone();
    witnessed.transactions = vec![envelopes::with_witness(deposit, b"different-witness")];
    assert_ne!(plan_hash(&a), plan_hash(&plan(&witnessed, &context)), "witness bytes must be hashed (wtxid changes)");

    let mut ctx2 = context.clone();
    ctx2.version += 1;
    assert_ne!(plan_hash(&a), plan_hash(&plan(&envelope, &ctx2)));
}

/// A valid deposit block commits everything together: the raw transaction
/// bytes, its placement in the block, the tracked bridge output, the deposit
/// itself, the block's header time, the context binding, and the
/// processed-block checkpoint carrying this plan's hash.
pub async fn deposit_block_commits_atomically<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    let tx = envelopes::deposit_tx(envelopes::seed_outpoint(1), Amount::from_sat(50_000), bridge_script(), RECEIVER_A);
    let txid = tx.compute_txid();
    let wtxid = tx.compute_wtxid();
    let anchor = envelopes::anchor(START_HEIGHT, T0);
    let plan = apply_ok(harness, &adapter, &envelopes::envelope(anchor, vec![tx])).await;

    let probe = harness.probe(&adapter).await;
    let cp = probe.checkpoint().await.expect("checkpoint after apply");
    assert_eq!((cp.height, cp.hash), (anchor.height, anchor.hash));
    assert_eq!(cp.last_plan_hash, plan_hash(&plan), "checkpoint must carry the committed plan hash");
    assert_eq!(cp.context_hash, plan.next_context.context_hash(), "checkpoint must bind the post-block context");

    let variants = probe.raw_variants(&txid).await;
    assert_eq!(variants.len(), 1);
    assert_eq!((variants[0].txid(), variants[0].wtxid()), (txid, wtxid));

    let incs = probe.inclusions(&txid).await;
    assert_eq!(incs.len(), 1);
    assert!(incs[0].canonical);
    assert_eq!(
        (incs[0].inclusion.block_hash, incs[0].inclusion.tx_index, incs[0].inclusion.wtxid),
        (anchor.hash, 0, wtxid)
    );

    let subject = OutPoint { txid, vout: 0 };
    let tracked = probe.tracked_output(&subject).await.expect("tracked bridge output");
    assert_eq!(tracked.create.value, Amount::from_sat(50_000));
    assert_eq!(tracked.canonical_spender, None);

    let deposits = probe.canonical_deposits().await;
    assert_eq!(
        deposits,
        vec![DepositFact {
            subject,
            amount_sat: 50_000,
            receiver: RECEIVER_A.to_vec(),
            l2_contract: [0; 20],
            call_data: vec![],
            sender_script: None,
            block_height: anchor.height,
            block_hash: anchor.hash,
            tx_index: 0,
            header_time: T0,
            source_wtxid: wtxid,
        }]
    );
    assert!(probe.rejections().await.is_empty());
}

/// A plain payment to the bridge (no Via message attached) is still
/// retained, and a transaction later in the same block can spend it,
/// resolved purely from the block and local data, with no Bitcoin RPC.
pub async fn tracked_funding_and_same_block_spend<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    harness.reset_historical_rpc_count().await;

    let funding = envelopes::payment_tx(envelopes::seed_outpoint(1), Amount::from_sat(70_000), bridge_script());
    let funding_txid = funding.compute_txid();
    let funded = OutPoint { txid: funding_txid, vout: 0 };
    let spend = envelopes::payment_tx(funded, Amount::from_sat(69_000), bitcoin::ScriptBuf::new());
    let spend_txid = spend.compute_txid();

    apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT, T0), vec![funding, spend])).await;

    let probe = harness.probe(&adapter).await;
    assert_eq!(
        probe.raw_variants(&funding_txid).await.len(),
        1,
        "non-protocol funding tx must be retained (observation closure)"
    );
    assert_eq!(probe.raw_variants(&spend_txid).await.len(), 1, "the spending tx must be retained too");
    let tracked = probe.tracked_output(&funded).await.expect("funding output tracked");
    assert_eq!(tracked.canonical_spender, Some(spend_txid), "same-block spend must resolve in transaction order");
    assert_eq!(harness.historical_rpc_count().await, 0);
}

/// Deposit rules for ambiguous OP_RETURN transactions: every output paying
/// the bridge is its own deposit; duplicate receiver encodings that agree
/// are fine; encodings that disagree invalidate the transaction, credit
/// nothing, and leave a durable typed rejection record. (The real
/// inscription+OP_RETURN precedence fixture lands with the parser slice.)
pub async fn deposit_per_output_and_op_return_rules<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;

    let multi = envelopes::multi_output_deposit_tx(
        envelopes::seed_outpoint(1),
        &[Amount::from_sat(30_000), Amount::from_sat(20_000)],
        bridge_script(),
        RECEIVER_A,
    );
    let multi_txid = multi.compute_txid();
    let agree = envelopes::dual_op_return_deposit_tx(
        envelopes::seed_outpoint(2),
        Amount::from_sat(10_000),
        bridge_script(),
        RECEIVER_A,
        RECEIVER_A,
    );
    let agree_txid = agree.compute_txid();
    let disagree = envelopes::dual_op_return_deposit_tx(
        envelopes::seed_outpoint(3),
        Amount::from_sat(40_000),
        bridge_script(),
        RECEIVER_A,
        RECEIVER_B,
    );
    let disagree_txid = disagree.compute_txid();

    let anchor = envelopes::anchor(START_HEIGHT, T0);
    let plan = apply_ok(harness, &adapter, &envelopes::envelope(anchor, vec![multi, agree, disagree])).await;

    assert_eq!(plan.events.len(), 3, "two multi-output subjects + one agreeing");
    let rejection = plan
        .dispositions
        .iter()
        .find(|d| d.ordinal.tx_index == 2)
        .expect("disagreeing encodings must record a disposition");
    assert!(
        matches!(
            rejection.kind,
            DispositionKind::RejectedInvalid { code: RejectionCode::ConflictingReceiverEncodings, .. }
        ),
        "expected typed ConflictingReceiverEncodings, got {rejection:?}"
    );

    let probe = harness.probe(&adapter).await;
    let mut deposits = probe.canonical_deposits().await;
    deposits.sort();
    let mut subjects: Vec<OutPoint> = deposits.iter().map(|d| d.subject).collect();
    subjects.sort();
    let mut expected = vec![
        OutPoint { txid: multi_txid, vout: 0 },
        OutPoint { txid: multi_txid, vout: 1 },
        OutPoint { txid: agree_txid, vout: 0 },
    ];
    expected.sort();
    assert_eq!(subjects, expected);
    assert!(deposits.iter().all(|d| d.subject.txid != disagree_txid));
    let amounts: BTreeMap<OutPoint, u64> = deposits.iter().map(|d| (d.subject, d.amount_sat)).collect();
    assert_eq!(amounts[&OutPoint { txid: multi_txid, vout: 0 }], 30_000);
    assert_eq!(amounts[&OutPoint { txid: multi_txid, vout: 1 }], 20_000);

    let rejections = probe.rejections().await;
    assert_eq!(
        rejections,
        vec![RejectionRecord {
            block_hash: anchor.hash,
            ordinal: rejection.ordinal,
            code: RejectionCode::ConflictingReceiverEncodings,
        }],
        "the rejection must be durably stored, not just present in the plan"
    );
}

/// Crash safety of `apply_block`: a failure at any write stage leaves no
/// trace of the block after restart (checked across every state class), and
/// the one tricky case (the database committed but the caller never heard
/// back) is recoverable through the checkpoint's `last_plan_hash` without
/// duplicating anything.
pub async fn apply_fault_matrix_and_commit_ack_loss<H: TestHarness>(harness: &H) {
    for point in ALL_APPLY_FAULT_POINTS {
        let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
        let tx =
            envelopes::deposit_tx(envelopes::seed_outpoint(1), Amount::from_sat(50_000), bridge_script(), RECEIVER_A);
        let txid = tx.compute_txid();
        let subject = OutPoint { txid, vout: 0 };
        let envelope = envelopes::envelope(envelopes::anchor(START_HEIGHT, T0), vec![tx]);
        let plan = plan_block(harness, &adapter, &envelope, &default_context()).await;

        harness.arm_apply_fault(point).await;
        let result = adapter.apply_block(None, &plan).await;
        assert!(result.is_err(), "armed fault {point:?} must surface as an error");
        assert!(
            harness.fault_stage_reached().await,
            "{point:?}: the armed stage must actually be reached; failing at entry is not a pass"
        );
        harness.clear_fault().await;

        let adapter = harness.restart(adapter).await;
        let probe = harness.probe(&adapter).await;

        if point == ApplyFaultPoint::AfterCommitBeforeAck {
            assert!(
                matches!(result, Err(ApplyError::CommitIndeterminate { .. })),
                "{point:?}: a lost acknowledgement is indeterminate, not a rollback promise; got {result:?}"
            );
            let cp = probe.checkpoint().await.expect("commit happened, restart must see it");
            assert_eq!(
                cp.last_plan_hash,
                plan_hash(&plan),
                "recovery rule: last_plan_hash identifies the committed plan"
            );
            assert_eq!(probe.canonical_deposits().await.len(), 1);
            // Replay against the stale expectation is rejected without
            // duplicating; the caller resolves via last_plan_hash.
            let replay = adapter.apply_block(None, &plan).await;
            match replay {
                Err(ApplyError::StaleCheckpoint { actual, .. }) => {
                    let actual = actual.expect("checkpoint exists");
                    assert_eq!(
                        actual.last_plan_hash,
                        plan_hash(&plan),
                        "the caller must be able to prove this exact plan committed"
                    );
                }
                other => panic!("replay must fail the checkpoint CAS, got {other:?}"),
            }
            assert_eq!(harness.probe(&adapter).await.canonical_deposits().await.len(), 1);
        } else {
            assert!(probe.checkpoint().await.is_none(), "{point:?}: no checkpoint may survive");
            assert!(probe.canonical_deposits().await.is_empty(), "{point:?}");
            assert!(
                probe.raw_variants(&txid).await.is_empty(),
                "{point:?}: no raw observation may survive a failed apply"
            );
            assert!(
                probe.inclusions(&txid).await.is_empty(),
                "{point:?}: no inclusion record may survive a failed apply"
            );
            assert!(probe.tracked_output(&subject).await.is_none(), "{point:?}: no tracked output");
            assert!(probe.rejections().await.is_empty(), "{point:?}");
            // Recovery: the same apply now succeeds completely.
            let receipt = adapter.apply_block(None, &plan).await.expect("apply after recovery");
            receipt.validate(&plan).expect("valid receipt");
            assert_eq!(harness.probe(&adapter).await.canonical_deposits().await.len(), 1);
        }
    }
}

/// `apply_block` refuses to write anything when its view is outdated or the
/// plan is unsound: wrong expected checkpoint, a height gap, a parent-hash
/// mismatch, a version mismatch, a context mismatch, or a structurally
/// invalid plan, each with its typed error. A previously committed block
/// stays untouched through all of these rejections.
pub async fn checkpoint_parent_version_and_adjacent_block_guard<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    let block_n = envelopes::envelope(
        envelopes::anchor(START_HEIGHT, T0),
        vec![envelopes::deposit_tx(envelopes::seed_outpoint(1), Amount::from_sat(50_000), bridge_script(), RECEIVER_A)],
    );
    let plan_n = plan_block(harness, &adapter, &block_n, &default_context()).await;

    // Stale checkpoint expectation on an empty store: zero writes.
    let fake = Checkpoint {
        height: 1,
        hash: envelopes::branch_hash(7, 1),
        kernel_version: plan_n.kernel_version,
        observation_rule_version: plan_n.observation_rule_version,
        context_hash: default_context().context_hash(),
        last_plan_hash: [0; 32],
    };
    assert!(matches!(adapter.apply_block(Some(fake), &plan_n).await, Err(ApplyError::StaleCheckpoint { .. })));
    let probe = harness.probe(&adapter).await;
    assert!(probe.checkpoint().await.is_none());
    assert!(probe.canonical_deposits().await.is_empty());

    // Commit N.
    adapter.apply_block(None, &plan_n).await.expect("apply N");
    let cp_n = checkpoint_of(harness, &adapter).await;

    // Height gap: rejected, N intact.
    let gap = envelopes::envelope(envelopes::anchor(START_HEIGHT + 2, T0 + 1200), vec![]);
    let plan_gap = plan_block(harness, &adapter, &gap, &default_context()).await;
    assert!(matches!(adapter.apply_block(Some(cp_n), &plan_gap).await, Err(ApplyError::NotAdjacent { .. })));

    // Wrong parent hash at the right height: rejected.
    let wrong_parent = envelopes::envelope(
        envelopes::fork_anchor(3, START_HEIGHT + 1, envelopes::branch_hash(3, START_HEIGHT), T0 + 600),
        vec![],
    );
    let plan_wrong = plan_block(harness, &adapter, &wrong_parent, &default_context()).await;
    assert!(matches!(adapter.apply_block(Some(cp_n), &plan_wrong).await, Err(ApplyError::NotAdjacent { .. })));

    let next = envelopes::envelope(envelopes::anchor(START_HEIGHT + 1, T0 + 600), vec![]);

    // Kernel version mismatch: typed rejection.
    let mut plan_versioned = plan_block(harness, &adapter, &next, &default_context()).await;
    plan_versioned.kernel_version = via_btc_ingestion::KernelVersion(plan_versioned.kernel_version.0 + 1);
    assert!(matches!(adapter.apply_block(Some(cp_n), &plan_versioned).await, Err(ApplyError::VersionMismatch(_))));

    // Context mismatch: a plan recording a different input context than the
    // checkpoint's cannot commit.
    let mut plan_stale_ctx = plan_block(harness, &adapter, &next, &default_context()).await;
    plan_stale_ctx.input_context_hash = [9; 32];
    assert!(matches!(adapter.apply_block(Some(cp_n), &plan_stale_ctx).await, Err(ApplyError::ContextMismatch { .. })));

    // Structurally invalid plan: typed rejection before any write.
    let mut plan_invalid = plan_block(harness, &adapter, &next, &default_context()).await;
    plan_invalid.dispositions = vec![
        via_btc_ingestion::Disposition {
            ordinal: via_btc_ingestion::EventOrdinal { tx_index: 1, location: MessageLocation::Output(0) },
            kind: DispositionKind::Duplicate,
        },
        via_btc_ingestion::Disposition {
            ordinal: via_btc_ingestion::EventOrdinal { tx_index: 0, location: MessageLocation::Output(0) },
            kind: DispositionKind::Duplicate,
        },
    ];
    assert!(matches!(adapter.apply_block(Some(cp_n), &plan_invalid).await, Err(ApplyError::InvalidPlan(_))));

    let probe = harness.probe(&adapter).await;
    assert_eq!(probe.checkpoint().await.unwrap(), cp_n, "N must remain committed exactly");
    assert_eq!(probe.canonical_deposits().await.len(), 1);
}

/// Different failure kinds compose correctly: valid and invalid transactions
/// commit together (projection plus durable rejection record), a typed
/// infrastructure error commits nothing of the failing block, dependency
/// unavailability halts as MissingDependency before any write (even when
/// the block also contains a valid deposit), and corrupt local observations
/// surface as corrupt reads, never as invalid input and never as an RPC
/// fallback. (Unsupported-valid-event composition has no constructible
/// representative in this wire format yet; it lands with the first versioned
/// event family.)
pub async fn mixed_failure_taxonomy<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    harness.reset_historical_rpc_count().await;

    // (a) Valid + invalid in one block: both visible, checkpoint advances.
    let valid =
        envelopes::deposit_tx(envelopes::seed_outpoint(1), Amount::from_sat(50_000), bridge_script(), RECEIVER_A);
    let valid_txid = valid.compute_txid();
    let invalid = envelopes::dual_op_return_deposit_tx(
        envelopes::seed_outpoint(2),
        Amount::from_sat(10_000),
        bridge_script(),
        RECEIVER_A,
        RECEIVER_B,
    );
    let anchor_n = envelopes::anchor(START_HEIGHT, T0);
    let plan = apply_ok(harness, &adapter, &envelopes::envelope(anchor_n, vec![valid, invalid])).await;
    assert_eq!(plan.events.len(), 1);
    assert_eq!(plan.dispositions.len(), 1);
    let probe = harness.probe(&adapter).await;
    assert_eq!(probe.canonical_deposits().await.len(), 1);
    assert_eq!(probe.rejections().await.len(), 1, "rejection must be durable");
    let cp = probe.checkpoint().await.unwrap();
    assert_eq!(cp.height, START_HEIGHT);

    // (b) Typed infrastructure failure mid-apply: nothing of N+1 visible,
    // checkpoint unchanged; unavailability is never invalidity.
    let next = envelopes::envelope(
        envelopes::anchor(START_HEIGHT + 1, T0 + 600),
        vec![envelopes::deposit_tx(envelopes::seed_outpoint(3), Amount::from_sat(20_000), bridge_script(), RECEIVER_B)],
    );
    let plan_next = plan_block(harness, &adapter, &next, &default_context()).await;
    harness.arm_apply_fault(ApplyFaultPoint::AfterDomainProjections).await;
    let res = adapter.apply_block(Some(cp), &plan_next).await;
    harness.clear_fault().await;
    assert!(
        matches!(res, Err(ApplyError::Infrastructure(_))),
        "an injected infrastructure fault must surface as the typed Infrastructure error, got {res:?}"
    );
    let probe = harness.probe(&adapter).await;
    assert_eq!(probe.checkpoint().await.unwrap(), cp);
    assert_eq!(probe.canonical_deposits().await.len(), 1);
    assert_eq!(probe.rejections().await.len(), 1);

    // (c) A block containing BOTH a valid deposit and a spend whose
    // dependency the store cannot answer: finalization halts as
    // MissingDependency before any adapter write; the valid prefix does not
    // commit and the cursor does not move.
    let engine = harness.engine().await;
    let mixed = envelopes::envelope(
        envelopes::anchor(START_HEIGHT + 1, T0 + 600),
        vec![
            envelopes::deposit_tx(envelopes::seed_outpoint(4), Amount::from_sat(15_000), bridge_script(), RECEIVER_A),
            envelopes::payment_tx(
                OutPoint { txid: valid_txid, vout: 0 },
                Amount::from_sat(1_000),
                bitcoin::ScriptBuf::new(),
            ),
        ],
    );
    let (draft, keys) = engine.inspect(&mixed, &default_context());
    assert!(
        keys.contains(&DependencyKey::TrackedOutput(OutPoint { txid: valid_txid, vout: 0 })),
        "the spent tracked outpoint must be among the listed dependencies (observation closure), got {keys:?}"
    );
    match engine.finalize(draft, &all_unavailable(&keys)) {
        Err(FinalizeFailure::MissingDependency(_)) => {}
        Err(other) => panic!("unavailable dependency must halt as MissingDependency, got {other:?}"),
        Ok(_) => panic!("unavailable dependency must not finalize"),
    }
    let probe = harness.probe(&adapter).await;
    assert_eq!(probe.checkpoint().await.unwrap(), cp, "no cursor movement past uninterpreted data");
    assert_eq!(probe.canonical_deposits().await.len(), 1, "the valid prefix must not commit");

    // (d) Corrupt stored raw bytes: the production reader must surface a
    // typed Corrupt error, never invalid input, never an RPC fallback.
    harness.corrupt_raw_variants(&adapter, &valid_txid).await;
    let reader = harness.observation_reader(&adapter).await;
    match reader.canonical_observed_tx(&valid_txid).await {
        Err(ObservationReadError::Corrupt(_)) => {}
        other => panic!("corrupt local bytes must surface as Corrupt, got {other:?}"),
    }
    assert_eq!(harness.historical_rpc_count().await, 0, "corruption must never trigger an RPC fallback");
}

/// When an apply and a revert race from the same checkpoint, exactly one
/// wins; the loser fails with the typed stale-checkpoint error and, after
/// reloading the winner's state, a correctly-based follow-up succeeds.
pub async fn concurrent_apply_vs_revert_serializes<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    let anchor_n = envelopes::anchor(START_HEIGHT, T0);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_n, vec![])).await;
    let anchor_n1 = envelopes::anchor(START_HEIGHT + 1, T0 + 600);
    apply_ok(
        harness,
        &adapter,
        &envelopes::envelope(
            anchor_n1,
            vec![envelopes::deposit_tx(
                envelopes::seed_outpoint(1),
                Amount::from_sat(50_000),
                bridge_script(),
                RECEIVER_A,
            )],
        ),
    )
    .await;
    let cp = checkpoint_of(harness, &adapter).await;

    let next = envelopes::envelope(envelopes::anchor(START_HEIGHT + 2, T0 + 1200), vec![]);
    let plan_next = plan_block(harness, &adapter, &next, &default_context()).await;

    let apply_fut = adapter.apply_block(Some(cp), &plan_next);
    let revert_fut = adapter.revert_to(cp, anchor_n);
    let (apply_res, revert_res) = futures::join!(apply_fut, revert_fut);

    let winners = usize::from(apply_res.is_ok()) + usize::from(revert_res.is_ok());
    assert_eq!(
        winners, 1,
        "exactly one racing operation may win from one checkpoint: apply={apply_res:?} revert={revert_res:?}"
    );
    if apply_res.is_err() {
        assert!(
            matches!(apply_res, Err(ApplyError::StaleCheckpoint { .. })),
            "loser must see the typed stale-CAS outcome: {apply_res:?}"
        );
    }
    if revert_res.is_err() {
        assert!(
            matches!(revert_res, Err(via_btc_ingestion::RevertError::StaleCheckpoint { .. })),
            "loser must see the typed stale-CAS outcome: {revert_res:?}"
        );
    }

    // Loser reload/retry: a follow-up based on the winner's checkpoint works.
    let cp_after = checkpoint_of(harness, &adapter).await;
    if apply_res.is_ok() {
        assert_eq!(cp_after.height, START_HEIGHT + 2);
        adapter.revert_to(cp_after, anchor_n1).await.expect("revert from the winning checkpoint");
    } else {
        assert_eq!((cp_after.height, cp_after.hash), (anchor_n.height, anchor_n.hash));
        let follow = envelopes::envelope(envelopes::fork_anchor(1, START_HEIGHT + 1, anchor_n.hash, T0 + 700), vec![]);
        let plan_follow = plan_block(harness, &adapter, &follow, &default_context()).await;
        adapter.apply_block(Some(cp_after), &plan_follow).await.expect("apply from the winning checkpoint");
    }
}

/// Crash safety of `revert_to`: after a failure at any stage and a restart,
/// the database is either fully at the old block or fully at the ancestor,
/// never in between; raw transaction bytes survive the revert; every
/// completed revert bumps the canonical revision; and reverting to where we
/// already are is a harmless no-op that does not bump it.
pub async fn revert_fault_matrix<H: TestHarness>(harness: &H) {
    for point in ALL_REVERT_FAULT_POINTS {
        let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
        let anchor_a = envelopes::anchor(START_HEIGHT, T0);
        let funding = envelopes::payment_tx(envelopes::seed_outpoint(1), Amount::from_sat(70_000), bridge_script());
        let funded = OutPoint { txid: funding.compute_txid(), vout: 0 };
        apply_ok(harness, &adapter, &envelopes::envelope(anchor_a, vec![funding])).await;

        let deposit =
            envelopes::deposit_tx(envelopes::seed_outpoint(2), Amount::from_sat(50_000), bridge_script(), RECEIVER_A);
        let deposit_txid = deposit.compute_txid();
        let spend = envelopes::payment_tx(funded, Amount::from_sat(69_000), bitcoin::ScriptBuf::new());
        apply_ok(
            harness,
            &adapter,
            &envelopes::envelope(envelopes::anchor(START_HEIGHT + 1, T0 + 600), vec![deposit, spend]),
        )
        .await;
        let cp_b = checkpoint_of(harness, &adapter).await;

        let rev_at_b = harness.probe(&adapter).await.canonical_revision().await;
        harness.arm_revert_fault(point).await;
        let res = adapter.revert_to(cp_b, anchor_a).await;
        assert!(res.is_err(), "armed fault {point:?} must surface");
        if point == RevertFaultPoint::AfterCommitBeforeAck {
            assert!(
                matches!(res, Err(via_btc_ingestion::RevertError::CommitIndeterminate)),
                "{point:?}: a lost acknowledgement is indeterminate, not a plain failure; got {res:?}"
            );
        }
        assert!(harness.fault_stage_reached().await, "{point:?}: the armed stage must actually be reached");
        harness.clear_fault().await;

        // Logical atomic visibility after restart: fully at B or fully at A.
        let adapter = harness.restart(adapter).await;
        let probe = harness.probe(&adapter).await;
        let cp = probe.checkpoint().await.unwrap();
        let deposits = probe.canonical_deposits().await;
        let spender = probe.tracked_output(&funded).await.unwrap().canonical_spender;
        if cp.height == START_HEIGHT + 1 {
            assert_eq!(deposits.len(), 1, "{point:?}: still fully at B");
            assert!(spender.is_some(), "{point:?}: still fully at B");
        } else {
            assert_eq!((cp.height, cp.hash), (anchor_a.height, anchor_a.hash), "{point:?}");
            assert!(deposits.is_empty(), "{point:?}: fully reverted");
            assert!(spender.is_none(), "{point:?}: spend reverted");
            assert!(!probe.raw_variants(&deposit_txid).await.is_empty(), "{point:?}: raw bytes must survive revert");
            assert!(
                probe.canonical_revision().await > rev_at_b,
                "{point:?}: a revert that committed must bump the revision even if the caller saw an error"
            );
        }

        // Complete the revert if it had not landed; the completed revert
        // must bump the canonical revision.
        let cp = probe.checkpoint().await.unwrap();
        if cp.height == START_HEIGHT + 1 {
            let before = probe.canonical_revision().await;
            let receipt = adapter.revert_to(cp, anchor_a).await.expect("revert after recovery");
            assert!(receipt.canonical_revision > before, "{point:?}: revert must bump revision");
            assert_eq!(harness.probe(&adapter).await.canonical_revision().await, receipt.canonical_revision);
        }
        // Idempotent no-op: reverting to the current anchor changes nothing.
        let cp = checkpoint_of(harness, &adapter).await;
        let rev_before = harness.probe(&adapter).await.canonical_revision().await;
        adapter.revert_to(cp, anchor_a).await.expect("revert to current anchor is a no-op");
        let probe = harness.probe(&adapter).await;
        assert_eq!(probe.checkpoint().await.unwrap(), cp);
        assert_eq!(probe.canonical_revision().await, rev_before, "{point:?}: no-op must not bump revision");
    }
}

/// A transaction orphaned by a reorg and re-mined unchanged in a different
/// block and position: bytes stored once, both placements recorded, and
/// exactly one canonical deposit anchored (with provenance) at the new
/// location.
pub async fn exact_transaction_remine<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    let anchor_a = envelopes::anchor(START_HEIGHT, T0);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_a, vec![])).await;

    let deposit =
        envelopes::deposit_tx(envelopes::seed_outpoint(1), Amount::from_sat(50_000), bridge_script(), RECEIVER_A);
    let txid = deposit.compute_txid();
    let wtxid = deposit.compute_wtxid();
    let anchor_b = envelopes::anchor(START_HEIGHT + 1, T0 + 600);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_b, vec![deposit.clone()])).await;
    let cp_b = checkpoint_of(harness, &adapter).await;

    adapter.revert_to(cp_b, anchor_a).await.expect("revert B");
    let filler = envelopes::payment_tx(envelopes::seed_outpoint(9), Amount::from_sat(1_000), bitcoin::ScriptBuf::new());
    let anchor_b2 = envelopes::fork_anchor(1, START_HEIGHT + 1, anchor_a.hash, T0 + 700);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_b2, vec![filler, deposit])).await;

    let probe = harness.probe(&adapter).await;
    assert_eq!(probe.raw_variants(&txid).await.len(), 1, "one raw variant: bytes are identical");
    let incs = probe.inclusions(&txid).await;
    assert_eq!(incs.len(), 2, "two inclusion occurrences (orphaned + canonical)");
    let canonical: Vec<_> = incs.iter().filter(|i| i.canonical).collect();
    assert_eq!(canonical.len(), 1);
    assert_eq!(canonical[0].inclusion.block_hash, anchor_b2.hash);
    assert_eq!(canonical[0].inclusion.tx_index, 1, "location-derived fields recalculated");

    let deposits = probe.canonical_deposits().await;
    assert_eq!(deposits.len(), 1, "exactly one canonical deposit effect");
    assert_eq!(
        (deposits[0].block_hash, deposits[0].tx_index, deposits[0].header_time, deposits[0].source_wtxid),
        (anchor_b2.hash, 1, T0 + 700, wtxid)
    );
}

/// Two transactions can share a txid while their witness data differs (txid
/// does not cover witnesses, and Via inscriptions live there). Both variants
/// must be stored separately, the canonical placement must reference the
/// variant on the surviving chain, and the canonical deposit's provenance
/// must name that variant's wtxid; the orphan's parse is never reused.
pub async fn same_txid_different_witness_remine<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    let anchor_a = envelopes::anchor(START_HEIGHT, T0);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_a, vec![])).await;

    let base =
        envelopes::deposit_tx(envelopes::seed_outpoint(1), Amount::from_sat(50_000), bridge_script(), RECEIVER_A);
    let variant_a = envelopes::with_witness(base.clone(), b"variant-a");
    let variant_b = envelopes::with_witness(base, b"variant-b");
    let txid = variant_a.compute_txid();
    assert_eq!(txid, variant_b.compute_txid(), "txid must not commit to witness");
    assert_ne!(variant_a.compute_wtxid(), variant_b.compute_wtxid());

    let anchor_b = envelopes::anchor(START_HEIGHT + 1, T0 + 600);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_b, vec![variant_a])).await;
    let cp_b = checkpoint_of(harness, &adapter).await;
    adapter.revert_to(cp_b, anchor_a).await.expect("revert");

    let anchor_b2 = envelopes::fork_anchor(1, START_HEIGHT + 1, anchor_a.hash, T0 + 700);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_b2, vec![variant_b.clone()])).await;

    let probe = harness.probe(&adapter).await;
    assert_eq!(probe.raw_variants(&txid).await.len(), 2, "distinct wtxids are distinct raw variants");
    let canonical: Vec<_> = probe.inclusions(&txid).await.into_iter().filter(|i| i.canonical).collect();
    assert_eq!(canonical.len(), 1);
    assert_eq!(canonical[0].inclusion.wtxid, variant_b.compute_wtxid());

    let deposits = probe.canonical_deposits().await;
    assert_eq!(deposits.len(), 1);
    assert_eq!(
        deposits[0].source_wtxid,
        variant_b.compute_wtxid(),
        "canonical projection must be derived from the canonical variant, never the orphan's parse"
    );

    // The production reader must select the canonical variant atomically.
    let reader = harness.observation_reader(&adapter).await;
    let observed = reader.canonical_observed_tx(&txid).await.expect("read").expect("canonically observed");
    assert_eq!(observed.inclusion().block_hash, anchor_b2.hash);
    assert_eq!(observed.variant().wtxid(), variant_b.compute_wtxid());
}

/// When competing branches spend the same tracked output with different
/// transactions, only the spender on the surviving branch counts, while
/// the orphaned spend remains visible as history.
pub async fn alternate_branch_spender<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    let anchor_a = envelopes::anchor(START_HEIGHT, T0);
    let funding = envelopes::payment_tx(envelopes::seed_outpoint(1), Amount::from_sat(70_000), bridge_script());
    let funded = OutPoint { txid: funding.compute_txid(), vout: 0 };
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_a, vec![funding])).await;

    let spender_a = envelopes::payment_tx(funded, Amount::from_sat(69_000), bitcoin::ScriptBuf::new());
    let spender_a_txid = spender_a.compute_txid();
    let anchor_b = envelopes::anchor(START_HEIGHT + 1, T0 + 600);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_b, vec![spender_a])).await;
    let cp_b = checkpoint_of(harness, &adapter).await;

    adapter.revert_to(cp_b, anchor_a).await.expect("revert");
    let spender_b = envelopes::payment_tx(funded, Amount::from_sat(68_000), bitcoin::ScriptBuf::new());
    let spender_b_txid = spender_b.compute_txid();
    let anchor_b2 = envelopes::fork_anchor(1, START_HEIGHT + 1, anchor_a.hash, T0 + 700);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_b2, vec![spender_b])).await;

    let probe = harness.probe(&adapter).await;
    let tracked = probe.tracked_output(&funded).await.expect("tracked output");
    assert_eq!(tracked.canonical_spender, Some(spender_b_txid), "the surviving branch's spender");
    assert!(
        tracked.observed_spenders.contains(&spender_a_txid) && tracked.observed_spenders.contains(&spender_b_txid),
        "both spend observations remain as history: {:?}",
        tracked.observed_spenders
    );
}

/// Reverting a later spend restores the spender that remains canonical at
/// the ancestor. Reverting past that older spend then clears it.
pub async fn revert_restores_surviving_spender<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    let anchor_a = envelopes::anchor(START_HEIGHT, T0);
    let funding = envelopes::payment_tx(envelopes::seed_outpoint(1), Amount::from_sat(70_000), bridge_script());
    let funded = OutPoint { txid: funding.compute_txid(), vout: 0 };
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_a, vec![funding])).await;

    let spender_a = envelopes::payment_tx(funded, Amount::from_sat(69_000), bitcoin::ScriptBuf::new());
    let spender_a_txid = spender_a.compute_txid();
    let anchor_b1 = envelopes::anchor(START_HEIGHT + 1, T0 + 600);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_b1, vec![spender_a])).await;

    let anchor_b2 = envelopes::anchor(START_HEIGHT + 2, T0 + 1200);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_b2, vec![])).await;

    let spender_b = envelopes::payment_tx(funded, Amount::from_sat(68_000), bitcoin::ScriptBuf::new());
    let spender_b_txid = spender_b.compute_txid();
    let anchor_b3 = envelopes::fork_anchor(1, START_HEIGHT + 3, anchor_b2.hash, T0 + 1800);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor_b3, vec![spender_b])).await;
    assert_eq!(
        harness.probe(&adapter).await.tracked_output(&funded).await.unwrap().canonical_spender,
        Some(spender_b_txid)
    );

    let cp_b3 = checkpoint_of(harness, &adapter).await;
    adapter.revert_to(cp_b3, anchor_b1).await.expect("revert later branch");
    assert_eq!(
        harness.probe(&adapter).await.tracked_output(&funded).await.unwrap().canonical_spender,
        Some(spender_a_txid),
        "the spender in the surviving ancestor block must be restored"
    );

    let cp_b1 = checkpoint_of(harness, &adapter).await;
    adapter.revert_to(cp_b1, anchor_a).await.expect("revert older spend");
    assert_eq!(
        harness.probe(&adapter).await.tracked_output(&funded).await.unwrap().canonical_spender,
        None,
        "reverting past the older spend must leave the output unspent"
    );
}

/// Where automatic reorg recovery must stop: purely local effects revert
/// automatically; effects already consumed downstream (a sealed L2 batch, an
/// emitted attestation) halt the node with a durable, restart-surviving
/// hard-reorg record (read through the production adapter) instead of
/// rewinding; the standalone indexer always reverts and its revision signal
/// increments; an unknown fork ancestor halts rather than guessing.
pub async fn deposit_reorg_safety_frontier<H: TestHarness>(harness: &H) {
    async fn setup<H: TestHarness>(harness: &H, role: Role) -> (H::Adapter, BlockAnchor, EffectId) {
        let adapter = harness.fresh_adapter(role).await;
        let anchor_a = envelopes::anchor(START_HEIGHT, T0);
        apply_ok(harness, &adapter, &envelopes::envelope(anchor_a, vec![])).await;
        let plan = apply_ok(
            harness,
            &adapter,
            &envelopes::envelope(
                envelopes::anchor(START_HEIGHT + 1, T0 + 600),
                vec![envelopes::deposit_tx(
                    envelopes::seed_outpoint(1),
                    Amount::from_sat(50_000),
                    bridge_script(),
                    RECEIVER_A,
                )],
            ),
        )
        .await;
        let effect = plan.effect_ids()[0];
        (adapter, anchor_a, effect)
    }
    let replacement =
        |anchor_a: BlockAnchor| vec![envelopes::fork_anchor(1, START_HEIGHT + 1, anchor_a.hash, T0 + 700)];

    // (a) Reorg entirely above the checkpoint: no-op.
    let (adapter, _a, _) = setup(harness, Role::CoreSequencer).await;
    let above =
        vec![envelopes::fork_anchor(2, START_HEIGHT + 2, envelopes::branch_hash(0, START_HEIGHT + 1), T0 + 1300)];
    assert_eq!(harness.run_reorg_coordinator(&adapter, &above).await, ReorgImpact::NoProcessedImpact);

    // (b) Projection-only impact: automatic revert, for every role, with the
    // exact ancestor reported and the revision signal fired.
    for role in [Role::CoreSequencer, Role::Verifier, Role::StandaloneIndexer] {
        let (adapter, anchor_a, _) = setup(harness, role).await;
        let rev_before = harness.probe(&adapter).await.canonical_revision().await;
        let impact = harness.run_reorg_coordinator(&adapter, &replacement(anchor_a)).await;
        assert!(
            matches!(impact, ReorgImpact::ProjectionOnly { ancestor } if ancestor.hash == anchor_a.hash),
            "{role:?}: projection-only must auto-revert, got {impact:?}"
        );
        let probe = harness.probe(&adapter).await;
        assert_eq!(probe.checkpoint().await.unwrap().hash, anchor_a.hash, "{role:?}: reverted");
        assert!(probe.canonical_deposits().await.is_empty(), "{role:?}");
        assert!(probe.canonical_revision().await > rev_before, "{role:?}: readers must see the revision signal");
        assert!(adapter.hard_reorg_halt().await.unwrap().is_none(), "{role:?}: no halt for a projection-only reorg");
    }

    // (c) Downstream-consumed impact: core and verifier halt durably without
    // partial rewind; the standalone indexer has no downstream boundary.
    for role in [Role::CoreSequencer, Role::Verifier] {
        let (adapter, anchor_a, effect) = setup(harness, role).await;
        harness.mark_downstream_consumed(&adapter, effect).await;
        let impact = harness.run_reorg_coordinator(&adapter, &replacement(anchor_a)).await;
        match &impact {
            ReorgImpact::DownstreamConsumed { ancestor, affected } => {
                assert_eq!(ancestor.hash, anchor_a.hash, "{role:?}: exact ancestor");
                assert_eq!(affected, &vec![effect], "{role:?}: exact affected effect set");
            }
            other => panic!("{role:?}: consumed deposit must classify DownstreamConsumed, got {other:?}"),
        }
        let probe = harness.probe(&adapter).await;
        assert_eq!(probe.checkpoint().await.unwrap().height, START_HEIGHT + 1, "{role:?}: no partial rewind");
        assert_eq!(probe.canonical_deposits().await.len(), 1, "{role:?}");
        let halt = adapter
            .hard_reorg_halt()
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{role:?}: durable hard-reorg record required"));
        assert_eq!(halt.affected, vec![effect], "{role:?}");
        assert_eq!(
            halt.divergence.hash,
            replacement(anchor_a)[0].hash,
            "{role:?}: the halt must record the replacement-branch divergence anchor"
        );

        // The record survives restart and blocks ingestion with the typed
        // Halted error.
        let adapter = harness.restart(adapter).await;
        assert!(adapter.hard_reorg_halt().await.unwrap().is_some(), "{role:?}: survives restart");
        let cp = checkpoint_of(harness, &adapter).await;
        let next = envelopes::envelope(envelopes::anchor(START_HEIGHT + 2, T0 + 1200), vec![]);
        let plan_next = plan_block(harness, &adapter, &next, &default_context()).await;
        assert!(
            matches!(adapter.apply_block(Some(cp), &plan_next).await, Err(ApplyError::Halted(_))),
            "{role:?}: ingestion must stay halted with the typed error"
        );
    }
    {
        let (adapter, anchor_a, effect) = setup(harness, Role::StandaloneIndexer).await;
        harness.mark_downstream_consumed(&adapter, effect).await;
        let rev_before = harness.probe(&adapter).await.canonical_revision().await;
        let impact = harness.run_reorg_coordinator(&adapter, &replacement(anchor_a)).await;
        assert!(
            matches!(impact, ReorgImpact::ProjectionOnly { .. }),
            "standalone has no downstream boundary: reverts with a revision signal, got {impact:?}"
        );
        let probe = harness.probe(&adapter).await;
        assert_eq!(probe.checkpoint().await.unwrap().hash, anchor_a.hash);
        assert!(probe.canonical_revision().await > rev_before, "revision signal must fire");
    }

    // (d) Divergence at (or below) the oldest locally known block: halt the
    // decision, never assume the window edge is the ancestor, never rewind.
    let (adapter, _a, _) = setup(harness, Role::CoreSequencer).await;
    let below = vec![envelopes::fork_anchor(4, START_HEIGHT, envelopes::branch_hash(4, START_HEIGHT - 1), T0 + 50)];
    assert_eq!(harness.run_reorg_coordinator(&adapter, &below).await, ReorgImpact::AncestorUnavailableOrBeyondPolicy);
    let probe = harness.probe(&adapter).await;
    assert_eq!(probe.checkpoint().await.unwrap().height, START_HEIGHT + 1, "no rewind on unavailable ancestor");
    assert_eq!(probe.canonical_deposits().await.len(), 1);
}

/// After a restart with cold caches and the source block pruned away, every
/// supported read answers through the production observation reader and the
/// normalized probes with zero historical Bitcoin RPC calls (as counted by
/// the harness's transport spy); locally missing data halts as a typed
/// missing dependency instead of falling back to RPC.
///
/// The end-to-end variant against a real `txindex=0` pruned regtest node
/// lands with the source shell, which owns real RPC; this generic fixture
/// does not claim it.
pub async fn pruned_restart_has_zero_historical_fallback<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    let funding = envelopes::payment_tx(envelopes::seed_outpoint(1), Amount::from_sat(70_000), bridge_script());
    let funded = OutPoint { txid: funding.compute_txid(), vout: 0 };
    let deposit =
        envelopes::deposit_tx(envelopes::seed_outpoint(2), Amount::from_sat(50_000), bridge_script(), RECEIVER_A);
    let deposit_txid = deposit.compute_txid();
    let deposit_wtxid = deposit.compute_wtxid();
    let anchor = envelopes::anchor(START_HEIGHT, T0);
    apply_ok(harness, &adapter, &envelopes::envelope(anchor, vec![funding, deposit])).await;

    // Process restart: fresh instances, empty caches. The source block is
    // pruned from here on: only local reads may answer.
    let adapter = harness.restart(adapter).await;
    harness.reset_historical_rpc_count().await;

    // Production reader path.
    let reader = harness.observation_reader(&adapter).await;
    let observed = reader.canonical_observed_tx(&deposit_txid).await.expect("read").expect("deposit resolves locally");
    assert_eq!(observed.variant().wtxid(), deposit_wtxid);
    assert_eq!(observed.inclusion().block_hash, anchor.hash);
    let tracked = reader.tracked_output(&funded).await.expect("read").expect("tracked TxOut resolves locally");
    assert_eq!(tracked.value, Amount::from_sat(70_000));

    // Normalized domain read-back.
    let probe = harness.probe(&adapter).await;
    assert_eq!(probe.tracked_output(&funded).await.unwrap().canonical_spender, None, "spend state resolves locally");
    let deposits = probe.canonical_deposits().await;
    assert_eq!(deposits.len(), 1);
    assert_eq!(deposits[0].header_time, T0, "block time from stored header data");

    assert_eq!(
        harness.historical_rpc_count().await,
        0,
        "all supported historical reads must resolve with zero forbidden RPC calls"
    );

    // Local data loss halts as a typed missing dependency, never a
    // fallback, never a silent success.
    harness.delete_raw_variants(&adapter, &deposit_txid).await;
    let reader = harness.observation_reader(&adapter).await;
    let after_delete = reader.canonical_observed_tx(&deposit_txid).await;
    assert!(
        after_delete.is_err(),
        "bytes missing for a transaction the store claims as canonically observed are a \
         store inconsistency and must surface as a read error, never as never-observed: \
         {after_delete:?}"
    );
    let engine = harness.engine().await;
    let dependent = envelopes::envelope(
        envelopes::anchor(START_HEIGHT + 1, T0 + 600),
        vec![envelopes::payment_tx(
            OutPoint { txid: deposit_txid, vout: 0 },
            Amount::from_sat(1_000),
            bitcoin::ScriptBuf::new(),
        )],
    );
    let (draft, keys) = engine.inspect(&dependent, &default_context());
    assert!(
        keys.contains(&DependencyKey::TrackedOutput(OutPoint { txid: deposit_txid, vout: 0 })),
        "the spent tracked outpoint must be among the listed dependencies, got {keys:?}"
    );
    // Coverage says this block was processed, so the store failing to
    // answer is Unavailable, which must halt.
    match engine.finalize(draft, &all_unavailable(&keys)) {
        Err(FinalizeFailure::MissingDependency(_)) => {}
        Err(other) => panic!("lost local data must halt as MissingDependency, got {other:?}"),
        Ok(_) => panic!("lost local data must not finalize"),
    }
    assert_eq!(harness.historical_rpc_count().await, 0, "missing local data must never trigger an RPC fallback");
}

/// The genesis message initializes wallets and version exactly once: a
/// second bootstrap is rejected durably and changes nothing.
pub async fn bootstrap_exactly_once<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    let builder = harness.message_builder();
    apply_genesis(harness, &adapter).await;

    let probe = harness.probe(&adapter).await;
    let ctx = probe.protocol_context().await.expect("context after genesis");
    assert_eq!(ctx.wallets, builder.wallet_set());
    assert!(probe
        .applied_protocol_versions()
        .await
        .contains(&via_btc_ingestion::ProtocolVersionTag { minor: 26, patch: 0 }));

    let again = builder.bootstrap_tx(
        &builder.wallet_set(),
        via_btc_ingestion::ProtocolVersionTag { minor: 27, patch: 0 },
        envelopes::seed_outpoint(0xB1),
    );
    let plan =
        apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 1, T0 + 600), vec![again]))
            .await;
    assert!(plan.events.is_empty(), "second bootstrap must not become an event");
    let probe = harness.probe(&adapter).await;
    assert!(
        probe.rejections().await.iter().any(|r| r.code == RejectionCode::InvalidBootstrap),
        "second bootstrap must leave a durable rejection"
    );
    assert_eq!(probe.protocol_context().await.unwrap().wallets, builder.wallet_set(), "wallets unchanged");
}

/// The attestation chain: a batch reference, a proof referencing it, then a
/// vote referencing the proof. The committed vote carries the resolved
/// batch identity, and a non-verifier vote is rejected.
pub async fn attestation_chain_commits_batch_identity<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::Verifier).await;
    let builder = harness.message_builder();
    apply_genesis(harness, &adapter).await;

    let batch = builder.batch_da_reference_tx(7, [3; 32], "blob-batch", envelopes::seed_outpoint(2));
    let batch_txid = batch.compute_txid();
    apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 1, T0 + 600), vec![batch])).await;

    let proof = builder.proof_da_reference_tx(batch_txid, "blob-proof", envelopes::seed_outpoint(3));
    let proof_txid = proof.compute_txid();
    apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 2, T0 + 1200), vec![proof]))
        .await;

    let vote = builder.attestation_tx(proof_txid, true, 0, envelopes::seed_outpoint(4));
    let bad_vote = builder.unauthorized_attestation_tx(proof_txid, envelopes::seed_outpoint(5));
    let anchor_v = envelopes::anchor(START_HEIGHT + 3, T0 + 1800);
    let plan = apply_ok(harness, &adapter, &envelopes::envelope(anchor_v, vec![vote, bad_vote])).await;

    let attn: Vec<_> = plan
        .events
        .iter()
        .filter_map(|e| match e {
            via_btc_ingestion::ProtocolEvent::ValidatorAttestation(a) => Some(a),
            _ => None,
        })
        .collect();
    assert_eq!(attn.len(), 1, "only the verifier-signed vote becomes an event");
    assert_eq!(attn[0].batch.l1_batch_index, 7, "vote must carry the resolved batch identity");
    assert_eq!(attn[0].batch.reveal_txid, batch_txid);

    let probe = harness.probe(&adapter).await;
    assert!(probe.batch_references().await.iter().any(|b| b.l1_batch_index == 7 && b.l1_batch_hash == [3; 32]));
    assert!(probe.proof_references().await.iter().any(|p| p.subject_txid == proof_txid && p.l1_batch_index == 7));
    let votes = probe.attestation_votes().await;
    assert_eq!(
        votes,
        vec![VoteFact {
            ordinal: via_btc_ingestion::EventOrdinal { tx_index: 0, location: MessageLocation::Input(1) },
            l1_batch_index: 7,
            attester_script: builder.wallet_set().verifiers[0].clone(),
            ok: true,
            block_hash: anchor_v.hash,
        }]
    );
    assert!(probe.rejections().await.iter().any(|r| r.code == RejectionCode::Unauthorized));
}

/// Two attestations in one Bitcoin transaction remain distinct occurrences
/// through planning, projection, and durable read-back.
pub async fn multiple_attestations_in_one_transaction_preserve_occurrences<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::Verifier).await;
    let builder = harness.message_builder();
    apply_genesis(harness, &adapter).await;

    let batch = builder.batch_da_reference_tx(8, [4; 32], "blob-batch-2", envelopes::seed_outpoint(0x51));
    let batch_txid = batch.compute_txid();
    apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 1, T0 + 600), vec![batch])).await;

    let proof = builder.proof_da_reference_tx(batch_txid, "blob-proof-2", envelopes::seed_outpoint(0x52));
    let proof_txid = proof.compute_txid();
    apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 2, T0 + 1200), vec![proof]))
        .await;

    let votes_tx = builder.attestations_tx(
        proof_txid,
        &[(true, 0, envelopes::seed_outpoint(0x53)), (false, 0, envelopes::seed_outpoint(0x54))],
    );
    let anchor = envelopes::anchor(START_HEIGHT + 3, T0 + 1800);
    let plan = apply_ok(harness, &adapter, &envelopes::envelope(anchor, vec![votes_tx])).await;

    let ordinals: Vec<_> = plan
        .events
        .iter()
        .filter_map(|event| match event {
            via_btc_ingestion::ProtocolEvent::ValidatorAttestation(event) => Some(event.ordinal),
            _ => None,
        })
        .collect();
    assert_eq!(
        ordinals,
        vec![
            via_btc_ingestion::EventOrdinal { tx_index: 0, location: MessageLocation::Input(1) },
            via_btc_ingestion::EventOrdinal { tx_index: 0, location: MessageLocation::Input(3) },
        ],
        "input locations distinguish same-kind occurrences in one transaction"
    );

    let votes = harness.probe(&adapter).await.attestation_votes().await;
    assert_eq!(
        votes,
        vec![
            VoteFact {
                ordinal: ordinals[0],
                l1_batch_index: 8,
                attester_script: builder.wallet_set().verifiers[0].clone(),
                ok: true,
                block_hash: anchor.hash,
            },
            VoteFact {
                ordinal: ordinals[1],
                l1_batch_index: 8,
                attester_script: builder.wallet_set().verifiers[0].clone(),
                ok: false,
                block_hash: anchor.hash,
            },
        ],
        "both ordinal-distinct attestations must persist and read back"
    );
}

/// Wallet rotation lifecycle: a governance-authorized rotation applies, a
/// second same-role rotation in the same block is rejected, and an
/// unauthorized rotation changes nothing.
pub async fn rotation_lifecycle_and_conflicts<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    let builder = harness.message_builder();
    apply_genesis(harness, &adapter).await;
    let wallets = builder.wallet_set();

    // Fund two governance outputs so two rotations can each spend one.
    let g1 =
        envelopes::payment_tx(envelopes::seed_outpoint(0x21), Amount::from_sat(10_000), wallets.governance.clone());
    let g2 =
        envelopes::payment_tx(envelopes::seed_outpoint(0x22), Amount::from_sat(10_000), wallets.governance.clone());
    let gov1 = OutPoint { txid: g1.compute_txid(), vout: 0 };
    let gov2 = OutPoint { txid: g2.compute_txid(), vout: 0 };
    apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 1, T0 + 600), vec![g1, g2]))
        .await;

    let new_a = bitcoin::ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::from_byte_array([0x77; 20]));
    let new_b = bitcoin::ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::from_byte_array([0x78; 20]));
    let r1 = builder.sequencer_rotation_tx(&new_a, gov1);
    let r2 = builder.sequencer_rotation_tx(&new_b, gov2);
    let plan =
        apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 2, T0 + 1200), vec![r1, r2]))
            .await;
    assert_eq!(
        plan.events.iter().filter(|e| matches!(e, via_btc_ingestion::ProtocolEvent::WalletRotation(_))).count(),
        1,
        "exactly one rotation per role per block"
    );
    let probe = harness.probe(&adapter).await;
    assert_eq!(probe.protocol_context().await.unwrap().wallets.sequencer, new_a);
    assert!(probe.rejections().await.iter().any(|r| r.code == RejectionCode::ConflictingRoleUpdate));

    // Unauthorized: spends a plain (untracked) outpoint.
    let r3 = builder.sequencer_rotation_tx(&new_b, envelopes::seed_outpoint(0x24));
    apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 3, T0 + 1800), vec![r3])).await;
    let probe = harness.probe(&adapter).await;
    assert_eq!(probe.protocol_context().await.unwrap().wallets.sequencer, new_a, "unauthorized rotation is inert");
    assert!(probe.rejections().await.iter().any(|r| r.code == RejectionCode::Unauthorized));
}

/// Upgrades: a governance-activated proposal advances the protocol version;
/// activating a lower version is rejected as non-monotonic.
pub async fn upgrade_activation_is_monotonic<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::CoreSequencer).await;
    let builder = harness.message_builder();
    apply_genesis(harness, &adapter).await;
    let wallets = builder.wallet_set();

    let g1 =
        envelopes::payment_tx(envelopes::seed_outpoint(0x31), Amount::from_sat(10_000), wallets.governance.clone());
    let g2 =
        envelopes::payment_tx(envelopes::seed_outpoint(0x32), Amount::from_sat(10_000), wallets.governance.clone());
    let gov1 = OutPoint { txid: g1.compute_txid(), vout: 0 };
    let gov2 = OutPoint { txid: g2.compute_txid(), vout: 0 };
    let up = builder.upgrade_proposal_tx(
        via_btc_ingestion::ProtocolVersionTag { minor: 27, patch: 0 },
        vec![([0x11; 20], [0x21; 32])],
        envelopes::seed_outpoint(0x33),
    );
    let up_txid = up.compute_txid();
    let down = builder.upgrade_proposal_tx(
        via_btc_ingestion::ProtocolVersionTag { minor: 25, patch: 0 },
        vec![([0x12; 20], [0x22; 32])],
        envelopes::seed_outpoint(0x34),
    );
    let down_txid = down.compute_txid();
    apply_ok(
        harness,
        &adapter,
        &envelopes::envelope(envelopes::anchor(START_HEIGHT + 1, T0 + 600), vec![g1, g2, up, down]),
    )
    .await;

    let act = builder.upgrade_activation_tx(up_txid, gov1);
    apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 2, T0 + 1200), vec![act])).await;
    let probe = harness.probe(&adapter).await;
    assert_eq!(
        probe.protocol_context().await.unwrap().protocol_version,
        via_btc_ingestion::ProtocolVersionTag { minor: 27, patch: 0 }
    );
    assert!(probe
        .applied_protocol_versions()
        .await
        .contains(&via_btc_ingestion::ProtocolVersionTag { minor: 27, patch: 0 }));

    let act_down = builder.upgrade_activation_tx(down_txid, gov2);
    apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 3, T0 + 1800), vec![act_down]))
        .await;
    let probe = harness.probe(&adapter).await;
    assert!(probe.rejections().await.iter().any(|r| r.code == RejectionCode::NonMonotonicUpgrade));
    assert_eq!(
        probe.protocol_context().await.unwrap().protocol_version,
        via_btc_ingestion::ProtocolVersionTag { minor: 27, patch: 0 },
        "rejected activation must not change the version"
    );
}

/// Withdrawals are authorized by spending a bridge-tracked output; anything
/// else is rejected and projects nothing.
pub async fn withdrawal_requires_bridge_input<H: TestHarness>(harness: &H) {
    let adapter = harness.fresh_adapter(Role::Verifier).await;
    let builder = harness.message_builder();
    apply_genesis(harness, &adapter).await;
    let wallets = builder.wallet_set();

    let funding =
        envelopes::payment_tx(envelopes::seed_outpoint(0x41), Amount::from_sat(100_000), wallets.bridge.clone());
    let bridge_prev = OutPoint { txid: funding.compute_txid(), vout: 0 };
    apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 1, T0 + 600), vec![funding]))
        .await;

    let receiver = wallets.governance.clone();
    let w = builder.withdrawal_tx(&[(receiver.clone(), 40_000, [9u8; 8])], bridge_prev);
    let w_txid = w.compute_txid();
    apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 2, T0 + 1200), vec![w])).await;
    let probe = harness.probe(&adapter).await;
    let facts = probe.bridge_withdrawals().await;
    assert_eq!(
        facts,
        vec![WithdrawalFact { subject_txid: w_txid, l2_id: [9u8; 8], receiver_script: receiver, amount_sat: 40_000 }]
    );

    let bad = builder.withdrawal_tx(&[(wallets.governance.clone(), 1_000, [8u8; 8])], envelopes::seed_outpoint(0x42));
    apply_ok(harness, &adapter, &envelopes::envelope(envelopes::anchor(START_HEIGHT + 3, T0 + 1800), vec![bad])).await;
    let probe = harness.probe(&adapter).await;
    assert!(probe.rejections().await.iter().any(|r| r.code == RejectionCode::Unauthorized));
    assert_eq!(probe.bridge_withdrawals().await.len(), 1, "unauthorized withdrawal projects nothing");
}

/// The sequencer, verifier, and indexer given the same block must produce
/// the same plan hash, the same checkpoint (including context binding and
/// last plan hash), and the same normalized deposit facts, differing only in
/// their per-role receipts.
pub async fn three_adapter_semantic_equivalence<H: TestHarness>(harness: &H) {
    let envelope = envelopes::envelope(
        envelopes::anchor(START_HEIGHT, T0),
        vec![
            envelopes::deposit_tx(envelopes::seed_outpoint(1), Amount::from_sat(50_000), bridge_script(), RECEIVER_A),
            envelopes::dual_op_return_deposit_tx(
                envelopes::seed_outpoint(2),
                Amount::from_sat(10_000),
                bridge_script(),
                RECEIVER_A,
                RECEIVER_B,
            ),
        ],
    );

    let mut plan_hashes = Vec::new();
    let mut all_deposits = Vec::new();
    let mut checkpoints = Vec::new();
    for role in [Role::CoreSequencer, Role::Verifier, Role::StandaloneIndexer] {
        let adapter = harness.fresh_adapter(role).await;
        let plan = plan_block(harness, &adapter, &envelope, &default_context()).await;
        let receipt = adapter.apply_block(None, &plan).await.expect("apply");
        receipt.validate(&plan).expect("receipt partitions the plan's effects");
        assert_eq!(receipt.role, role);
        assert_eq!(receipt.plan_hash, plan_hash(&plan));
        assert_eq!(receipt.applied, plan.effect_ids(), "{role:?}: the deposit slice applies everywhere");
        plan_hashes.push(plan_hash(&plan));
        let probe = harness.probe(&adapter).await;
        checkpoints.push(probe.checkpoint().await.unwrap());
        let mut deposits = probe.canonical_deposits().await;
        deposits.sort();
        all_deposits.push(deposits);
    }
    assert!(plan_hashes.windows(2).all(|w| w[0] == w[1]), "role-neutral plan hash must match across roles");
    assert!(
        checkpoints.windows(2).all(|w| w[0] == w[1]),
        "checkpoints (incl. context binding and last plan hash) must match"
    );
    assert!(
        all_deposits.windows(2).all(|w| w[0] == w[1]),
        "normalized deposit facts must be equivalent across all three adapters"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn witness_variant_changes_wtxid_only() {
        let base =
            envelopes::deposit_tx(envelopes::seed_outpoint(1), Amount::from_sat(50_000), bridge_script(), RECEIVER_A);
        let a = envelopes::with_witness(base.clone(), b"a");
        let b = envelopes::with_witness(base, b"b");
        assert_eq!(a.compute_txid(), b.compute_txid());
        assert_ne!(a.compute_wtxid(), b.compute_wtxid());
    }

    #[test]
    fn builders_are_deterministic() {
        let mk = || {
            envelopes::deposit_tx(envelopes::seed_outpoint(1), Amount::from_sat(50_000), bridge_script(), RECEIVER_A)
                .compute_txid()
        };
        assert_eq!(mk(), mk());
    }

    #[test]
    fn fork_anchor_diverges_from_main_branch() {
        let a = envelopes::anchor(101, T0);
        let f = envelopes::fork_anchor(1, 101, envelopes::branch_hash(0, 100), T0);
        assert_ne!(a.hash, f.hash);
        assert_eq!(a.prev_hash, f.prev_hash);
    }
}
