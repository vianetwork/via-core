#![deny(clippy::expect_used, clippy::unwrap_used)]

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
    // Encoding into an in-memory byte vector is infallible.
    #[allow(clippy::expect_used)]
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

/// Why an output is tracked. `Sequencer` is reserved: no observation rule
/// creates it yet (sequencer identity is input-signer based today), but the
/// wire tag is pinned so a future rule cannot renumber the others.
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

/// Which carrier inside a transaction holds a message. Inputs order before
/// outputs, so `Ord` on this type (inputs first, then outputs, each by
/// index) is the total message order inside one transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MessageLocation {
    /// Witness-encoded message: the input index carrying the inscription.
    Input(u32),
    /// Output-encoded message: the lowest output index carrying the
    /// message's encoding.
    Output(u32),
}

impl MessageLocation {
    pub fn wire_tag(self) -> u32 {
        match self {
            MessageLocation::Input(_) => 0,
            MessageLocation::Output(_) => 1,
        }
    }

    pub fn index(self) -> u32 {
        match self {
            MessageLocation::Input(i) | MessageLocation::Output(i) => i,
        }
    }
}

/// Where a message sits inside a block: transaction index first, then the
/// message's physical location in that transaction, never enum or map
/// ordering. A deposit encoded both ways (witness inscription plus an
/// agreeing OP_RETURN) is owned by its witness `Input` location; the
/// OP_RETURN copy is agreement evidence, not a second message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventOrdinal {
    pub tx_index: u32,
    pub location: MessageLocation,
}

/// Closed set of event kinds with stable wire tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EventKind {
    Deposit,
    L1BatchDAReference,
    ProofDAReference,
    ValidatorAttestation,
    SystemBootstrapping,
    SystemContractUpgradeProposal,
    SystemContractUpgradeActivation,
    BridgeWithdrawal,
    UpdateBridgeProposal,
    WalletRotation,
}

impl EventKind {
    pub fn wire_tag(self) -> u32 {
        match self {
            EventKind::Deposit => 0,
            EventKind::L1BatchDAReference => 1,
            EventKind::ProofDAReference => 2,
            EventKind::ValidatorAttestation => 3,
            EventKind::SystemBootstrapping => 4,
            EventKind::SystemContractUpgradeProposal => 5,
            EventKind::SystemContractUpgradeActivation => 6,
            EventKind::BridgeWithdrawal => 7,
            EventKind::UpdateBridgeProposal => 8,
            EventKind::WalletRotation => 9,
        }
    }
}

/// What an event is about. One message can govern several subjects (a
/// receiver message covering several bridge-paying outputs produces one
/// deposit per output), so the subject is part of the effect identity.
/// Events scoped to a whole message use the message transaction's txid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EffectSubject {
    Output(OutPoint),
    Tx(Txid),
}

impl EffectSubject {
    pub fn wire_tag(self) -> u32 {
        match self {
            EffectSubject::Output(_) => 0,
            EffectSubject::Tx(_) => 1,
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

/// An L2 (EVM-style) address as plain bytes.
pub type Address20 = [u8; 20];

/// Protocol semantic version, decoupled from any L2 framework type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProtocolVersionTag {
    pub minor: u32,
    pub patch: u32,
}

/// The system wallets in force, as script pubkeys. Scripts, not address
/// strings, so identity is byte-exact and network-independent. Empty
/// scripts mean "not yet bootstrapped" and match no transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletSet {
    pub sequencer: ScriptBuf,
    pub bridge: ScriptBuf,
    pub governance: ScriptBuf,
    /// Registration order; the order is part of the context identity, and
    /// entries must be unique (votes are counted per verifier identity).
    pub verifiers: Vec<ScriptBuf>,
}

impl WalletSet {
    /// All role scripts set. Before bootstrap every script is empty.
    pub fn is_bootstrapped(&self) -> bool {
        !self.sequencer.is_empty() && !self.bridge.is_empty() && !self.governance.is_empty()
    }

    /// Structural validity: verifier identities unique; a bootstrapped set
    /// has no empty verifier script.
    pub fn validate(&self) -> Result<(), String> {
        for (i, a) in self.verifiers.iter().enumerate() {
            if self.is_bootstrapped() && a.is_empty() {
                return Err(format!("verifier {i} has an empty script"));
            }
            if self.verifiers[..i].contains(a) {
                return Err(format!("duplicate verifier script at position {i}"));
            }
        }
        Ok(())
    }
}

/// One deposit per bridge-paying output, keyed by that output's `OutPoint`,
/// with the amount equal to that output's value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositObserved {
    pub ordinal: EventOrdinal,
    pub subject: OutPoint,
    pub amount: Amount,
    /// L2 receiver address as parsed from the encoding.
    pub receiver: Address20,
    /// Target L2 contract; all-zero means the protocol default bridge
    /// target.
    pub l2_contract: Address20,
    /// L2 call data; empty for plain value transfers.
    pub call_data: Vec<u8>,
    /// Script of the depositor's P2WPKH input when one is identifiable
    /// (the standalone indexer projects it as the deposit sender).
    pub sender_script: Option<ScriptBuf>,
    pub encoding: DepositEncoding,
}

