//! Typed contract for the Via ingestion kernel
//! (architecture: `docs/ingestion/ingestion-kernel.md`).
//!
//! Kernel implementations and all three adapters (core, verifier,
//! standalone indexer) compile against this crate; the acceptance suite in
//! `via_btc_ingestion_tests` asserts its invariants. No type in this crate
//! can carry an RPC connection: the engine only ever sees block data and
//! local reads ([`ObservationReader`]).

use std::collections::BTreeMap;

use async_trait::async_trait;
use bitcoin::{
    consensus::{Decodable, Encodable},
    hashes::{sha256d, Hash},
    Amount, BlockHash, Network, OutPoint, ScriptBuf, Transaction, Txid, Wtxid,
};
use serde::{Deserialize, Serialize};

/// Version of the kernel's interpretation rules. Bumped whenever the meaning
/// of any event or disposition changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KernelVersion(pub u32);

/// Version of the observation-closure rule (which transactions must be
/// retained at observation time). Persisted with the checkpoint so a newer
/// kernel never silently assumes observations an older rule did not capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObservationRuleVersion(pub u32);

/// 32-byte hash produced by the canonical encodings in this crate
/// (double-SHA256, stored in internal byte order).
pub type Hash32 = [u8; 32];

/// A block position on some branch. `prev_hash` makes the chain linkage
/// checkable without an RPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockAnchor {
    pub height: u64,
    pub hash: BlockHash,
    pub prev_hash: BlockHash,
    /// Header time of the block, the only timestamp the kernel may use.
    pub time: u32,
}

/// The last processed block for one schema family. `apply_block` and
/// `revert_to` compare-and-swap on this value. `context_hash` names the
/// protocol context in force after that block; a plan built from a
/// different context is rejected. `last_plan_hash` identifies the last
/// committed plan and is how a caller recovers from
/// [`ApplyError::CommitIndeterminate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub height: u64,
    pub hash: BlockHash,
    pub kernel_version: KernelVersion,
    pub observation_rule_version: ObservationRuleVersion,
    pub context_hash: Hash32,
    pub last_plan_hash: Hash32,
}

/// Everything the source shell hands the engine for one block.
/// Transactions carry full witness data; the engine derives txid, wtxid, and
/// raw bytes from them and never fetches anything else.
#[derive(Clone, Debug)]
pub struct BitcoinBlockEnvelope {
    pub network: Network,
    pub anchor: BlockAnchor,
    pub transactions: Vec<Transaction>,
}

/// Immutable raw transaction bytes keyed by `(txid, wtxid)`, never txid
/// alone: a txid does not commit to witness data, and Via inscriptions live
/// in witness data. Construction and deserialization verify both hashes
/// against the bytes, so an entry whose bytes do not match its claimed
/// identities cannot exist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawTxVariantUnchecked", into = "RawTxVariantUnchecked")]
pub struct RawTxVariant {
    txid: Txid,
    wtxid: Wtxid,
    raw: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
struct RawTxVariantUnchecked {
    txid: Txid,
    wtxid: Wtxid,
    raw: Vec<u8>,
}

impl From<RawTxVariant> for RawTxVariantUnchecked {
    fn from(v: RawTxVariant) -> Self {
        Self { txid: v.txid, wtxid: v.wtxid, raw: v.raw }
    }
}

impl TryFrom<RawTxVariantUnchecked> for RawTxVariant {
    type Error = String;
    fn try_from(u: RawTxVariantUnchecked) -> Result<Self, Self::Error> {
        let v = RawTxVariant::from_raw(u.raw).map_err(|e| e.to_string())?;
        if v.txid != u.txid || v.wtxid != u.wtxid {
            return Err("raw bytes do not match the claimed txid/wtxid".into());
        }
        Ok(v)
    }
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("undecodable raw transaction bytes: {0}")]
pub struct RawBytesError(String);

impl RawTxVariant {
    /// Build from a full transaction, deriving all identities from the bytes.
    pub fn from_transaction(tx: &Transaction) -> Self {
        let mut raw = Vec::new();
        tx.consensus_encode(&mut raw).expect("in-memory encoding cannot fail");
        Self { txid: tx.compute_txid(), wtxid: tx.compute_wtxid(), raw }
    }

    /// Build from raw bytes, recomputing both identities. This is how
    /// stored bytes are re-verified on every read. The bytes must be
    /// exactly one transaction; trailing bytes are rejected, or two
    /// different blobs could share one identity pair.
    pub fn from_raw(raw: Vec<u8>) -> Result<Self, RawBytesError> {
        let mut slice = raw.as_slice();
        let tx = Transaction::consensus_decode(&mut slice).map_err(|e| RawBytesError(e.to_string()))?;
        if !slice.is_empty() {
            return Err(RawBytesError(format!("{} trailing bytes after the transaction", slice.len())));
        }
        Ok(Self { txid: tx.compute_txid(), wtxid: tx.compute_wtxid(), raw })
    }