/// Sequencer commitment of one L1 batch to the DA layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct L1BatchDAReferenceObserved {
    pub ordinal: EventOrdinal,
    pub subject_txid: Txid,
    pub l1_batch_hash: Hash32,
    pub l1_batch_index: u64,
    pub da_identifier: String,
    pub blob_id: String,
    pub prev_l1_batch_hash: Hash32,
}

/// The resolved identity of the L1 batch a proof or attestation is about,
/// copied out of the locally stored batch DA reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchReferenceSnapshot {
    pub reveal_txid: Txid,
    pub l1_batch_hash: Hash32,
    pub l1_batch_index: u64,
    pub prev_l1_batch_hash: Hash32,
    pub da_identifier: String,
    pub blob_id: String,
}

/// Sequencer commitment of one proof to the DA layer. Projection-closed:
/// carries the resolved snapshot of the batch it references.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofDAReferenceObserved {
    pub ordinal: EventOrdinal,
    pub subject_txid: Txid,
    pub da_identifier: String,
    pub blob_id: String,
    pub batch: BatchReferenceSnapshot,
}

/// A verifier's vote on a referenced proof transaction. `attester_script`
/// identifies which verifier wallet voted. Carries the resolved batch
/// identity (attestation to proof to batch) so adapters never reparse
/// history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatorAttestationObserved {
    pub ordinal: EventOrdinal,
    pub subject_txid: Txid,
    pub reference_txid: Txid,
    pub ok: bool,
    pub attester_script: ScriptBuf,
    pub batch: BatchReferenceSnapshot,
}

/// The genesis message: initial wallets, protocol version, and code hashes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemBootstrappingObserved {
    pub ordinal: EventOrdinal,
    pub subject_txid: Txid,
    pub start_block_height: u64,
    pub protocol_version: ProtocolVersionTag,
    pub bootloader_hash: Hash32,
    pub abstract_account_hash: Hash32,
    pub snark_wrapper_vk_hash: Hash32,
    pub evm_emulator_hash: Hash32,
    pub wallets: WalletSet,
}

/// The semantic content of a system-contract upgrade proposal.
/// `system_contracts` preserves inscription order; that order feeds
/// canonical upgrade-transaction construction and is identity-bearing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradePayload {
    pub version: ProtocolVersionTag,
    pub bootloader_code_hash: Hash32,
    pub default_account_code_hash: Hash32,
    pub evm_emulator_code_hash: Option<Hash32>,
    pub recursion_scheduler_level_vk_hash: Hash32,
    pub system_contracts: Vec<(Address20, Hash32)>,
}

/// A proposed system-contract upgrade (not yet activated).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemContractUpgradeProposalObserved {
    pub ordinal: EventOrdinal,
    pub subject_txid: Txid,
    pub proposal: UpgradePayload,
}

/// Governance activation of a previously proposed upgrade. Carries the
/// resolved proposal content so adapters write protocol versions from the
/// plan alone. Folding advances the context's protocol version to
/// `proposal.version`, which must exceed the current one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemContractUpgradeActivationObserved {
    pub ordinal: EventOrdinal,
    pub subject_txid: Txid,
    pub proposal_txid: Txid,
    pub proposal: UpgradePayload,
}

/// One L1 payout inside a bridge withdrawal transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawalOutput {
    /// First 8 raw bytes of the L2 withdrawal hash.
    pub l2_id: [u8; 8],
    /// Index of the L2 log where the withdrawal was executed.
    pub l2_tx_event_index: u16,
    pub receiver_script: ScriptBuf,
    pub amount: Amount,
}

/// A bridge withdrawal transaction: bridge inputs spent, L1 payouts made.
/// `inputs` preserves transaction input order; `withdrawals` preserves
/// output order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeWithdrawalObserved {
    pub ordinal: EventOrdinal,
    pub subject_txid: Txid,
    /// Stable tag of the withdrawal wire version the message used.
    pub withdrawal_version: u32,
    pub total_size: u64,
    pub v_size: u64,
    pub inputs: Vec<OutPoint>,
    pub output_amount: Amount,
    pub withdrawals: Vec<WithdrawalOutput>,
}

/// A proposed bridge rotation (new bridge wallet plus verifier set), not
/// yet activated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateBridgeProposalObserved {
    pub ordinal: EventOrdinal,
    pub subject_txid: Txid,
    pub bridge_script: ScriptBuf,
    pub verifier_scripts: Vec<ScriptBuf>,
}

/// Which system wallet a rotation replaces, with its new value. Bridge
/// rotations activate a prior proposal and carry the new verifier set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalletRotation {
    Sequencer { new_script: ScriptBuf },
    Governance { new_script: ScriptBuf },
    Bridge { proposal_txid: Txid, new_bridge_script: ScriptBuf, new_verifier_scripts: Vec<ScriptBuf> },
}

impl WalletRotation {
    pub fn wire_tag(&self) -> u32 {
        match self {
            WalletRotation::Sequencer { .. } => 0,
            WalletRotation::Governance { .. } => 1,
            WalletRotation::Bridge { .. } => 2,
        }
    }
}

/// An activated system-wallet rotation. The engine also folds it into the
/// plan's `next_context`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletRotationObserved {
    pub ordinal: EventOrdinal,
    pub subject_txid: Txid,
    pub rotation: WalletRotation,
}