    pub fn txid(&self) -> Txid {
        self.txid
    }
    pub fn wtxid(&self) -> Wtxid {
        self.wtxid
    }
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }
}

/// One occurrence of a transaction in a block on some branch, keyed by
/// `(block_hash, tx_index)`. A transaction orphaned and re-mined keeps its
/// raw variant, orphans the old inclusion, gains a new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inclusion {
    pub block_hash: BlockHash,
    pub height: u64,
    pub tx_index: u32,
    pub txid: Txid,
    pub wtxid: Wtxid,
}

/// Why an output is tracked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackedRole {
    Bridge,
    Sequencer,
    Governance,
}

impl TrackedRole {
    /// Stable wire tag: never derived from declaration order.
    pub fn wire_tag(self) -> u32 {
        match self {
            TrackedRole::Bridge => 0,
            TrackedRole::Sequencer => 1,
            TrackedRole::Governance => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedOutputCreate {
    pub outpoint: OutPoint,
    pub value: Amount,
    pub script_pubkey: ScriptBuf,
    pub role: TrackedRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedOutputSpend {
    pub outpoint: OutPoint,
    pub spending_txid: Txid,
    pub spending_wtxid: Wtxid,
    pub input_index: u32,
}

/// Where a message sits inside a block: transaction index first, then the
/// message's position inside that transaction. For a message encoded in
/// outputs, `message_ordinal` is the lowest output index carrying the
/// message; for future witness-encoded messages, the input index carrying
/// the inscription. Always derived from physical placement, never from
/// enum or map ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventOrdinal {
    pub tx_index: u32,
    pub message_ordinal: u32,
}

/// Closed set of event kinds with stable wire tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EventKind {
    Deposit,
}

impl EventKind {
    pub fn wire_tag(self) -> u32 {
        match self {
            EventKind::Deposit => 0,
        }
    }
}

/// What an event is about. One message can govern several subjects (a
/// receiver message covering several bridge-paying outputs produces one
/// deposit per output), so the subject is part of the effect identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EffectSubject {
    Output(OutPoint),
}

impl EffectSubject {
    pub fn wire_tag(self) -> u32 {
        match self {
            EffectSubject::Output(_) => 0,
        }
    }
}

/// Globally unique, branch-anchored identity of one event effect. This is
/// the key used in projection receipts, downstream-consumption records,
/// rejection records, and reorg impacts. The block hash (not the height)
/// anchors it: competing blocks at one height produce distinct identities.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EffectId {
    pub block_hash: BlockHash,
    pub ordinal: EventOrdinal,
    pub kind: EventKind,
    pub subject: EffectSubject,
}

/// How a deposit was encoded in the transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepositEncoding {
    Inscription,
    OpReturn,
    /// Both encodings present and agreeing on the receiver. Disagreement is
    /// not representable here: it is an invalid-input disposition.
    Both,
}

impl DepositEncoding {
    pub fn wire_tag(self) -> u32 {
        match self {
            DepositEncoding::Inscription => 0,
            DepositEncoding::OpReturn => 1,
            DepositEncoding::Both => 2,
        }
    }
}

/// One deposit per bridge-paying output, keyed by that output's `OutPoint`,
/// with the amount equal to that output's value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositObserved {
    pub ordinal: EventOrdinal,
    pub subject: OutPoint,
    pub amount: Amount,
    /// L2 receiver address bytes as parsed from the encoding.
    pub receiver: Vec<u8>,
    pub encoding: DepositEncoding,
}

/// Typed protocol events, ordered by [`EventOrdinal`]. The enum is
/// exhaustive on purpose: adding a variant breaks the build in every
/// adapter until that adapter decides how to handle it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolEvent {
    DepositObserved(DepositObserved),
}

impl ProtocolEvent {
    pub fn ordinal(&self) -> EventOrdinal {
        match self {
            ProtocolEvent::DepositObserved(e) => e.ordinal,
        }
    }

    pub fn kind(&self) -> EventKind {
        match self {
            ProtocolEvent::DepositObserved(_) => EventKind::Deposit,
        }
    }

    pub fn subject(&self) -> EffectSubject {
        match self {
            ProtocolEvent::DepositObserved(e) => EffectSubject::Output(e.subject),
        }
    }

    /// The anchored effect identity of this event inside the given block.
    pub fn effect_id(&self, block_hash: BlockHash) -> EffectId {
        EffectId { block_hash, ordinal: self.ordinal(), kind: self.kind(), subject: self.subject() }
    }
}

/// Typed reasons for rejecting input. The code is part of the plan hash;
/// free-text diagnostics are not, so two correct parsers phrasing the same
/// rejection differently still produce identical plans.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectionCode {
    MalformedMessage,
    ConflictingReceiverEncodings,
    ConflictingRoleUpdate,
}

impl RejectionCode {
    pub fn wire_tag(self) -> u32 {
        match self {
            RejectionCode::MalformedMessage => 0,
            RejectionCode::ConflictingReceiverEncodings => 1,
            RejectionCode::ConflictingRoleUpdate => 2,
        }
    }
}

/// Non-event outcomes recorded in the plan. There is no `IrrelevantToRole`
/// here on purpose: that is a per-role decision and lives in the
/// [`ProjectionReceipt`], so the plan hash stays identical across roles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispositionKind {
    /// Malformed or invalid Bitcoin data. This is a final verdict, so the
    /// cursor advances; missing data never lands here (it halts instead).
    /// `detail` is a human diagnostic and not part of the plan hash.
    RejectedInvalid { code: RejectionCode, detail: String },
    /// A duplicate visible from the block and resolved dependencies alone.
    /// A role's own database never decides this, or plans would differ per
    /// role. Replays of an already committed plan are caught by the
    /// checkpoint, not here.
    Duplicate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disposition {
    pub ordinal: EventOrdinal,
    pub kind: DispositionKind,
}

/// Protocol state the engine needs to interpret a block. Each plan records
/// the hash of the context it was built from, and the checkpoint rejects a
/// plan built from any other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolContext {
    pub version: u32,
    pub bridge_script_pubkey: ScriptBuf,
}

const CONTEXT_WIRE_MAGIC: &[u8; 8] = b"VIA_CTX\0";

impl ProtocolContext {
    /// Canonical fingerprint of this context (double-SHA256 over a
    /// length-delimited encoding with a domain prefix).
    pub fn context_hash(&self) -> Hash32 {
        let mut out = Vec::new();
        out.extend_from_slice(CONTEXT_WIRE_MAGIC);
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&(self.bridge_script_pubkey.as_bytes().len() as u64).to_be_bytes());
        out.extend_from_slice(self.bridge_script_pubkey.as_bytes());
        sha256d::Hash::hash(&out).to_byte_array()
    }
}

/// Wire-format identity of the canonical plan encoding. Distinct from
/// [`KernelVersion`]: that versions interpretation rules, this versions the
/// byte grammar itself.
pub const PLAN_WIRE_MAGIC: &[u8; 12] = b"VIA_BLKPLAN\0";
pub const PLAN_WIRE_FORMAT_VERSION: u32 = 1;

/// Why a plan failed structural validation. An adapter must reject an
/// invalid plan before writing anything.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlanValidationError {
    #[error("{collection} not sorted by its canonical key or contains duplicates at position {position}")]
    NotSortedOrDuplicate { collection: &'static str, position: usize },
    #[error("inclusion at tx_index {tx_index} does not match any raw variant")]
    InclusionWithoutVariant { tx_index: u32 },
    #[error("inclusion at tx_index {tx_index} is not anchored to this plan's block")]
    InclusionOutsideAnchor { tx_index: u32 },
    #[error("event at tx_index {tx_index} references a transaction with no inclusion in this block")]
    EventWithoutInclusion { tx_index: u32 },
}

/// What one Bitcoin block means for Via: the deterministic, versioned
/// output of the protocol engine. All three roles produce byte-identical
/// plans for the same envelope, context, and dependencies.
///
/// Canonical byte grammar (see `canonical_bytes`): integers big-endian;
/// byte strings length-prefixed with u64; collections count-prefixed with
/// u64 and sorted by their canonical key; 32-byte identities in Bitcoin
/// internal byte order, NOT the reversed order shown in txid strings; enum
/// tags are the explicit `wire_tag` constants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockPlan {
    pub kernel_version: KernelVersion,
    pub observation_rule_version: ObservationRuleVersion,
    pub network: Network,
    pub anchor: BlockAnchor,
    /// Hash of the [`ProtocolContext`] this plan was built from.
    pub input_context_hash: Hash32,
    /// Sorted by (txid, wtxid), unique.
    pub raw_variants: Vec<RawTxVariant>,
    /// Sorted by tx_index, unique; every inclusion references a raw variant.
    pub inclusions: Vec<Inclusion>,
    /// Sorted by outpoint, unique.
    pub tracked_creates: Vec<TrackedOutputCreate>,
    /// Sorted by outpoint, unique.
    pub tracked_spends: Vec<TrackedOutputSpend>,
    /// Sorted by ordinal; effect identities unique.
    pub events: Vec<ProtocolEvent>,
    /// Sorted by ordinal.
    pub dispositions: Vec<Disposition>,
    pub next_context: ProtocolContext,
}