/// Typed protocol events, ordered by [`EventOrdinal`]. The enum is
/// exhaustive on purpose: adding a variant breaks the build in every
/// adapter until that adapter decides how to handle it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolEvent {
    DepositObserved(DepositObserved),
    L1BatchDAReference(L1BatchDAReferenceObserved),
    ProofDAReference(ProofDAReferenceObserved),
    ValidatorAttestation(ValidatorAttestationObserved),
    SystemBootstrapping(SystemBootstrappingObserved),
    SystemContractUpgradeProposal(SystemContractUpgradeProposalObserved),
    SystemContractUpgradeActivation(SystemContractUpgradeActivationObserved),
    BridgeWithdrawal(BridgeWithdrawalObserved),
    UpdateBridgeProposal(UpdateBridgeProposalObserved),
    WalletRotation(WalletRotationObserved),
}

impl ProtocolEvent {
    pub fn ordinal(&self) -> EventOrdinal {
        match self {
            ProtocolEvent::DepositObserved(e) => e.ordinal,
            ProtocolEvent::L1BatchDAReference(e) => e.ordinal,
            ProtocolEvent::ProofDAReference(e) => e.ordinal,
            ProtocolEvent::ValidatorAttestation(e) => e.ordinal,
            ProtocolEvent::SystemBootstrapping(e) => e.ordinal,
            ProtocolEvent::SystemContractUpgradeProposal(e) => e.ordinal,
            ProtocolEvent::SystemContractUpgradeActivation(e) => e.ordinal,
            ProtocolEvent::BridgeWithdrawal(e) => e.ordinal,
            ProtocolEvent::UpdateBridgeProposal(e) => e.ordinal,
            ProtocolEvent::WalletRotation(e) => e.ordinal,
        }
    }

    pub fn kind(&self) -> EventKind {
        match self {
            ProtocolEvent::DepositObserved(_) => EventKind::Deposit,
            ProtocolEvent::L1BatchDAReference(_) => EventKind::L1BatchDAReference,
            ProtocolEvent::ProofDAReference(_) => EventKind::ProofDAReference,
            ProtocolEvent::ValidatorAttestation(_) => EventKind::ValidatorAttestation,
            ProtocolEvent::SystemBootstrapping(_) => EventKind::SystemBootstrapping,
            ProtocolEvent::SystemContractUpgradeProposal(_) => EventKind::SystemContractUpgradeProposal,
            ProtocolEvent::SystemContractUpgradeActivation(_) => EventKind::SystemContractUpgradeActivation,
            ProtocolEvent::BridgeWithdrawal(_) => EventKind::BridgeWithdrawal,
            ProtocolEvent::UpdateBridgeProposal(_) => EventKind::UpdateBridgeProposal,
            ProtocolEvent::WalletRotation(_) => EventKind::WalletRotation,
        }
    }

    /// The transaction carrying this event's message.
    pub fn subject_txid(&self) -> Txid {
        match self {
            ProtocolEvent::DepositObserved(e) => e.subject.txid,
            ProtocolEvent::L1BatchDAReference(e) => e.subject_txid,
            ProtocolEvent::ProofDAReference(e) => e.subject_txid,
            ProtocolEvent::ValidatorAttestation(e) => e.subject_txid,
            ProtocolEvent::SystemBootstrapping(e) => e.subject_txid,
            ProtocolEvent::SystemContractUpgradeProposal(e) => e.subject_txid,
            ProtocolEvent::SystemContractUpgradeActivation(e) => e.subject_txid,
            ProtocolEvent::BridgeWithdrawal(e) => e.subject_txid,
            ProtocolEvent::UpdateBridgeProposal(e) => e.subject_txid,
            ProtocolEvent::WalletRotation(e) => e.subject_txid,
        }
    }

    pub fn subject(&self) -> EffectSubject {
        match self {
            ProtocolEvent::DepositObserved(e) => EffectSubject::Output(e.subject),
            other => EffectSubject::Tx(other.subject_txid()),
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
    /// Recognized message whose signer is not the wallet the context
    /// authorizes for that message kind.
    Unauthorized,
    /// A bootstrap message while the context is already bootstrapped.
    InvalidBootstrap,
    /// An upgrade activation whose version does not increase the context's
    /// protocol version.
    NonMonotonicUpgrade,
    /// A recognized message whose referenced transaction is authoritatively
    /// absent or does not contain the required message. Applies to every
    /// reference-bearing event; unavailability halts instead.
    InvalidReference,
}

impl RejectionCode {
    pub fn wire_tag(self) -> u32 {
        match self {
            RejectionCode::MalformedMessage => 0,
            RejectionCode::ConflictingReceiverEncodings => 1,
            RejectionCode::ConflictingRoleUpdate => 2,
            RejectionCode::Unauthorized => 3,
            RejectionCode::InvalidBootstrap => 4,
            RejectionCode::NonMonotonicUpgrade => 5,
            RejectionCode::InvalidReference => 6,
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

/// Protocol state the engine needs to interpret a block: the system
/// wallets and protocol version in force. Each plan records the hash of
/// the context it was built from, and the checkpoint rejects a plan built
/// from any other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolContext {
    /// Context schema version (bump when this struct's shape changes).
    pub version: u32,
    pub wallets: WalletSet,
    pub protocol_version: ProtocolVersionTag,
}

const CONTEXT_WIRE_MAGIC: &[u8; 8] = b"VIA_CTX\0";

fn put_len_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u64).to_be_bytes());
    out.extend_from_slice(b);
}

fn put_wallet_set(out: &mut Vec<u8>, w: &WalletSet) {
    put_len_bytes(out, w.sequencer.as_bytes());
    put_len_bytes(out, w.bridge.as_bytes());
    put_len_bytes(out, w.governance.as_bytes());
    out.extend_from_slice(&(w.verifiers.len() as u64).to_be_bytes());
    for v in &w.verifiers {
        put_len_bytes(out, v.as_bytes());
    }
}

/// The normative context transition. Every engine derives `next_context`
/// with exactly this function.
///
/// `events` must be the plan's valid events in ordinal order. The whole
/// block is authorized against the input context; a rotation never
/// re-authorizes later transactions in its own block. The engine rejects,
/// before folding: a bootstrap when already bootstrapped, a second
/// rotation of the same role in one block, and a non-monotonic upgrade.
/// The fold applies the surviving transitions in order.
pub fn fold_context(input: &ProtocolContext, events: &[ProtocolEvent]) -> ProtocolContext {
    let mut ctx = input.clone();
    for e in events {
        match e {
            ProtocolEvent::SystemBootstrapping(b) => {
                ctx.wallets = b.wallets.clone();
                ctx.protocol_version = b.protocol_version;
            }
            ProtocolEvent::SystemContractUpgradeActivation(a) => {
                ctx.protocol_version = a.proposal.version;
            }
            ProtocolEvent::WalletRotation(r) => match &r.rotation {
                WalletRotation::Sequencer { new_script } => ctx.wallets.sequencer = new_script.clone(),
                WalletRotation::Governance { new_script } => ctx.wallets.governance = new_script.clone(),
                WalletRotation::Bridge { new_bridge_script, new_verifier_scripts, .. } => {
                    ctx.wallets.bridge = new_bridge_script.clone();
                    ctx.wallets.verifiers = new_verifier_scripts.clone();
                }
            },
            _ => {}
        }
    }
    ctx
}

impl ProtocolContext {
    /// Canonical fingerprint of this context (double-SHA256 over a
    /// length-delimited encoding with a domain prefix).
    pub fn context_hash(&self) -> Hash32 {
        let mut out = Vec::new();
        out.extend_from_slice(CONTEXT_WIRE_MAGIC);
        out.extend_from_slice(&self.version.to_be_bytes());
        put_wallet_set(&mut out, &self.wallets);
        out.extend_from_slice(&self.protocol_version.minor.to_be_bytes());
        out.extend_from_slice(&self.protocol_version.patch.to_be_bytes());
        sha256d::Hash::hash(&out).to_byte_array()
    }
}

/// Wire-format identity of the canonical plan encoding. Distinct from
/// [`KernelVersion`]: that versions interpretation rules, this versions the
/// byte grammar itself.
pub const PLAN_WIRE_MAGIC: &[u8; 12] = b"VIA_BLKPLAN\0";
pub const PLAN_WIRE_FORMAT_VERSION: u32 = 3;

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
/// byte strings length-prefixed with u64; top-level collections
/// count-prefixed with u64 and sorted by their canonical key; enum tags
/// are the explicit `wire_tag` constants. Nested vectors in event
/// payloads keep their source order (input, inscription, or output
/// order); that order carries meaning and is never sorted. Bitcoin
/// identities (txid, wtxid, block hash) are written in Bitcoin internal
/// byte order, not the reversed display order; protocol hashes (`Hash32`)
/// are opaque bytes written exactly as parsed, never reversed.
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
            let subject_txid = e.subject_txid();
            let tx_index = e.ordinal().tx_index;
            if !self.inclusions.iter().any(|i| i.txid == subject_txid && i.tx_index == tx_index) {
                return Err(PlanValidationError::EventWithoutInclusion { tx_index });
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
        let put_batch_snapshot = |out: &mut Vec<u8>, b: &BatchReferenceSnapshot| {
            out.extend_from_slice(b.reveal_txid.as_ref());
            out.extend_from_slice(&b.l1_batch_hash);
            out.extend_from_slice(&b.l1_batch_index.to_be_bytes());
            put_len_bytes(out, b.da_identifier.as_bytes());
            put_len_bytes(out, b.blob_id.as_bytes());
            out.extend_from_slice(&b.prev_l1_batch_hash);
        };
        let put_upgrade_payload = |out: &mut Vec<u8>, p: &UpgradePayload| {
            out.extend_from_slice(&p.version.minor.to_be_bytes());
            out.extend_from_slice(&p.version.patch.to_be_bytes());
            out.extend_from_slice(&p.bootloader_code_hash);
            out.extend_from_slice(&p.default_account_code_hash);
            match &p.evm_emulator_code_hash {
                Some(h) => {
                    out.extend_from_slice(&1u32.to_be_bytes());
                    out.extend_from_slice(h);
                }
                None => out.extend_from_slice(&0u32.to_be_bytes()),
            }
            out.extend_from_slice(&p.recursion_scheduler_level_vk_hash);
            out.extend_from_slice(&(p.system_contracts.len() as u64).to_be_bytes());
            for (addr, hash) in &p.system_contracts {
                out.extend_from_slice(addr);
                out.extend_from_slice(hash);
            }
        };

        put_u64(&mut out, self.events.len() as u64);
        for e in &self.events {
            put_u32(&mut out, e.kind().wire_tag());
            put_u32(&mut out, e.ordinal().tx_index);
            put_u32(&mut out, e.ordinal().location.wire_tag());
            put_u32(&mut out, e.ordinal().location.index());
            match e {
                ProtocolEvent::DepositObserved(d) => {
                    put_outpoint(&mut out, &d.subject);
                    put_u64(&mut out, d.amount.to_sat());
                    out.extend_from_slice(&d.receiver);
                    out.extend_from_slice(&d.l2_contract);
                    put_bytes(&mut out, &d.call_data);
                    match &d.sender_script {
                        Some(s) => {
                            put_u32(&mut out, 1);
                            put_bytes(&mut out, s.as_bytes());
                        }
                        None => put_u32(&mut out, 0),
                    }
                    put_u32(&mut out, d.encoding.wire_tag());
                }
                ProtocolEvent::L1BatchDAReference(v) => {
                    put_hash32(&mut out, v.subject_txid.as_ref());
                    put_hash32(&mut out, &v.l1_batch_hash);
                    put_u64(&mut out, v.l1_batch_index);
                    put_bytes(&mut out, v.da_identifier.as_bytes());
                    put_bytes(&mut out, v.blob_id.as_bytes());
                    put_hash32(&mut out, &v.prev_l1_batch_hash);
                }
                ProtocolEvent::ProofDAReference(v) => {
                    put_hash32(&mut out, v.subject_txid.as_ref());
                    put_bytes(&mut out, v.da_identifier.as_bytes());
                    put_bytes(&mut out, v.blob_id.as_bytes());
                    put_batch_snapshot(&mut out, &v.batch);
                }
                ProtocolEvent::ValidatorAttestation(v) => {
                    put_hash32(&mut out, v.subject_txid.as_ref());
                    put_hash32(&mut out, v.reference_txid.as_ref());
                    put_u32(&mut out, u32::from(v.ok));
                    put_bytes(&mut out, v.attester_script.as_bytes());
                    put_batch_snapshot(&mut out, &v.batch);
                }
                ProtocolEvent::SystemBootstrapping(v) => {
                    put_hash32(&mut out, v.subject_txid.as_ref());
                    put_u64(&mut out, v.start_block_height);
                    put_u32(&mut out, v.protocol_version.minor);
                    put_u32(&mut out, v.protocol_version.patch);
                    put_hash32(&mut out, &v.bootloader_hash);
                    put_hash32(&mut out, &v.abstract_account_hash);
                    put_hash32(&mut out, &v.snark_wrapper_vk_hash);
                    put_hash32(&mut out, &v.evm_emulator_hash);
                    put_wallet_set(&mut out, &v.wallets);
                }
                ProtocolEvent::SystemContractUpgradeProposal(v) => {
                    put_hash32(&mut out, v.subject_txid.as_ref());
                    put_upgrade_payload(&mut out, &v.proposal);
                }
                ProtocolEvent::SystemContractUpgradeActivation(v) => {
                    put_hash32(&mut out, v.subject_txid.as_ref());
                    put_hash32(&mut out, v.proposal_txid.as_ref());
                    put_upgrade_payload(&mut out, &v.proposal);
                }
                ProtocolEvent::BridgeWithdrawal(v) => {
                    put_hash32(&mut out, v.subject_txid.as_ref());
                    put_u32(&mut out, v.withdrawal_version);
                    put_u64(&mut out, v.total_size);
                    put_u64(&mut out, v.v_size);
                    put_u64(&mut out, v.inputs.len() as u64);
                    for op in &v.inputs {
                        put_outpoint(&mut out, op);
                    }
                    put_u64(&mut out, v.output_amount.to_sat());
                    put_u64(&mut out, v.withdrawals.len() as u64);
                    for w in &v.withdrawals {
                        out.extend_from_slice(&w.l2_id);
                        put_u32(&mut out, u32::from(w.l2_tx_event_index));
                        put_bytes(&mut out, w.receiver_script.as_bytes());
                        put_u64(&mut out, w.amount.to_sat());
                    }
                }
                ProtocolEvent::UpdateBridgeProposal(v) => {
                    put_hash32(&mut out, v.subject_txid.as_ref());
                    put_bytes(&mut out, v.bridge_script.as_bytes());
                    put_u64(&mut out, v.verifier_scripts.len() as u64);
                    for s in &v.verifier_scripts {
                        put_bytes(&mut out, s.as_bytes());
                    }
                }
                ProtocolEvent::WalletRotation(v) => {
                    put_hash32(&mut out, v.subject_txid.as_ref());
                    put_u32(&mut out, v.rotation.wire_tag());
                    match &v.rotation {
                        WalletRotation::Sequencer { new_script } | WalletRotation::Governance { new_script } => {
                            put_bytes(&mut out, new_script.as_bytes());
                        }
                        WalletRotation::Bridge { proposal_txid, new_bridge_script, new_verifier_scripts } => {
                            put_hash32(&mut out, proposal_txid.as_ref());
                            put_bytes(&mut out, new_bridge_script.as_bytes());
                            put_u64(&mut out, new_verifier_scripts.len() as u64);
                            for s in new_verifier_scripts {
                                put_bytes(&mut out, s.as_bytes());
                            }
                        }
                    }
                }
            }
        }
        put_u64(&mut out, self.dispositions.len() as u64);
        for d in &self.dispositions {
            put_u32(&mut out, d.ordinal.tx_index);
            put_u32(&mut out, d.ordinal.location.wire_tag());
            put_u32(&mut out, d.ordinal.location.index());
            match &d.kind {
                DispositionKind::RejectedInvalid { code, detail: _ } => {
                    put_u32(&mut out, 0);
                    put_u32(&mut out, code.wire_tag());
                }
                DispositionKind::Duplicate => put_u32(&mut out, 1),
            }
        }
        put_u32(&mut out, self.next_context.version);
        put_wallet_set(&mut out, &self.next_context.wallets);
        put_u32(&mut out, self.next_context.protocol_version.minor);
        put_u32(&mut out, self.next_context.protocol_version.patch);
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
/// The constructor enforces the binding, so a mismatched pair cannot exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalObservedTx {
    inclusion: Inclusion,
    variant: RawTxVariant,
}

impl CanonicalObservedTx {
    pub fn new(inclusion: Inclusion, variant: RawTxVariant) -> Result<Self, String> {
        if inclusion.txid != variant.txid() || inclusion.wtxid != variant.wtxid() {
            return Err("inclusion and raw variant identify different transactions".into());
        }
        Ok(Self { inclusion, variant })
    }