impl BlockPlan {
    /// Structural validation: ordering, uniqueness, cross-references. An
    /// invalid plan has no canonical form, and `apply_block` must reject it.
    pub fn validate(&self) -> Result<(), PlanValidationError> {
        fn sorted_unique<T, K: Ord>(
            items: &[T], key: impl Fn(&T) -> K, name: &'static str,
        ) -> Result<(), PlanValidationError> {
            for (i, w) in items.windows(2).enumerate() {
                if key(&w[0]) >= key(&w[1]) {
                    return Err(PlanValidationError::NotSortedOrDuplicate { collection: name, position: i + 1 });
                }
            }
            Ok(())
        }
        sorted_unique(&self.raw_variants, |v| (v.txid(), v.wtxid()), "raw_variants")?;
        sorted_unique(&self.inclusions, |i| i.tx_index, "inclusions")?;
        sorted_unique(&self.tracked_creates, |c| c.outpoint, "tracked_creates")?;
        sorted_unique(&self.tracked_spends, |s| s.outpoint, "tracked_spends")?;
        sorted_unique(&self.events, |e| (e.ordinal(), e.kind(), e.subject()), "events")?;
        sorted_unique(&self.dispositions, |d| d.ordinal, "dispositions")?;

        for inc in &self.inclusions {
            if inc.block_hash != self.anchor.hash || inc.height != self.anchor.height {
                return Err(PlanValidationError::InclusionOutsideAnchor { tx_index: inc.tx_index });
            }
            if !self.raw_variants.iter().any(|v| v.txid() == inc.txid && v.wtxid() == inc.wtxid) {
                return Err(PlanValidationError::InclusionWithoutVariant { tx_index: inc.tx_index });
            }
        }
        for e in &self.events {
            let subject_txid = match e {
                ProtocolEvent::DepositObserved(d) => d.subject.txid,
            };
            if !self.inclusions.iter().any(|i| i.txid == subject_txid) {
                return Err(PlanValidationError::EventWithoutInclusion { tx_index: e.ordinal().tx_index });
            }
        }
        Ok(())
    }

    /// Canonical bytes for hashing; validates first. Free-text fields
    /// (`detail` in rejections) are excluded so plan identity depends only
    /// on typed content.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, PlanValidationError> {
        self.validate()?;
        let mut out = Vec::new();
        let put_u32 = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_be_bytes());
        let put_u64 = |out: &mut Vec<u8>, v: u64| out.extend_from_slice(&v.to_be_bytes());
        let put_bytes = |out: &mut Vec<u8>, b: &[u8]| {
            out.extend_from_slice(&(b.len() as u64).to_be_bytes());
            out.extend_from_slice(b);
        };
        let put_hash32 = |out: &mut Vec<u8>, b: &[u8]| {
            debug_assert_eq!(b.len(), 32);
            out.extend_from_slice(b);
        };
        let put_outpoint = |out: &mut Vec<u8>, op: &OutPoint| {
            out.extend_from_slice(op.txid.as_ref());
            out.extend_from_slice(&op.vout.to_be_bytes());
        };

        out.extend_from_slice(PLAN_WIRE_MAGIC);
        put_u32(&mut out, PLAN_WIRE_FORMAT_VERSION);
        put_u32(&mut out, self.kernel_version.0);
        put_u32(&mut out, self.observation_rule_version.0);
        // Network as its 4-byte P2P magic: stable across library versions,
        // unlike Display text.
        out.extend_from_slice(&self.network.magic().to_bytes());
        put_u64(&mut out, self.anchor.height);
        put_hash32(&mut out, self.anchor.hash.as_ref());
        put_hash32(&mut out, self.anchor.prev_hash.as_ref());
        put_u32(&mut out, self.anchor.time);
        put_hash32(&mut out, &self.input_context_hash);

        put_u64(&mut out, self.raw_variants.len() as u64);
        for v in &self.raw_variants {
            put_hash32(&mut out, v.txid().as_ref());
            put_hash32(&mut out, v.wtxid().as_ref());
            put_bytes(&mut out, v.raw());
        }
        put_u64(&mut out, self.inclusions.len() as u64);
        for i in &self.inclusions {
            put_hash32(&mut out, i.block_hash.as_ref());
            put_u64(&mut out, i.height);
            put_u32(&mut out, i.tx_index);
            put_hash32(&mut out, i.txid.as_ref());
            put_hash32(&mut out, i.wtxid.as_ref());
        }
        put_u64(&mut out, self.tracked_creates.len() as u64);
        for c in &self.tracked_creates {
            put_outpoint(&mut out, &c.outpoint);
            put_u64(&mut out, c.value.to_sat());
            put_bytes(&mut out, c.script_pubkey.as_bytes());
            put_u32(&mut out, c.role.wire_tag());
        }
        put_u64(&mut out, self.tracked_spends.len() as u64);
        for s in &self.tracked_spends {
            put_outpoint(&mut out, &s.outpoint);
            put_hash32(&mut out, s.spending_txid.as_ref());
            put_hash32(&mut out, s.spending_wtxid.as_ref());
            put_u32(&mut out, s.input_index);
        }
        put_u64(&mut out, self.events.len() as u64);
        for e in &self.events {
            put_u32(&mut out, e.kind().wire_tag());
            match e {
                ProtocolEvent::DepositObserved(d) => {
                    put_u32(&mut out, d.ordinal.tx_index);
                    put_u32(&mut out, d.ordinal.message_ordinal);
                    put_outpoint(&mut out, &d.subject);
                    put_u64(&mut out, d.amount.to_sat());
                    put_bytes(&mut out, &d.receiver);
                    put_u32(&mut out, d.encoding.wire_tag());
                }
            }
        }
        put_u64(&mut out, self.dispositions.len() as u64);
        for d in &self.dispositions {
            put_u32(&mut out, d.ordinal.tx_index);
            put_u32(&mut out, d.ordinal.message_ordinal);
            match &d.kind {
                DispositionKind::RejectedInvalid { code, detail: _ } => {
                    put_u32(&mut out, 0);
                    put_u32(&mut out, code.wire_tag());
                }
                DispositionKind::Duplicate => put_u32(&mut out, 1),
            }
        }
        put_u32(&mut out, self.next_context.version);
        put_bytes(&mut out, self.next_context.bridge_script_pubkey.as_bytes());
        Ok(out)
    }

    pub fn plan_hash(&self) -> Result<Hash32, PlanValidationError> {
        Ok(sha256d::Hash::hash(&self.canonical_bytes()?).to_byte_array())
    }

    /// Anchored effect identities of all events, in plan order.
    pub fn effect_ids(&self) -> Vec<EffectId> {
        self.events.iter().map(|e| e.effect_id(self.anchor.hash)).collect()
    }
}

/// A dependency the engine needs resolved before it can finalize a plan.
/// Resolvable only from the current block or the local observation store.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DependencyKey {
    /// A historical transaction, resolved to its canonical observation so
    /// the finalizer never guesses among witness variants.
    RawTx(Txid),
    TrackedOutput(OutPoint),
}

/// A historical transaction as canonically observed: the inclusion selects
/// the wtxid, the variant carries verified bytes for exactly that wtxid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalObservedTx {
    pub inclusion: Inclusion,
    pub variant: RawTxVariant,
}

/// The three answers a local store can give for a dependency.
/// `KnownAbsent` means the store definitely does not have it.
/// `Unavailable` means the store could not answer even though it should
/// have; that halts ingestion as a missing dependency.
#[derive(Clone, Debug)]
pub enum Resolution<T> {
    Present(T),
    KnownAbsent,
    Unavailable,
}

#[derive(Clone, Debug)]
pub enum ResolvedDependency {
    RawTx(Resolution<CanonicalObservedTx>),
    TrackedOutput(Resolution<TrackedOutputCreate>),
}

/// Why plan finalization halted. Missing data is never treated as invalid
/// data, and the cursor never advances past any of these.
#[derive(Clone, Debug, thiserror::Error)]
pub enum FinalizeFailure {
    #[error("missing required dependencies: {0:?}")]
    MissingDependency(Vec<DependencyKey>),
    #[error("dependency present but corrupt: {0:?}")]
    CorruptDependency(DependencyKey),
    #[error("recognized but unsupported event (kind {kind}) at {ordinal:?}")]
    UnsupportedValidEvent { ordinal: EventOrdinal, kind: String },
    #[error("infrastructure failure: {0}")]
    Infrastructure(String),
}

/// Failure of a local observation read. `Corrupt` means stored data failed
/// verification (e.g. raw bytes not matching their claimed hashes). It
/// surfaces as a corrupt dependency, never as invalid Bitcoin input, and
/// never triggers an RPC fallback.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ObservationReadError {
    #[error("infrastructure: {0}")]
    Infrastructure(String),
    #[error("corrupt local observation: {0}")]
    Corrupt(String),
}

/// Local-only reads used to resolve the engine's dependencies.
/// Implementations answer purely from the local observation store and must
/// live in crates without an RPC-capable dependency.
#[async_trait]
pub trait ObservationReader: Send + Sync {
    /// The inclusion on the current canonical chain plus the raw variant
    /// matching that inclusion's wtxid. `Ok(None)` means "never canonically
    /// observed".
    async fn canonical_observed_tx(&self, txid: &Txid) -> Result<Option<CanonicalObservedTx>, ObservationReadError>;
    /// All stored witness variants for a txid.
    async fn raw_variants(&self, txid: &Txid) -> Result<Vec<RawTxVariant>, ObservationReadError>;
    async fn tracked_output(&self, outpoint: &OutPoint) -> Result<Option<TrackedOutputCreate>, ObservationReadError>;
}