    pub fn inclusion(&self) -> &Inclusion {
        &self.inclusion
    }
    pub fn variant(&self) -> &RawTxVariant {
        &self.variant
    }
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

/// Check that every resolved value actually answers its map key: a raw
/// transaction resolution must carry that key's txid, a tracked-output
/// resolution that key's outpoint. Engines call this before finalizing so
/// a miswired resolver surfaces as a typed failure, not a wrong plan.
pub fn validate_resolutions(deps: &BTreeMap<DependencyKey, ResolvedDependency>) -> Result<(), FinalizeFailure> {
    for (key, value) in deps {
        let ok = match (key, value) {
            (DependencyKey::RawTx(txid), ResolvedDependency::RawTx(r)) => match r {
                Resolution::Present(obs) => obs.variant().txid() == *txid,
                _ => true,
            },
            (DependencyKey::TrackedOutput(op), ResolvedDependency::TrackedOutput(r)) => match r {
                Resolution::Present(create) => create.outpoint == *op,
                _ => true,
            },
            _ => false,
        };
        if !ok {
            return Err(FinalizeFailure::CorruptDependency(key.clone()));
        }
    }
    Ok(())
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

/// Result of one finalization round. Some dependencies are only
/// discoverable after resolving earlier ones (an attestation names a proof
/// transaction, whose content names the batch transaction), so
/// finalization iterates until closure.
pub enum FinalizeOutcome<D> {
    Complete(Box<BlockPlan>),
    /// More dependencies are needed. `keys` is sorted, deduplicated, and
    /// only holds keys missing from the resolved map. The caller resolves
    /// them, merges, and calls `finalize` again with this draft. Discovery
    /// is deterministic and currently at most two rounds deep; a runner
    /// may cap rounds and fail an overflow as infrastructure.
    NeedDependencies {
        draft: D,
        keys: Vec<DependencyKey>,
    },
}

/// The deterministic protocol engine: two stages, no I/O in either.
pub trait ProtocolEngine {
    type Draft;

    /// Classify the envelope against the context and list the dependencies
    /// finalization needs. Authorization is judged before reference
    /// resolution: an unauthorized message rejects without requiring its
    /// references, so garbage can never stall ingestion on missing data. Discovery is conservative on purpose: the engine
    /// lists a `TrackedOutput` dependency for every input spending an
    /// outpoint not created in this envelope, and the store answers each
    /// with `Present` or `KnownAbsent` in one indexed lookup. The
    /// alternative, letting `inspect` read local state, would make it
    /// impure.
    fn inspect(&self, envelope: &BitcoinBlockEnvelope, context: &ProtocolContext) -> (Self::Draft, Vec<DependencyKey>);

    /// Pure finalization against the dependencies resolved so far.
    /// Implementations must check the map with [`validate_resolutions`]
    /// and must derive the plan's `next_context` with [`fold_context`].
    fn finalize(
        &self, draft: Self::Draft, dependencies: &BTreeMap<DependencyKey, ResolvedDependency>,
    ) -> Result<FinalizeOutcome<Self::Draft>, FinalizeFailure>;
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

// Test assertions use panicking conveniences and fixture-bounded indices.
#[allow(clippy::cast_possible_truncation, clippy::expect_used, clippy::unwrap_used)]
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
        ProtocolContext {
            version: 1,
            wallets: WalletSet {
                sequencer: ScriptBuf::new(),
                bridge: ScriptBuf::new(),
                governance: ScriptBuf::new(),
                verifiers: vec![],
            },
            protocol_version: ProtocolVersionTag { minor: 26, patch: 0 },
        }
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
            hex, "429456345217e6e8a19c59116e987fcd993e1ef6ad9e79a7f2d50df896769958",
            "canonical wire format changed: if intentional, bump PLAN_WIRE_FORMAT_VERSION and refreeze"
        );
    }