/// Infrastructure error (database, serialization, internal). Never
/// convertible into an invalid-input disposition.
#[derive(Clone, Debug, thiserror::Error)]
#[error("infrastructure: {0}")]
pub struct InfraError(pub String);

/// The deterministic protocol engine: two stages, no I/O in either.
pub trait ProtocolEngine {
    type Draft;

    /// Classify the envelope against the context and list the dependencies
    /// finalization needs. Discovery is conservative on purpose: the engine
    /// lists a `TrackedOutput` dependency for every input spending an
    /// outpoint not created in this envelope, and the store answers each
    /// with `Present` or `KnownAbsent` in one indexed lookup. The
    /// alternative, letting `inspect` read local state, would make it
    /// impure.
    fn inspect(&self, envelope: &BitcoinBlockEnvelope, context: &ProtocolContext) -> (Self::Draft, Vec<DependencyKey>);

    /// Pure finalization against pre-resolved dependencies.
    fn finalize(
        &self, draft: Self::Draft, dependencies: &BTreeMap<DependencyKey, ResolvedDependency>,
    ) -> Result<BlockPlan, FinalizeFailure>;
}

/// Which node shell is projecting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    CoreSequencer,
    Verifier,
    StandaloneIndexer,
}

/// Per-role outcome of applying a role-neutral plan. Separate from the plan
/// so the plan hash stays identical across roles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionReceipt {
    pub role: Role,
    pub plan_hash: Hash32,
    pub applied: Vec<EffectId>,
    pub irrelevant_to_role: Vec<EffectId>,
}

impl ProjectionReceipt {
    /// A valid receipt names the plan it belongs to (`plan_hash` must
    /// match) and partitions the plan's effects: `applied` and
    /// `irrelevant_to_role` are disjoint and their union is exactly the
    /// plan's effect identities.
    pub fn validate(&self, plan: &BlockPlan) -> Result<(), String> {
        let expected_hash = plan.plan_hash().map_err(|e| format!("plan has no canonical form: {e}"))?;
        if self.plan_hash != expected_hash {
            return Err("receipt plan_hash does not match the plan".into());
        }
        let mut claimed: Vec<EffectId> = self.applied.iter().chain(self.irrelevant_to_role.iter()).copied().collect();
        claimed.sort();
        if claimed.windows(2).any(|w| w[0] == w[1]) {
            return Err("receipt lists an effect twice".into());
        }
        let mut expected = plan.effect_ids();
        expected.sort();
        if claimed != expected {
            return Err(format!(
                "receipt does not partition the plan's effects: claimed {claimed:?}, plan has {expected:?}"
            ));
        }
        Ok(())
    }
}

/// Durable record of a reorg that crossed the downstream-consumption
/// boundary: ingestion halts until explicit operator recovery clears it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardReorgHalt {
    /// The first replacement-branch anchor that diverges from local state.
    pub divergence: BlockAnchor,
    /// The consumed effects that make automatic revert unsafe.
    pub affected: Vec<EffectId>,
}

/// Typed failure of `apply_block`. For every variant EXCEPT
/// `CommitIndeterminate`, zero writes of this call remain.
/// `CommitIndeterminate` (e.g. the connection died between commit and
/// acknowledgement) requires reloading the checkpoint and comparing
/// `last_plan_hash` with the attempted plan's hash: equal means this exact
/// plan committed (treat as success); different means it did not.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("stale checkpoint: expected {expected:?}, found {actual:?}")]
    StaleCheckpoint { expected: Box<Option<Checkpoint>>, actual: Box<Option<Checkpoint>> },
    #[error("plan does not extend checkpoint: plan height {plan_height}, prev {plan_prev}, checkpoint {checkpoint_hash} at {checkpoint_height}")]
    NotAdjacent { plan_height: u64, plan_prev: BlockHash, checkpoint_height: u64, checkpoint_hash: BlockHash },
    #[error("kernel/schema version mismatch: {0}")]
    VersionMismatch(String),
    #[error("plan built from a different protocol context than the checkpoint's")]
    ContextMismatch { plan_context: Hash32, checkpoint_context: Hash32 },
    #[error("structurally invalid plan: {0}")]
    InvalidPlan(#[from] PlanValidationError),
    #[error("recognized but unsupported event for this adapter: {effect:?}")]
    UnsupportedEvent { effect: EffectId },
    #[error("ingestion halted by durable hard-reorg record")]
    Halted(HardReorgHalt),
    #[error("commit outcome indeterminate for plan at height {height}: reload checkpoint and compare last_plan_hash")]
    CommitIndeterminate { height: u64, plan_hash: Hash32 },
    #[error(transparent)]
    Infrastructure(#[from] InfraError),
}

/// Receipt of a completed revert. `canonical_revision` increases with
/// every successful revert; readers (chiefly the standalone indexer) use
/// it to detect that canonical history was rewritten.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevertReceipt {
    pub reverted_to: BlockAnchor,
    pub canonical_revision: u64,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum RevertError {
    #[error("stale checkpoint: expected {expected:?}, found {actual:?}")]
    StaleCheckpoint { expected: Box<Checkpoint>, actual: Box<Option<Checkpoint>> },
    #[error("ancestor {0} not found in local canonical history")]
    UnknownAncestor(BlockHash),
    #[error("revert outcome indeterminate: reload checkpoint")]
    CommitIndeterminate,
    #[error(transparent)]
    Infrastructure(#[from] InfraError),
}

/// Coverage audit result over an anchored range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub from_height: u64,
    pub to_height: u64,
    pub contiguous: bool,
    pub unresolved_dependencies: Vec<DependencyKey>,
}

/// The database adapter, one per schema family.
///
/// `apply_block` is atomic: after it returns, or after a crash and restart,
/// either all of a block's writes are visible or none are. Replaying the
/// exact committed plan is detected through `Checkpoint::last_plan_hash`.
/// `revert_to` is atomic in the same way; raw variants stay (they are a
/// content-addressed cache), everything canonical reverts together, and the
/// canonical revision increments.
#[async_trait]
pub trait AggregateAdapter: Send + Sync {
    async fn load_checkpoint_and_context(&self) -> Result<Option<(Checkpoint, ProtocolContext)>, InfraError>;

    async fn apply_block(
        &self, expected_checkpoint: Option<Checkpoint>, plan: &BlockPlan,
    ) -> Result<ProjectionReceipt, ApplyError>;

    async fn revert_to(
        &self, expected_checkpoint: Checkpoint, ancestor: BlockAnchor,
    ) -> Result<RevertReceipt, RevertError>;

    /// Which of these effects crossed this role's downstream-consumption
    /// boundary (sealed L2 batch, emitted attestation). The standalone
    /// indexer has no boundary and always answers with an empty set.
    async fn downstream_consumed(&self, effects: &[EffectId]) -> Result<Vec<EffectId>, InfraError>;

    async fn record_hard_reorg_halt(&self, halt: HardReorgHalt) -> Result<(), InfraError>;

    async fn hard_reorg_halt(&self) -> Result<Option<HardReorgHalt>, InfraError>;

    async fn audit_coverage(&self, from_height: u64, to_height: u64) -> Result<CoverageReport, InfraError>;
}

/// Reorg classification: the safety frontier for automatic revert is
/// downstream consumption, never reorg depth or confirmation depth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReorgImpact {
    /// Divergence entirely above the processed checkpoint: no-op.
    NoProcessedImpact,
    /// All affected effects are local projections: automatic `revert_to`.
    ProjectionOnly { ancestor: BlockAnchor },
    /// Some affected effect crossed the role's downstream-consumption
    /// boundary: record a durable hard-reorg halt and alarm. Never
    /// partially rewind.
    DownstreamConsumed { ancestor: BlockAnchor, affected: Vec<EffectId> },
    /// No common ancestor within stored coverage, or beyond configured
    /// policy: halt. The window edge is a search bound, not an ancestor.
    AncestorUnavailableOrBeyondPolicy,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor() -> BlockAnchor {
        BlockAnchor {
            height: 100,
            hash: BlockHash::all_zeros(),
            prev_hash: BlockHash::all_zeros(),
            time: 1_700_000_000,
        }
    }

    fn context() -> ProtocolContext {
        ProtocolContext { version: 1, bridge_script_pubkey: ScriptBuf::new() }
    }

    fn dummy_plan() -> BlockPlan {
        BlockPlan {
            kernel_version: KernelVersion(1),
            observation_rule_version: ObservationRuleVersion(1),
            network: Network::Regtest,
            anchor: anchor(),
            input_context_hash: context().context_hash(),
            raw_variants: vec![],
            inclusions: vec![],
            tracked_creates: vec![],
            tracked_spends: vec![],
            events: vec![],
            dispositions: vec![],
            next_context: context(),
        }
    }

    #[test]
    fn plan_hash_is_deterministic() {
        assert_eq!(dummy_plan().plan_hash(), dummy_plan().plan_hash());
    }

    /// Frozen golden vector: any change to the canonical wire format must
    /// consciously update this constant AND bump PLAN_WIRE_FORMAT_VERSION.
    #[test]
    fn plan_hash_matches_frozen_golden() {
        use std::fmt::Write;
        let hash = dummy_plan().plan_hash().unwrap();
        let hex = hash.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
        assert_eq!(
            hex, "e0cab6ea11d174351ace38d0a82ec48e6301cc80d6984649c3602a77593ff3fb",
            "canonical wire format changed: if intentional, bump PLAN_WIRE_FORMAT_VERSION and refreeze"
        );
    }

    #[test]
    fn plan_hash_changes_with_anchor_version_and_context() {
        let base = dummy_plan().plan_hash().unwrap();
        let mut p = dummy_plan();
        p.anchor.height = 101;
        assert_ne!(base, p.plan_hash().unwrap());
        let mut p = dummy_plan();
        p.kernel_version = KernelVersion(2);
        assert_ne!(base, p.plan_hash().unwrap());
        let mut p = dummy_plan();
        p.input_context_hash = [1; 32];
        assert_ne!(base, p.plan_hash().unwrap());
    }

    #[test]
    fn rejection_detail_is_not_hashed() {
        let mk = |detail: &str| {
            let mut p = dummy_plan();
            p.dispositions = vec![Disposition {
                ordinal: EventOrdinal { tx_index: 0, message_ordinal: 0 },
                kind: DispositionKind::RejectedInvalid { code: RejectionCode::MalformedMessage, detail: detail.into() },
            }];
            p.plan_hash().unwrap()
        };
        assert_eq!(mk("phrased one way"), mk("phrased another way"));
    }

    #[test]
    fn unsorted_plan_has_no_canonical_form() {
        let mut p = dummy_plan();
        p.dispositions = vec![
            Disposition { ordinal: EventOrdinal { tx_index: 1, message_ordinal: 0 }, kind: DispositionKind::Duplicate },
            Disposition { ordinal: EventOrdinal { tx_index: 0, message_ordinal: 0 }, kind: DispositionKind::Duplicate },
        ];
        assert!(matches!(p.canonical_bytes(), Err(PlanValidationError::NotSortedOrDuplicate { .. })));
    }

    #[test]
    fn raw_variant_rejects_trailing_bytes() {
        let tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![],
            output: vec![],
        };
        let mut raw = RawTxVariant::from_transaction(&tx).raw().to_vec();
        raw.push(0x00);
        assert!(RawTxVariant::from_raw(raw).is_err());
    }

    #[test]
    fn inclusions_must_be_anchored_to_the_plan_block() {
        let tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![],
            output: vec![],
        };
        let variant = RawTxVariant::from_transaction(&tx);
        let mut p = dummy_plan();
        p.raw_variants = vec![variant.clone()];
        p.inclusions = vec![Inclusion {
            block_hash: BlockHash::all_zeros(),
            height: p.anchor.height + 1, // wrong height
            tx_index: 0,
            txid: variant.txid(),
            wtxid: variant.wtxid(),
        }];
        assert!(matches!(p.validate(), Err(PlanValidationError::InclusionOutsideAnchor { .. })));
    }

    #[test]
    fn events_must_reference_an_included_transaction() {
        let mut p = dummy_plan();
        p.events = vec![ProtocolEvent::DepositObserved(DepositObserved {
            ordinal: EventOrdinal { tx_index: 0, message_ordinal: 0 },
            subject: OutPoint { txid: Txid::all_zeros(), vout: 0 },
            amount: Amount::from_sat(1),
            receiver: vec![0; 20],
            encoding: DepositEncoding::OpReturn,
        })];
        assert!(matches!(p.validate(), Err(PlanValidationError::EventWithoutInclusion { .. })));
    }

    #[test]
    fn receipt_must_name_the_plan_it_belongs_to() {
        let plan = dummy_plan();
        let receipt = ProjectionReceipt {
            role: Role::CoreSequencer,
            plan_hash: [7; 32],
            applied: vec![],
            irrelevant_to_role: vec![],
        };
        assert!(receipt.validate(&plan).is_err());
    }

    #[test]
    fn raw_variant_rejects_mismatched_identities_on_deserialize() {
        let tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![],
            output: vec![],
        };
        let good = RawTxVariant::from_transaction(&tx);
        let json = serde_json::json!({
            "txid": Txid::all_zeros(),
            "wtxid": good.wtxid(),
            "raw": good.raw(),
        });
        assert!(serde_json::from_value::<RawTxVariant>(json).is_err());
    }

    #[test]
    fn receipt_must_partition_plan_effects() {
        let plan = dummy_plan();
        let ok = ProjectionReceipt {
            role: Role::CoreSequencer,
            plan_hash: plan.plan_hash().unwrap(),
            applied: vec![],
            irrelevant_to_role: vec![],
        };
        assert!(ok.validate(&plan).is_ok());
        let phantom = ProjectionReceipt {
            applied: vec![EffectId {
                block_hash: BlockHash::all_zeros(),
                ordinal: EventOrdinal { tx_index: 0, message_ordinal: 0 },
                kind: EventKind::Deposit,
                subject: EffectSubject::Output(OutPoint { txid: Txid::all_zeros(), vout: 0 }),
            }],
            ..ok
        };
        assert!(phantom.validate(&plan).is_err());
    }
}