    fn tx_n(n: u32) -> Transaction {
        Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::from_consensus(n),
            input: vec![],
            output: vec![],
        }
    }

    fn script_n(n: u8) -> ScriptBuf {
        ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::from_byte_array([n; 20]))
    }

    /// One plan carrying every event kind, both `Option` arms, and all
    /// wallet-rotation tags, so no v3 encoding path is left unfrozen.
    fn exhaustive_plan() -> BlockPlan {
        let txs: Vec<Transaction> = (0..10).map(tx_n).collect();
        let mut variants: Vec<RawTxVariant> = txs.iter().map(RawTxVariant::from_transaction).collect();
        let anchor = anchor();
        let inclusions: Vec<Inclusion> = variants
            .iter()
            .enumerate()
            .map(|(i, v)| Inclusion {
                block_hash: anchor.hash,
                height: anchor.height,
                tx_index: i as u32,
                txid: v.txid(),
                wtxid: v.wtxid(),
            })
            .collect();
        let txids: Vec<Txid> = inclusions.iter().map(|i| i.txid).collect();
        let txid = |i: usize| txids[i];
        let ord = |i: usize| EventOrdinal { tx_index: i as u32, location: MessageLocation::Output(0) };
        let batch = BatchReferenceSnapshot {
            reveal_txid: txid(1),
            l1_batch_hash: [3; 32],
            l1_batch_index: 7,
            prev_l1_batch_hash: [2; 32],
            da_identifier: "celestia".into(),
            blob_id: "blob-1".into(),
        };
        let upgrade = UpgradePayload {
            version: ProtocolVersionTag { minor: 27, patch: 0 },
            bootloader_code_hash: [4; 32],
            default_account_code_hash: [5; 32],
            evm_emulator_code_hash: Some([6; 32]),
            recursion_scheduler_level_vk_hash: [7; 32],
            system_contracts: vec![([8; 20], [9; 32]), ([10; 20], [11; 32])],
        };
        let upgrade_no_emu = UpgradePayload { evm_emulator_code_hash: None, ..upgrade.clone() };
        let wallets = WalletSet {
            sequencer: script_n(0x11),
            bridge: script_n(0x12),
            governance: script_n(0x13),
            verifiers: vec![script_n(0x14), script_n(0x15)],
        };
        let events = vec![
            ProtocolEvent::DepositObserved(DepositObserved {
                ordinal: ord(0),
                subject: OutPoint { txid: txid(0), vout: 0 },
                amount: Amount::from_sat(50_000),
                receiver: [0xAA; 20],
                l2_contract: [0; 20],
                call_data: vec![1, 2, 3],
                sender_script: Some(script_n(0x16)),
                encoding: DepositEncoding::Both,
            }),
            ProtocolEvent::L1BatchDAReference(L1BatchDAReferenceObserved {
                ordinal: ord(1),
                subject_txid: txid(1),
                l1_batch_hash: [3; 32],
                l1_batch_index: 7,
                da_identifier: "celestia".into(),
                blob_id: "blob-1".into(),
                prev_l1_batch_hash: [2; 32],
            }),
            ProtocolEvent::ProofDAReference(ProofDAReferenceObserved {
                ordinal: ord(2),
                subject_txid: txid(2),
                da_identifier: "celestia".into(),
                blob_id: "blob-2".into(),
                batch: batch.clone(),
            }),
            ProtocolEvent::ValidatorAttestation(ValidatorAttestationObserved {
                ordinal: ord(3),
                subject_txid: txid(3),
                reference_txid: txid(2),
                ok: true,
                attester_script: script_n(0x14),
                batch,
            }),
            ProtocolEvent::SystemBootstrapping(SystemBootstrappingObserved {
                ordinal: ord(4),
                subject_txid: txid(4),
                start_block_height: 100,
                protocol_version: ProtocolVersionTag { minor: 26, patch: 0 },
                bootloader_hash: [12; 32],
                abstract_account_hash: [13; 32],
                snark_wrapper_vk_hash: [14; 32],
                evm_emulator_hash: [15; 32],
                wallets: wallets.clone(),
            }),
            ProtocolEvent::SystemContractUpgradeProposal(SystemContractUpgradeProposalObserved {
                ordinal: ord(5),
                subject_txid: txid(5),
                proposal: upgrade_no_emu,
            }),
            ProtocolEvent::SystemContractUpgradeActivation(SystemContractUpgradeActivationObserved {
                ordinal: ord(6),
                subject_txid: txid(6),
                proposal_txid: txid(5),
                proposal: upgrade,
            }),
            ProtocolEvent::BridgeWithdrawal(BridgeWithdrawalObserved {
                ordinal: ord(7),
                subject_txid: txid(7),
                withdrawal_version: 1,
                total_size: 400,
                v_size: 200,
                inputs: vec![OutPoint { txid: txid(0), vout: 0 }],
                output_amount: Amount::from_sat(49_000),
                withdrawals: vec![WithdrawalOutput {
                    l2_id: [1, 2, 3, 4, 5, 6, 7, 8],
                    l2_tx_event_index: 3,
                    receiver_script: script_n(0x17),
                    amount: Amount::from_sat(49_000),
                }],
            }),
            ProtocolEvent::UpdateBridgeProposal(UpdateBridgeProposalObserved {
                ordinal: ord(8),
                subject_txid: txid(8),
                bridge_script: script_n(0x18),
                verifier_scripts: vec![script_n(0x19)],
            }),
            ProtocolEvent::WalletRotation(WalletRotationObserved {
                ordinal: ord(9),
                subject_txid: txid(9),
                rotation: WalletRotation::Bridge {
                    proposal_txid: txid(8),
                    new_bridge_script: script_n(0x18),
                    new_verifier_scripts: vec![script_n(0x19)],
                },
            }),
        ];
        let input_context =
            ProtocolContext { version: 1, wallets, protocol_version: ProtocolVersionTag { minor: 26, patch: 0 } };
        let next_context = fold_context(&input_context, &events);
        variants.sort_by_key(|v| (v.txid(), v.wtxid()));
        BlockPlan {
            kernel_version: KernelVersion(1),
            observation_rule_version: ObservationRuleVersion(1),
            network: Network::Regtest,
            anchor,
            input_context_hash: input_context.context_hash(),
            raw_variants: variants,
            inclusions,
            tracked_creates: vec![TrackedOutputCreate {
                outpoint: OutPoint { txid: txid(0), vout: 0 },
                value: Amount::from_sat(50_000),
                script_pubkey: script_n(0x12),
                role: TrackedRole::Bridge,
            }],
            tracked_spends: vec![],
            events,
            dispositions: vec![
                Disposition {
                    ordinal: EventOrdinal { tx_index: 0, location: MessageLocation::Input(0) },
                    kind: DispositionKind::RejectedInvalid {
                        code: RejectionCode::Unauthorized,
                        detail: "excluded from hash".into(),
                    },
                },
                Disposition {
                    ordinal: EventOrdinal { tx_index: 1, location: MessageLocation::Output(1) },
                    kind: DispositionKind::Duplicate,
                },
            ],
            next_context,
        }
    }

    /// Frozen golden over the exhaustive plan: freezes every v3 event
    /// encoding, not just the empty-plan header.
    #[test]
    fn exhaustive_plan_hash_matches_frozen_golden() {
        use std::fmt::Write;
        let plan = exhaustive_plan();
        plan.validate().expect("exhaustive plan must be structurally valid");
        let hash = plan.plan_hash().unwrap();
        let hex = hash.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
        assert_eq!(
            hex, "811e334ba720e566e3b078f603124f57884b90031bef05cdbac6b14d899602cd",
            "canonical wire format changed: if intentional, bump PLAN_WIRE_FORMAT_VERSION and refreeze"
        );
    }

    #[test]
    fn fold_context_applies_bootstrap_rotation_and_upgrade() {
        let plan = exhaustive_plan();
        let next = &plan.next_context;
        assert_eq!(next.protocol_version, ProtocolVersionTag { minor: 27, patch: 0 });
        assert_eq!(next.wallets.bridge, script_n(0x18), "bridge rotation folded");
        assert_eq!(next.wallets.verifiers, vec![script_n(0x19)], "verifier set folded");
        assert_eq!(next.wallets.sequencer, script_n(0x11), "sequencer untouched");
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
                ordinal: EventOrdinal { tx_index: 0, location: MessageLocation::Output(0) },
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
            Disposition {
                ordinal: EventOrdinal { tx_index: 1, location: MessageLocation::Output(0) },
                kind: DispositionKind::Duplicate,
            },
            Disposition {
                ordinal: EventOrdinal { tx_index: 0, location: MessageLocation::Output(0) },
                kind: DispositionKind::Duplicate,
            },
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
    fn inclusion_must_match_a_stored_variant_on_both_identities() {
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
            block_hash: p.anchor.hash,
            height: p.anchor.height,
            tx_index: 0,
            txid: variant.txid(),
            // A wtxid that was never stored: matching the txid alone must
            // not be enough, or witness variants could alias each other.
            wtxid: Wtxid::from_byte_array([0xEE; 32]),
        }];
        assert!(matches!(p.validate(), Err(PlanValidationError::InclusionWithoutVariant { .. })));
    }

    #[test]
    fn events_must_reference_an_included_transaction() {
        let mut p = dummy_plan();
        p.events = vec![ProtocolEvent::DepositObserved(DepositObserved {
            ordinal: EventOrdinal { tx_index: 0, location: MessageLocation::Output(0) },
            subject: OutPoint { txid: Txid::all_zeros(), vout: 0 },
            amount: Amount::from_sat(1),
            receiver: [0; 20],
            l2_contract: [0; 20],
            call_data: vec![],
            sender_script: None,
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
                ordinal: EventOrdinal { tx_index: 0, location: MessageLocation::Output(0) },
                kind: EventKind::Deposit,
                subject: EffectSubject::Output(OutPoint { txid: Txid::all_zeros(), vout: 0 }),
            }],
            ..ok
        };
        assert!(phantom.validate(&plan).is_err());
    }
}
