//! Production implementation of the ingestion kernel's [`ProtocolEngine`].
//!
//! Turns one Bitcoin block envelope plus protocol context into a
//! deterministic [`BlockPlan`], with no I/O: every historical fact arrives
//! as a resolved dependency. Message decoding is delegated to
//! [`PositionedMessageParser`], so protocol payload logic exists once.

use std::collections::BTreeMap;

use bitcoin::{
    address::NetworkUnchecked, Address, Amount, Network, OutPoint, ScriptBuf, Transaction, Txid,
};
use via_btc_ingestion::{
    fold_context, validate_resolutions, BatchReferenceSnapshot, BitcoinBlockEnvelope, BlockPlan,
    BridgeWithdrawalObserved, CanonicalObservedTx, DependencyKey, DepositEncoding, DepositObserved,
    Disposition, DispositionKind, EventOrdinal, FinalizeFailure, FinalizeOutcome, Inclusion,
    KernelVersion, L1BatchDAReferenceObserved, MessageLocation, ObservationRuleVersion,
    ProofDAReferenceObserved, ProtocolContext, ProtocolEngine, ProtocolEvent, ProtocolVersionTag,
    RawTxVariant, RejectionCode, Resolution, ResolvedDependency, SystemBootstrappingObserved,
    SystemContractUpgradeActivationObserved, SystemContractUpgradeProposalObserved,
    TrackedOutputCreate, TrackedOutputSpend, TrackedRole, UpdateBridgeProposalObserved,
    UpgradePayload, ValidatorAttestationObserved, WalletRotation, WalletRotationObserved,
    WalletSet, WithdrawalOutput,
};
use zksync_types::via_wallet::SystemWallets;

use crate::{
    indexer::positioned::{PositionedMessage, PositionedMessageParser, PositionedParseOutcome},
    types::{FullInscriptionMessage, Vote},
};

/// Interpretation rules of this engine build. Bump when the meaning of any
/// event or disposition changes.
pub const ENGINE_KERNEL_VERSION: KernelVersion = KernelVersion(1);
/// Retention rule: keep every transaction with a recognized carrier, every
/// transaction paying a tracked role script, and every spender of a tracked
/// output.
pub const ENGINE_OBSERVATION_RULE_VERSION: ObservationRuleVersion = ObservationRuleVersion(1);

/// The production protocol engine. Holds no client, storage, or clock.
#[derive(Clone, Debug)]
pub struct ViaProtocolEngine {
    network: Network,
}

impl ViaProtocolEngine {
    pub fn new(network: Network) -> Self {
        Self { network }
    }

    fn wallets_of(&self, context: &ProtocolContext) -> Option<SystemWallets> {
        if !context.wallets.is_bootstrapped() {
            return None;
        }
        let addr = |script: &ScriptBuf| Address::from_script(script, self.network).ok();
        Some(SystemWallets {
            sequencer: addr(&context.wallets.sequencer)?,
            bridge: addr(&context.wallets.bridge)?,
            governance: addr(&context.wallets.governance)?,
            verifiers: context
                .wallets
                .verifiers
                .iter()
                .map(addr)
                .collect::<Option<Vec<_>>>()?,
        })
    }
}

/// Everything `inspect` learned about one transaction.
#[derive(Clone, Debug)]
struct TxDraft {
    tx_index: u32,
    txid: Txid,
    variant: RawTxVariant,
    outcomes: Vec<PositionedParseOutcome>,
    /// (vout, value, script) of every output.
    outputs: Vec<(u32, Amount, ScriptBuf)>,
    /// (input index, spent outpoint) of every input.
    inputs: Vec<(u32, OutPoint)>,
}

/// Pure state carried between `inspect` and `finalize` rounds.
#[derive(Clone, Debug)]
pub struct EngineDraft {
    anchor: via_btc_ingestion::BlockAnchor,
    network: Network,
    context: ProtocolContext,
    txs: Vec<TxDraft>,
}

impl ProtocolEngine for ViaProtocolEngine {
    type Draft = EngineDraft;

    fn inspect(
        &self,
        envelope: &BitcoinBlockEnvelope,
        context: &ProtocolContext,
    ) -> (EngineDraft, Vec<DependencyKey>) {
        let wallets = self.wallets_of(context);
        let mut parser = PositionedMessageParser::new(self.network);
        let mut txs = Vec::with_capacity(envelope.transactions.len());
        let mut keys = Vec::new();
        let mut in_envelope: std::collections::BTreeSet<Txid> = std::collections::BTreeSet::new();

        for tx in &envelope.transactions {
            in_envelope.insert(tx.compute_txid());
        }
        for (tx_index, tx) in envelope.transactions.iter().enumerate() {
            let outcomes = parser.parse_transaction(
                tx,
                tx_index as u32,
                envelope.anchor.height.min(u64::from(u32::MAX)) as u32,
                wallets.as_ref(),
            );
            let draft = TxDraft {
                tx_index: tx_index as u32,
                txid: tx.compute_txid(),
                variant: RawTxVariant::from_transaction(tx),
                outputs: tx
                    .output
                    .iter()
                    .enumerate()
                    .map(|(i, o)| (i as u32, o.value, o.script_pubkey.clone()))
                    .collect(),
                inputs: tx
                    .input
                    .iter()
                    .enumerate()
                    .map(|(i, inp)| (i as u32, inp.previous_output))
                    .collect(),
                outcomes,
            };
            // Conservative discovery: whether an input spends a tracked
            // output is store knowledge, so ask about every external input.
            for (_, outpoint) in &draft.inputs {
                if !in_envelope.contains(&outpoint.txid) {
                    keys.push(DependencyKey::TrackedOutput(*outpoint));
                }
            }
            // Historical transactions named inside messages.
            for outcome in &draft.outcomes {
                if let PositionedParseOutcome::Valid(msg) = outcome {
                    for txid in referenced_txids(&msg.message) {
                        if !in_envelope.contains(&txid) {
                            keys.push(DependencyKey::RawTx(txid));
                        }
                    }
                }
            }
            txs.push(draft);
        }
        keys.sort();
        keys.dedup();
        (
            EngineDraft {
                anchor: envelope.anchor,
                network: envelope.network,
                context: context.clone(),
                txs,
            },
            keys,
        )
    }

    fn finalize(
        &self,
        draft: EngineDraft,
        deps: &BTreeMap<DependencyKey, ResolvedDependency>,
    ) -> Result<FinalizeOutcome<EngineDraft>, FinalizeFailure> {
        validate_resolutions(deps)?;
        Finalizer::new(self, draft, deps).run()
    }
}

/// Transaction ids a message references and finalization must look up.
fn referenced_txids(message: &FullInscriptionMessage) -> Vec<Txid> {
    match message {
        FullInscriptionMessage::ValidatorAttestation(m) => vec![m.input.reference_txid],
        FullInscriptionMessage::SystemContractUpgrade(m) => vec![m.input.proposal_tx_id],
        FullInscriptionMessage::UpdateBridge(m) => vec![m.input.proposal_tx_id],
        _ => vec![],
    }
}

struct Finalizer<'a> {
    engine: &'a ViaProtocolEngine,
    draft: EngineDraft,
    deps: &'a BTreeMap<DependencyKey, ResolvedDependency>,
    missing: Vec<DependencyKey>,
    second_round: Vec<DependencyKey>,
    events: Vec<ProtocolEvent>,
    dispositions: Vec<Disposition>,
    tracked_creates: Vec<TrackedOutputCreate>,
    tracked_spends: Vec<TrackedOutputSpend>,
    /// Which roles rotated in this block already (bootstrap counts for all).
    rotated: std::collections::BTreeSet<&'static str>,
    important: std::collections::BTreeSet<u32>,
}

impl<'a> Finalizer<'a> {
    fn new(
        engine: &'a ViaProtocolEngine,
        draft: EngineDraft,
        deps: &'a BTreeMap<DependencyKey, ResolvedDependency>,
    ) -> Self {
        Self {
            engine,
            draft,
            deps,
            missing: vec![],
            second_round: vec![],
            events: vec![],
            dispositions: vec![],
            tracked_creates: vec![],
            tracked_spends: vec![],
            rotated: Default::default(),
            important: Default::default(),
        }
    }

    fn reject(&mut self, ordinal: EventOrdinal, code: RejectionCode, detail: impl Into<String>) {
        self.important.insert(ordinal.tx_index);
        self.dispositions.push(Disposition {
            ordinal,
            kind: DispositionKind::RejectedInvalid {
                code,
                detail: detail.into(),
            },
        });
    }

    fn raw_tx(&mut self, txid: Txid) -> Option<Option<&'a CanonicalObservedTx>> {
        // In-envelope references never need the store.
        match self.deps.get(&DependencyKey::RawTx(txid)) {
            Some(ResolvedDependency::RawTx(Resolution::Present(obs))) => Some(Some(obs)),
            Some(ResolvedDependency::RawTx(Resolution::KnownAbsent)) => Some(None),
            Some(ResolvedDependency::RawTx(Resolution::Unavailable)) => {
                self.missing.push(DependencyKey::RawTx(txid));
                None
            }
            Some(_) => {
                self.missing.push(DependencyKey::RawTx(txid));
                None
            }
            None => {
                self.second_round.push(DependencyKey::RawTx(txid));
                None
            }
        }
    }

    fn tracked_role_of(&mut self, outpoint: OutPoint) -> Option<Option<TrackedRole>> {
        // A same-block create resolves in-envelope.
        for tx in &self.draft.txs {
            if tx.txid == outpoint.txid {
                return Some(self.in_envelope_role(outpoint));
            }
        }
        match self.deps.get(&DependencyKey::TrackedOutput(outpoint)) {
            Some(ResolvedDependency::TrackedOutput(Resolution::Present(create))) => {
                Some(Some(create.role))
            }
            Some(ResolvedDependency::TrackedOutput(Resolution::KnownAbsent)) => Some(None),
            _ => {
                self.missing.push(DependencyKey::TrackedOutput(outpoint));
                None
            }
        }
    }

    fn in_envelope_role(&self, outpoint: OutPoint) -> Option<TrackedRole> {
        let ctx = &self.draft.context;
        self.draft
            .txs
            .iter()
            .find(|t| t.txid == outpoint.txid)
            .and_then(|t| {
                t.outputs
                    .iter()
                    .find(|(v, _, _)| *v == outpoint.vout)
                    .and_then(|(_, _, script)| {
                        if *script == ctx.wallets.bridge {
                            Some(TrackedRole::Bridge)
                        } else if *script == ctx.wallets.governance {
                            Some(TrackedRole::Governance)
                        } else {
                            None
                        }
                    })
            })
    }

    /// Parse a historically observed transaction and return its messages.
    /// A malformed or unsupported carrier inside a referenced historical
    /// transaction is dropped here on purpose: the referring event then
    /// rejects as InvalidReference. The historical transaction's own
    /// taxonomy was already recorded when its block was processed.
    fn parse_observed(
        &self,
        obs: &CanonicalObservedTx,
    ) -> Result<Vec<PositionedMessage>, FinalizeFailure> {
        use bitcoin::consensus::Decodable;
        let tx = Transaction::consensus_decode(&mut obs.variant().raw()).map_err(|_| {
            FinalizeFailure::CorruptDependency(DependencyKey::RawTx(obs.variant().txid()))
        })?;
        let wallets = self.engine.wallets_of(&self.draft.context);
        let mut parser = PositionedMessageParser::new(self.engine.network);
        let outcomes = parser.parse_transaction(
            &tx,
            obs.inclusion().tx_index,
            obs.inclusion().height as u32,
            wallets.as_ref(),
        );
        Ok(outcomes
            .into_iter()
            .filter_map(|o| match o {
                PositionedParseOutcome::Valid(m) => Some(*m),
                _ => None,
            })
            .collect())
    }

    /// Messages of an in-envelope transaction, for same-block references.
    fn in_envelope_messages(&self, txid: Txid) -> Option<Vec<PositionedMessage>> {
        self.draft.txs.iter().find(|t| t.txid == txid).map(|t| {
            t.outcomes
                .iter()
                .filter_map(|o| match o {
                    PositionedParseOutcome::Valid(m) => Some((**m).clone()),
                    _ => None,
                })
                .collect()
        })
    }

    /// Resolve a referenced txid to its messages: in-envelope first, then
    /// the store. `Ok(None)` means known-absent (a definite verdict).
    fn messages_of(
        &mut self,
        txid: Txid,
    ) -> Result<Option<Option<Vec<PositionedMessage>>>, FinalizeFailure> {
        if let Some(msgs) = self.in_envelope_messages(txid) {
            return Ok(Some(Some(msgs)));
        }
        match self.raw_tx(txid) {
            Some(Some(obs)) => {
                let obs = obs.clone();
                Ok(Some(Some(self.parse_observed(&obs)?)))
            }
            Some(None) => Ok(Some(None)),
            None => Ok(None),
        }
    }

    fn batch_snapshot_of_proof(
        &mut self,
        proof: &PositionedMessage,
    ) -> Result<Option<Option<BatchReferenceSnapshot>>, FinalizeFailure> {
        let FullInscriptionMessage::ProofDAReference(p) = &proof.message else {
            return Ok(Some(None));
        };
        let reveal = p.input.l1_batch_reveal_txid;
        let Some(resolved) = self.messages_of(reveal)? else {
            return Ok(None);
        };
        let Some(msgs) = resolved else {
            return Ok(Some(None));
        };
        for m in msgs {
            if let FullInscriptionMessage::L1BatchDAReference(b) = m.message {
                return Ok(Some(Some(BatchReferenceSnapshot {
                    reveal_txid: reveal,
                    l1_batch_hash: b.input.l1_batch_hash.to_fixed_bytes(),
                    l1_batch_index: b.input.l1_batch_index.0 as u64,
                    prev_l1_batch_hash: b.input.prev_l1_batch_hash.to_fixed_bytes(),
                    da_identifier: b.input.da_identifier,
                    blob_id: b.input.blob_id,
                })));
            }
        }
        Ok(Some(None))
    }

    fn run(mut self) -> Result<FinalizeOutcome<EngineDraft>, FinalizeFailure> {
        if self.draft.network != self.engine.network {
            return Err(FinalizeFailure::Infrastructure(format!(
                "envelope network {:?} does not match engine network {:?}",
                self.draft.network, self.engine.network
            )));
        }
        if u32::try_from(self.draft.anchor.height).is_err() {
            return Err(FinalizeFailure::Infrastructure(
                "block height exceeds u32".into(),
            ));
        }
        let context = self.draft.context.clone();
        let bootstrapped = context.wallets.is_bootstrapped();
        let txs = self.draft.txs.clone();

        for tx in &txs {
            // Tracked output creation is context-driven, not message-driven.
            for (vout, value, script) in &tx.outputs {
                let role = if *script == context.wallets.bridge && bootstrapped {
                    Some(TrackedRole::Bridge)
                } else if *script == context.wallets.governance && bootstrapped {
                    Some(TrackedRole::Governance)
                } else {
                    None
                };
                if let Some(role) = role {
                    self.important.insert(tx.tx_index);
                    self.tracked_creates.push(TrackedOutputCreate {
                        outpoint: OutPoint {
                            txid: tx.txid,
                            vout: *vout,
                        },
                        value: *value,
                        script_pubkey: script.clone(),
                        role,
                    });
                }
            }
            for (input_index, outpoint) in &tx.inputs {
                if let Some(Some(_role)) = self.tracked_role_of(*outpoint) {
                    self.important.insert(tx.tx_index);
                    self.tracked_spends.push(TrackedOutputSpend {
                        outpoint: *outpoint,
                        spending_txid: tx.txid,
                        spending_wtxid: tx.variant.wtxid(),
                        input_index: *input_index,
                    });
                }
            }

            self.process_tx(tx, &context, bootstrapped)?;
        }

        if !self.missing.is_empty() {
            self.missing.sort();
            self.missing.dedup();
            return Err(FinalizeFailure::MissingDependency(std::mem::take(
                &mut self.missing,
            )));
        }
        if !self.second_round.is_empty() {
            self.second_round.sort();
            self.second_round.dedup();
            let keys = std::mem::take(&mut self.second_round);
            return Ok(FinalizeOutcome::NeedDependencies {
                draft: self.draft,
                keys,
            });
        }

        self.events
            .sort_by_key(|e| (e.ordinal(), e.kind(), e.subject()));
        self.dispositions.sort_by_key(|d| d.ordinal);
        self.tracked_creates.sort_by_key(|c| c.outpoint);
        self.tracked_spends.sort_by_key(|s| s.outpoint);
        let next_context = fold_context(&context, &self.events);

        let mut raw_variants = Vec::new();
        let mut inclusions = Vec::new();
        for tx in &txs {
            if self.important.contains(&tx.tx_index) {
                raw_variants.push(tx.variant.clone());
                inclusions.push(Inclusion {
                    block_hash: self.draft.anchor.hash,
                    height: self.draft.anchor.height,
                    tx_index: tx.tx_index,
                    txid: tx.variant.txid(),
                    wtxid: tx.variant.wtxid(),
                });
            }
        }
        raw_variants.sort_by_key(|v| (v.txid(), v.wtxid()));
        raw_variants.dedup_by_key(|v| (v.txid(), v.wtxid()));

        let plan = BlockPlan {
            kernel_version: ENGINE_KERNEL_VERSION,
            observation_rule_version: ENGINE_OBSERVATION_RULE_VERSION,
            network: self.draft.network,
            anchor: self.draft.anchor,
            input_context_hash: context.context_hash(),
            raw_variants,
            inclusions,
            tracked_creates: self.tracked_creates.clone(),
            tracked_spends: self.tracked_spends.clone(),
            events: self.events.clone(),
            dispositions: self.dispositions.clone(),
            next_context,
        };
        Ok(FinalizeOutcome::Complete(Box::new(plan)))
    }

    fn process_tx(
        &mut self,
        tx: &TxDraft,
        context: &ProtocolContext,
        bootstrapped: bool,
    ) -> Result<(), FinalizeFailure> {
        // Group deposit carriers first: one deposit message may appear as a
        // witness inscription, an OP_RETURN, or both.
        let mut deposit_carriers: Vec<(&PositionedMessage, MessageLocation)> = Vec::new();

        for outcome in &tx.outcomes {
            match outcome {
                PositionedParseOutcome::Irrelevant { .. } => {}
                PositionedParseOutcome::ContextRequired { .. } => {
                    // Before bootstrap nothing but the bootstrap message can
                    // mean anything; afterwards this outcome is unreachable
                    // because wallets are always supplied.
                    if bootstrapped {
                        return Err(FinalizeFailure::Infrastructure(
                            "parser reported ContextRequired although wallets were supplied".into(),
                        ));
                    }
                }
                PositionedParseOutcome::Unsupported(u) => {
                    return Err(FinalizeFailure::UnsupportedValidEvent {
                        ordinal: EventOrdinal {
                            tx_index: tx.tx_index,
                            location: u.location,
                        },
                        kind: format!("unsupported Via marker {:?}", u.kind),
                    });
                }
                PositionedParseOutcome::Malformed(m) => {
                    let ordinal = EventOrdinal {
                        tx_index: tx.tx_index,
                        location: m.location,
                    };
                    self.reject(ordinal, m.code, m.detail.clone());
                }
                PositionedParseOutcome::Valid(msg) => {
                    if matches!(msg.message, FullInscriptionMessage::L1ToL2Message(_)) {
                        deposit_carriers.push((msg, msg.location));
                        continue;
                    }
                    self.process_message(tx, msg, context, bootstrapped)?;
                }
            }
        }
        self.process_deposits(tx, &deposit_carriers, context, bootstrapped);
        Ok(())
    }

    fn process_deposits(
        &mut self,
        tx: &TxDraft,
        carriers: &[(&PositionedMessage, MessageLocation)],
        context: &ProtocolContext,
        bootstrapped: bool,
    ) {
        if carriers.is_empty() || !bootstrapped {
            return;
        }
        let owner = carriers
            .iter()
            .map(|(_, l)| *l)
            .min()
            .expect("non-empty carriers");
        let ordinal = EventOrdinal {
            tx_index: tx.tx_index,
            location: owner,
        };
        let receiver_of = |m: &PositionedMessage| match &m.message {
            FullInscriptionMessage::L1ToL2Message(d) => Some(d.input.receiver_l2_address.0),
            _ => None,
        };
        let first = receiver_of(carriers[0].0);
        if carriers.iter().any(|(m, _)| receiver_of(m) != first) {
            self.reject(
                ordinal,
                RejectionCode::ConflictingReceiverEncodings,
                "receiver encodings disagree",
            );
            return;
        }
        let Some(receiver) = first else { return };
        let has_input = carriers
            .iter()
            .any(|(_, l)| matches!(l, MessageLocation::Input(_)));
        let has_output = carriers
            .iter()
            .any(|(_, l)| matches!(l, MessageLocation::Output(_)));
        let encoding = match (has_input, has_output) {
            (true, true) => DepositEncoding::Both,
            (true, false) => DepositEncoding::Inscription,
            _ => DepositEncoding::OpReturn,
        };
        let (l2_contract, call_data, sender_script) = match &carriers[0].0.message {
            FullInscriptionMessage::L1ToL2Message(d) => (
                d.input.l2_contract_address.0,
                d.input.call_data.clone(),
                carriers[0].0.signer_script.clone(),
            ),
            _ => unreachable!("deposit carriers only hold L1ToL2Message"),
        };
        self.important.insert(tx.tx_index);
        for (vout, value, script) in &tx.outputs {
            if *script == context.wallets.bridge {
                self.events
                    .push(ProtocolEvent::DepositObserved(DepositObserved {
                        ordinal,
                        subject: OutPoint {
                            txid: tx.txid,
                            vout: *vout,
                        },
                        amount: *value,
                        receiver,
                        l2_contract,
                        call_data: call_data.clone(),
                        sender_script: sender_script.clone(),
                        encoding,
                    }));
            }
        }
    }

    fn process_message(
        &mut self,
        tx: &TxDraft,
        msg: &PositionedMessage,
        context: &ProtocolContext,
        bootstrapped: bool,
    ) -> Result<(), FinalizeFailure> {
        let ordinal = EventOrdinal {
            tx_index: tx.tx_index,
            location: msg.location,
        };
        let subject_txid = tx.txid;
        let signer = msg.signer_script.as_ref();
        let sequencer_signed = signer == Some(&context.wallets.sequencer);
        self.important.insert(tx.tx_index);

        match &msg.message {
            FullInscriptionMessage::SystemBootstrapping(b) => {
                if bootstrapped || !self.rotated.insert("bootstrap") {
                    self.reject(
                        ordinal,
                        RejectionCode::InvalidBootstrap,
                        "context already bootstrapped",
                    );
                    return Ok(());
                }
                let script = |a: &Address<NetworkUnchecked>| {
                    a.clone()
                        .require_network(self.engine.network)
                        .map(|a| a.script_pubkey())
                };
                let (gov, seq, bridge) = match (
                    script(&b.input.governance_address),
                    script(&b.input.sequencer_address),
                    script(&b.input.bridge_musig2_address),
                ) {
                    (Ok(g), Ok(s), Ok(br)) => (g, s, br),
                    _ => {
                        self.reject(
                            ordinal,
                            RejectionCode::MalformedMessage,
                            "bootstrap address on wrong network",
                        );
                        return Ok(());
                    }
                };
                let verifiers: Result<Vec<_>, _> = b
                    .input
                    .verifier_p2wpkh_addresses
                    .iter()
                    .map(script)
                    .collect();
                let Ok(verifiers) = verifiers else {
                    self.reject(
                        ordinal,
                        RejectionCode::MalformedMessage,
                        "verifier address on wrong network",
                    );
                    return Ok(());
                };
                self.events.push(ProtocolEvent::SystemBootstrapping(
                    SystemBootstrappingObserved {
                        ordinal,
                        subject_txid,
                        start_block_height: u64::from(b.input.start_block_height),
                        protocol_version: version_tag(&b.input.protocol_version),
                        bootloader_hash: b.input.bootloader_hash.to_fixed_bytes(),
                        abstract_account_hash: b.input.abstract_account_hash.to_fixed_bytes(),
                        snark_wrapper_vk_hash: b.input.snark_wrapper_vk_hash.to_fixed_bytes(),
                        evm_emulator_hash: b.input.evm_emulator_hash.to_fixed_bytes(),
                        wallets: WalletSet {
                            sequencer: seq,
                            bridge,
                            governance: gov,
                            verifiers,
                        },
                    },
                ));
            }
            FullInscriptionMessage::L1BatchDAReference(b) => {
                if !sequencer_signed {
                    self.reject(
                        ordinal,
                        RejectionCode::Unauthorized,
                        "batch reference not sequencer-signed",
                    );
                    return Ok(());
                }
                self.events.push(ProtocolEvent::L1BatchDAReference(
                    L1BatchDAReferenceObserved {
                        ordinal,
                        subject_txid,
                        l1_batch_hash: b.input.l1_batch_hash.to_fixed_bytes(),
                        l1_batch_index: u64::from(b.input.l1_batch_index.0),
                        da_identifier: b.input.da_identifier.clone(),
                        blob_id: b.input.blob_id.clone(),
                        prev_l1_batch_hash: b.input.prev_l1_batch_hash.to_fixed_bytes(),
                    },
                ));
            }
            FullInscriptionMessage::ProofDAReference(p) => {
                if !sequencer_signed {
                    self.reject(
                        ordinal,
                        RejectionCode::Unauthorized,
                        "proof reference not sequencer-signed",
                    );
                    return Ok(());
                }
                let Some(batch) = self.batch_snapshot_of_proof(msg)? else {
                    return Ok(());
                };
                let Some(batch) = batch else {
                    self.reject(
                        ordinal,
                        RejectionCode::InvalidReference,
                        "referenced batch unknown or not a batch reference",
                    );
                    return Ok(());
                };
                self.events
                    .push(ProtocolEvent::ProofDAReference(ProofDAReferenceObserved {
                        ordinal,
                        subject_txid,
                        da_identifier: p.input.da_identifier.clone(),
                        blob_id: p.input.blob_id.clone(),
                        batch,
                    }));
            }
            FullInscriptionMessage::ValidatorAttestation(a) => {
                let Some(attester) = signer.filter(|s| context.wallets.verifiers.contains(s))
                else {
                    self.reject(
                        ordinal,
                        RejectionCode::Unauthorized,
                        "attestation not verifier-signed",
                    );
                    return Ok(());
                };
                let attester = attester.clone();
                let Some(proof_msgs) = self.resolve_proof_of_attestation(a.input.reference_txid)?
                else {
                    return Ok(());
                };
                let Some(batch) = proof_msgs else {
                    self.reject(
                        ordinal,
                        RejectionCode::InvalidReference,
                        "referenced proof unknown or not a proof reference",
                    );
                    return Ok(());
                };
                self.events.push(ProtocolEvent::ValidatorAttestation(
                    ValidatorAttestationObserved {
                        ordinal,
                        subject_txid,
                        reference_txid: a.input.reference_txid,
                        ok: matches!(a.input.attestation, Vote::Ok),
                        attester_script: attester,
                        batch,
                    },
                ));
            }
            FullInscriptionMessage::SystemContractUpgradeProposal(p) => {
                self.events
                    .push(ProtocolEvent::SystemContractUpgradeProposal(
                        SystemContractUpgradeProposalObserved {
                            ordinal,
                            subject_txid,
                            proposal: upgrade_payload(&p.input),
                        },
                    ));
            }
            FullInscriptionMessage::SystemContractUpgrade(u) => {
                if !self.first_input_has_role(tx, TrackedRole::Governance)? {
                    self.reject(
                        ordinal,
                        RejectionCode::Unauthorized,
                        "activation does not spend a governance output",
                    );
                    return Ok(());
                }
                let Some(resolved) = self.messages_of(u.input.proposal_tx_id)? else {
                    return Ok(());
                };
                let payload = resolved.and_then(|msgs| {
                    msgs.into_iter().find_map(|m| match m.message {
                        FullInscriptionMessage::SystemContractUpgradeProposal(p) => {
                            Some(upgrade_payload(&p.input))
                        }
                        _ => None,
                    })
                });
                let Some(payload) = payload else {
                    self.reject(
                        ordinal,
                        RejectionCode::InvalidReference,
                        "referenced proposal unknown or not an upgrade proposal",
                    );
                    return Ok(());
                };
                if payload.version <= context.protocol_version {
                    self.reject(
                        ordinal,
                        RejectionCode::NonMonotonicUpgrade,
                        "activated version does not increase",
                    );
                    return Ok(());
                }
                if !self.rotated.insert("upgrade") {
                    self.reject(
                        ordinal,
                        RejectionCode::ConflictingRoleUpdate,
                        "second upgrade activation in one block",
                    );
                    return Ok(());
                }
                self.events
                    .push(ProtocolEvent::SystemContractUpgradeActivation(
                        SystemContractUpgradeActivationObserved {
                            ordinal,
                            subject_txid,
                            proposal_txid: u.input.proposal_tx_id,
                            proposal: payload,
                        },
                    ));
            }
            FullInscriptionMessage::BridgeWithdrawal(w) => {
                if !self.first_input_has_role(tx, TrackedRole::Bridge)? {
                    self.reject(
                        ordinal,
                        RejectionCode::Unauthorized,
                        "withdrawal does not spend a bridge output",
                    );
                    return Ok(());
                }
                let withdrawals: Result<Vec<WithdrawalOutput>, String> = w
                    .input
                    .withdrawals
                    .iter()
                    .map(|wd| {
                        let mut l2_id = [0u8; 8];
                        let bytes = hex::decode(&wd.l2_meta.l2_id).map_err(|e| e.to_string())?;
                        if bytes.len() != 8 {
                            return Err(format!("l2_id must be 8 bytes, got {}", bytes.len()));
                        }
                        l2_id.copy_from_slice(&bytes);
                        Ok(WithdrawalOutput {
                            l2_id,
                            l2_tx_event_index: wd.l2_meta.l2_tx_event_index,
                            receiver_script: wd.receiver.script_pubkey(),
                            amount: wd.value,
                        })
                    })
                    .collect();
                let withdrawals = match withdrawals {
                    Ok(w) => w,
                    Err(detail) => {
                        self.reject(ordinal, RejectionCode::MalformedMessage, detail);
                        return Ok(());
                    }
                };
                self.events
                    .push(ProtocolEvent::BridgeWithdrawal(BridgeWithdrawalObserved {
                        ordinal,
                        subject_txid,
                        withdrawal_version: w.input.version.clone() as u32,
                        total_size: w.input.total_size.max(0) as u64,
                        v_size: w.input.v_size.max(0) as u64,
                        inputs: w.input.inputs.clone(),
                        output_amount: Amount::from_sat(w.input.output_amount),
                        withdrawals,
                    }));
            }
            FullInscriptionMessage::UpdateBridgeProposal(p) => {
                let scripts: Result<Vec<_>, _> = p
                    .input
                    .verifier_p2wpkh_addresses
                    .iter()
                    .map(|a| {
                        a.clone()
                            .require_network(self.engine.network)
                            .map(|a| a.script_pubkey())
                    })
                    .collect();
                let bridge = p
                    .input
                    .bridge_musig2_address
                    .clone()
                    .require_network(self.engine.network);
                let (Ok(verifier_scripts), Ok(bridge)) = (scripts, bridge) else {
                    self.reject(
                        ordinal,
                        RejectionCode::MalformedMessage,
                        "proposal address on wrong network",
                    );
                    return Ok(());
                };
                self.events.push(ProtocolEvent::UpdateBridgeProposal(
                    UpdateBridgeProposalObserved {
                        ordinal,
                        subject_txid,
                        bridge_script: bridge.script_pubkey(),
                        verifier_scripts,
                    },
                ));
            }
            FullInscriptionMessage::UpdateBridge(u) => {
                if !self.first_input_has_role(tx, TrackedRole::Governance)? {
                    self.reject(
                        ordinal,
                        RejectionCode::Unauthorized,
                        "bridge rotation does not spend a governance output",
                    );
                    return Ok(());
                }
                let Some(resolved) = self.messages_of(u.input.proposal_tx_id)? else {
                    return Ok(());
                };
                let proposal = resolved.and_then(|msgs| {
                    msgs.into_iter().find_map(|m| match m.message {
                        FullInscriptionMessage::UpdateBridgeProposal(p) => Some(p),
                        _ => None,
                    })
                });
                let Some(proposal) = proposal else {
                    self.reject(
                        ordinal,
                        RejectionCode::InvalidReference,
                        "referenced proposal unknown or not a bridge proposal",
                    );
                    return Ok(());
                };
                let scripts: Result<Vec<_>, _> = proposal
                    .input
                    .verifier_p2wpkh_addresses
                    .iter()
                    .map(|a| {
                        a.clone()
                            .require_network(self.engine.network)
                            .map(|a| a.script_pubkey())
                    })
                    .collect();
                let bridge = proposal
                    .input
                    .bridge_musig2_address
                    .clone()
                    .require_network(self.engine.network);
                let (Ok(new_verifier_scripts), Ok(bridge)) = (scripts, bridge) else {
                    self.reject(
                        ordinal,
                        RejectionCode::MalformedMessage,
                        "proposal address on wrong network",
                    );
                    return Ok(());
                };
                self.rotate(
                    ordinal,
                    subject_txid,
                    "bridge",
                    WalletRotation::Bridge {
                        proposal_txid: u.input.proposal_tx_id,
                        new_bridge_script: bridge.script_pubkey(),
                        new_verifier_scripts,
                    },
                );
            }
            FullInscriptionMessage::UpdateSequencer(u) => {
                if !self.first_input_has_role(tx, TrackedRole::Governance)? {
                    self.reject(
                        ordinal,
                        RejectionCode::Unauthorized,
                        "sequencer rotation does not spend a governance output",
                    );
                    return Ok(());
                }
                match u.input.address.clone().require_network(self.engine.network) {
                    Ok(a) => self.rotate(
                        ordinal,
                        subject_txid,
                        "sequencer",
                        WalletRotation::Sequencer {
                            new_script: a.script_pubkey(),
                        },
                    ),
                    Err(_) => self.reject(
                        ordinal,
                        RejectionCode::MalformedMessage,
                        "address on wrong network",
                    ),
                }
            }
            FullInscriptionMessage::UpdateGovernance(u) => {
                if !self.first_input_has_role(tx, TrackedRole::Governance)? {
                    self.reject(
                        ordinal,
                        RejectionCode::Unauthorized,
                        "governance rotation does not spend a governance output",
                    );
                    return Ok(());
                }
                match u.input.address.clone().require_network(self.engine.network) {
                    Ok(a) => self.rotate(
                        ordinal,
                        subject_txid,
                        "governance",
                        WalletRotation::Governance {
                            new_script: a.script_pubkey(),
                        },
                    ),
                    Err(_) => self.reject(
                        ordinal,
                        RejectionCode::MalformedMessage,
                        "address on wrong network",
                    ),
                }
            }
            FullInscriptionMessage::L1ToL2Message(_) => {
                unreachable!("deposits are grouped before per-message processing")
            }
        }
        Ok(())
    }

    fn rotate(
        &mut self,
        ordinal: EventOrdinal,
        subject_txid: Txid,
        role: &'static str,
        rotation: WalletRotation,
    ) {
        if !self.rotated.insert(role) {
            self.reject(
                ordinal,
                RejectionCode::ConflictingRoleUpdate,
                format!("second {role} rotation in one block"),
            );
            return;
        }
        self.events
            .push(ProtocolEvent::WalletRotation(WalletRotationObserved {
                ordinal,
                subject_txid,
                rotation,
            }));
    }

    /// Attestation chain: reference txid must hold a proof reference, whose
    /// content names the batch. `Ok(None)` means dependencies still pending.
    fn resolve_proof_of_attestation(
        &mut self,
        reference_txid: Txid,
    ) -> Result<Option<Option<BatchReferenceSnapshot>>, FinalizeFailure> {
        let Some(resolved) = self.messages_of(reference_txid)? else {
            return Ok(None);
        };
        let Some(msgs) = resolved else {
            return Ok(Some(None));
        };
        for m in msgs {
            if matches!(m.message, FullInscriptionMessage::ProofDAReference(_)) {
                return self.batch_snapshot_of_proof(&m);
            }
        }
        Ok(Some(None))
    }

    fn first_input_has_role(
        &mut self,
        tx: &TxDraft,
        role: TrackedRole,
    ) -> Result<bool, FinalizeFailure> {
        let Some((_, outpoint)) = tx.inputs.first() else {
            return Ok(false);
        };
        match self.tracked_role_of(*outpoint) {
            Some(Some(r)) => Ok(r == role),
            Some(None) => Ok(false),
            // Unavailable or unrequested: recorded by tracked_role_of.
            None => Ok(false),
        }
    }
}

fn version_tag(v: &zksync_types::protocol_version::ProtocolSemanticVersion) -> ProtocolVersionTag {
    ProtocolVersionTag {
        minor: v.minor as u32,
        patch: v.patch.0,
    }
}

fn upgrade_payload(input: &crate::types::SystemContractUpgradeProposalInput) -> UpgradePayload {
    UpgradePayload {
        version: version_tag(&input.version),
        bootloader_code_hash: input.bootloader_code_hash.to_fixed_bytes(),
        default_account_code_hash: input.default_account_code_hash.to_fixed_bytes(),
        evm_emulator_code_hash: input.evm_emulator_code_hash.map(|h| h.to_fixed_bytes()),
        recursion_scheduler_level_vk_hash: input.recursion_scheduler_level_vk_hash.to_fixed_bytes(),
        system_contracts: input
            .system_contracts
            .iter()
            .map(|(a, h)| (a.0, h.to_fixed_bytes()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use bitcoin::{
        hashes::Hash, Address, Amount, BlockHash, Network, OutPoint, ScriptBuf, Transaction, TxOut,
        Txid,
    };
    use via_btc_ingestion::{
        BlockAnchor, CanonicalObservedTx, DependencyKey, DepositEncoding, DispositionKind,
        FinalizeFailure, FinalizeOutcome, Inclusion, MessageLocation, ProtocolContext,
        ProtocolEngine, ProtocolEvent, ProtocolVersionTag, RejectionCode, Resolution,
        ResolvedDependency, TrackedOutputCreate, TrackedRole, WalletRotation,
    };
    use zksync_types::{
        protocol_version::{ProtocolSemanticVersion, ProtocolVersionId, VersionPatch},
        Address as EvmAddress, L1BatchNumber, H256,
    };

    use super::ViaProtocolEngine;
    use crate::{
        test_message_encoder::{
            activation_tx, external_input, inscribed_tx, op_return, p2tr, p2wpkh, rotation_tx,
            seeded_outpoint, transaction, BRIDGE_SEED, GOVERNANCE_SEED, SEQUENCER_SEED,
            VERIFIER_SEED,
        },
        types::{
            InscriptionMessage, L1BatchDAReferenceInput, L1ToL2MessageInput, ProofDAReferenceInput,
            SystemBootstrappingInput, SystemContractUpgradeProposalInput,
            ValidatorAttestationInput, Vote,
        },
    };

    fn context() -> ProtocolContext {
        ProtocolContext {
            version: 1,
            wallets: via_btc_ingestion::WalletSet {
                sequencer: p2wpkh(SEQUENCER_SEED, Network::Regtest).0.script_pubkey(),
                bridge: p2tr(BRIDGE_SEED, Network::Regtest).script_pubkey(),
                governance: p2wpkh(GOVERNANCE_SEED, Network::Regtest).0.script_pubkey(),
                verifiers: vec![p2wpkh(VERIFIER_SEED, Network::Regtest).0.script_pubkey()],
            },
            protocol_version: ProtocolVersionTag {
                minor: 26,
                patch: 0,
            },
        }
    }

    fn unbootstrapped_context() -> ProtocolContext {
        ProtocolContext {
            version: 1,
            wallets: via_btc_ingestion::WalletSet {
                sequencer: ScriptBuf::new(),
                bridge: ScriptBuf::new(),
                governance: ScriptBuf::new(),
                verifiers: vec![],
            },
            protocol_version: ProtocolVersionTag { minor: 0, patch: 0 },
        }
    }

    fn anchor(height: u64) -> BlockAnchor {
        BlockAnchor {
            height,
            hash: BlockHash::from_byte_array([height as u8; 32]),
            prev_hash: BlockHash::from_byte_array([height.saturating_sub(1) as u8; 32]),
            time: 1_700_000_000 + height as u32,
        }
    }

    fn envelope(
        height: u64,
        transactions: Vec<Transaction>,
    ) -> via_btc_ingestion::BitcoinBlockEnvelope {
        via_btc_ingestion::BitcoinBlockEnvelope {
            network: Network::Regtest,
            anchor: anchor(height),
            transactions,
        }
    }

    fn bridge_output(value: u64) -> TxOut {
        TxOut {
            value: Amount::from_sat(value),
            script_pubkey: context().wallets.bridge,
        }
    }

    fn absent(key: &DependencyKey) -> ResolvedDependency {
        match key {
            DependencyKey::RawTx(_) => ResolvedDependency::RawTx(Resolution::KnownAbsent),
            DependencyKey::TrackedOutput(_) => {
                ResolvedDependency::TrackedOutput(Resolution::KnownAbsent)
            }
        }
    }

    fn all_known_absent(keys: &[DependencyKey]) -> BTreeMap<DependencyKey, ResolvedDependency> {
        keys.iter()
            .cloned()
            .map(|key| {
                let value = absent(&key);
                (key, value)
            })
            .collect()
    }

    fn complete_plan(
        engine: &ViaProtocolEngine,
        envelope: &via_btc_ingestion::BitcoinBlockEnvelope,
        context: &ProtocolContext,
        overrides: impl IntoIterator<Item = (DependencyKey, ResolvedDependency)>,
    ) -> via_btc_ingestion::BlockPlan {
        let (draft, keys) = engine.inspect(envelope, context);
        let mut deps = all_known_absent(&keys);
        deps.extend(overrides);
        match engine.finalize(draft, &deps).unwrap() {
            FinalizeOutcome::Complete(plan) => *plan,
            FinalizeOutcome::NeedDependencies { keys, .. } => {
                panic!("unexpected second dependency round: {keys:?}")
            }
        }
    }

    fn tracked_resolution(
        outpoint: OutPoint,
        role: TrackedRole,
        script: ScriptBuf,
    ) -> ResolvedDependency {
        ResolvedDependency::TrackedOutput(Resolution::Present(TrackedOutputCreate {
            outpoint,
            value: Amount::from_sat(50_000),
            script_pubkey: script,
            role,
        }))
    }

    fn observed(tx: &Transaction, tag: u8) -> CanonicalObservedTx {
        let variant = via_btc_ingestion::RawTxVariant::from_transaction(tx);
        CanonicalObservedTx::new(
            Inclusion {
                block_hash: BlockHash::from_byte_array([tag; 32]),
                height: u64::from(tag),
                tx_index: 0,
                txid: variant.txid(),
                wtxid: variant.wtxid(),
            },
            variant,
        )
        .unwrap()
    }

    fn deposits(plan: &via_btc_ingestion::BlockPlan) -> Vec<&via_btc_ingestion::DepositObserved> {
        plan.events
            .iter()
            .filter_map(|event| match event {
                ProtocolEvent::DepositObserved(deposit) => Some(deposit),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn deposits_are_per_output_for_single_and_multi_output_transactions() {
        let engine = ViaProtocolEngine::new(Network::Regtest);
        let receiver = [0x41; 20];
        let single_tx = transaction(
            vec![external_input(
                seeded_outpoint(1),
                p2wpkh(21, Network::Regtest).1,
            )],
            vec![bridge_output(10_000), op_return(receiver.to_vec())],
        );
        let single = complete_plan(&engine, &envelope(100, vec![single_tx]), &context(), []);
        let single_deposits = deposits(&single);
        assert_eq!(single_deposits.len(), 1);
        assert_eq!(single_deposits[0].subject.vout, 0);
        assert_eq!(single_deposits[0].amount, Amount::from_sat(10_000));
        assert_eq!(single_deposits[0].encoding, DepositEncoding::OpReturn);

        let multi_tx = transaction(
            vec![external_input(
                seeded_outpoint(2),
                p2wpkh(22, Network::Regtest).1,
            )],
            vec![
                bridge_output(11_000),
                bridge_output(12_000),
                op_return(receiver.to_vec()),
            ],
        );
        let multi = complete_plan(&engine, &envelope(101, vec![multi_tx]), &context(), []);
        let multi_deposits = deposits(&multi);
        assert_eq!(multi_deposits.len(), 2);
        assert_eq!(multi_deposits[0].subject.vout, 0);
        assert_eq!(multi_deposits[0].amount, Amount::from_sat(11_000));
        assert_eq!(multi_deposits[1].subject.vout, 1);
        assert_eq!(multi_deposits[1].amount, Amount::from_sat(12_000));
    }

    #[test]
    fn dual_deposit_encodings_agree_or_reject_and_use_input_ownership() {
        let engine = ViaProtocolEngine::new(Network::Regtest);
        let receiver = EvmAddress::from([0x51; 20]);
        let message = InscriptionMessage::L1ToL2Message(L1ToL2MessageInput {
            receiver_l2_address: receiver,
            l2_contract_address: EvmAddress::from([0x61; 20]),
            call_data: vec![1, 2, 3],
        });
        let agreeing_tx = inscribed_tx(
            seeded_outpoint(24),
            23,
            24,
            message.clone(),
            vec![
                bridge_output(20_000),
                op_return(receiver.as_bytes().to_vec()),
            ],
            Network::Regtest,
        );
        let agreeing = complete_plan(&engine, &envelope(102, vec![agreeing_tx]), &context(), []);
        let agreeing_deposits = deposits(&agreeing);
        assert_eq!(agreeing_deposits.len(), 1);
        assert_eq!(agreeing_deposits[0].encoding, DepositEncoding::Both);
        assert_eq!(
            agreeing_deposits[0].ordinal.location,
            MessageLocation::Input(1),
            "the witness carrier owns an agreeing dual encoding"
        );

        let disagreeing_tx = inscribed_tx(
            seeded_outpoint(26),
            25,
            26,
            message,
            vec![bridge_output(21_000), op_return(vec![0x71; 20])],
            Network::Regtest,
        );
        let disagreeing = complete_plan(
            &engine,
            &envelope(103, vec![disagreeing_tx]),
            &context(),
            [],
        );
        assert!(deposits(&disagreeing).is_empty());
        assert!(matches!(
            &disagreeing.dispositions[..],
            [via_btc_ingestion::Disposition {
                kind: DispositionKind::RejectedInvalid {
                    code: RejectionCode::ConflictingReceiverEncodings,
                    ..
                },
                ..
            }]
        ));
    }

    fn batch_tx() -> Transaction {
        inscribed_tx(
            seeded_outpoint(31),
            SEQUENCER_SEED,
            31,
            InscriptionMessage::L1BatchDAReference(L1BatchDAReferenceInput {
                l1_batch_hash: H256::from([0x81; 32]),
                l1_batch_index: L1BatchNumber(7),
                da_identifier: "celestia".into(),
                blob_id: "batch-blob".into(),
                prev_l1_batch_hash: H256::from([0x80; 32]),
            }),
            vec![],
            Network::Regtest,
        )
    }

    fn proof_tx(batch_txid: Txid) -> Transaction {
        inscribed_tx(
            seeded_outpoint(33),
            SEQUENCER_SEED,
            33,
            InscriptionMessage::ProofDAReference(ProofDAReferenceInput {
                l1_batch_reveal_txid: batch_txid,
                da_identifier: "celestia".into(),
                blob_id: "proof-blob".into(),
            }),
            vec![],
            Network::Regtest,
        )
    }

    fn attestation_tx(proof_txid: Txid) -> Transaction {
        inscribed_tx(
            seeded_outpoint(35),
            VERIFIER_SEED,
            35,
            InscriptionMessage::ValidatorAttestation(ValidatorAttestationInput {
                reference_txid: proof_txid,
                attestation: Vote::Ok,
            }),
            vec![],
            Network::Regtest,
        )
    }

    #[test]
    fn attestation_resolves_two_hops_and_carries_batch_snapshot() {
        let engine = ViaProtocolEngine::new(Network::Regtest);
        let batch = batch_tx();
        let proof = proof_tx(batch.compute_txid());
        let attestation = attestation_tx(proof.compute_txid());
        let env = envelope(110, vec![attestation]);
        let ctx = context();
        let (draft, keys) = engine.inspect(&env, &ctx);
        let mut deps = all_known_absent(&keys);
        deps.insert(
            DependencyKey::RawTx(proof.compute_txid()),
            ResolvedDependency::RawTx(Resolution::Present(observed(&proof, 40))),
        );
        let (draft, requested) = match engine.finalize(draft, &deps).unwrap() {
            FinalizeOutcome::NeedDependencies { draft, keys } => (draft, keys),
            FinalizeOutcome::Complete(_) => panic!("batch dependency must require a second round"),
        };
        assert_eq!(requested, vec![DependencyKey::RawTx(batch.compute_txid())]);
        deps.insert(
            DependencyKey::RawTx(batch.compute_txid()),
            ResolvedDependency::RawTx(Resolution::Present(observed(&batch, 39))),
        );
        let plan = match engine.finalize(draft, &deps).unwrap() {
            FinalizeOutcome::Complete(plan) => plan,
            FinalizeOutcome::NeedDependencies { keys, .. } => panic!("unexpected keys: {keys:?}"),
        };
        let ProtocolEvent::ValidatorAttestation(event) = &plan.events[0] else {
            panic!("expected validator attestation")
        };
        assert_eq!(event.reference_txid, proof.compute_txid());
        assert_eq!(event.batch.reveal_txid, batch.compute_txid());
        assert_eq!(event.batch.l1_batch_hash, [0x81; 32]);
        assert_eq!(event.batch.l1_batch_index, 7);
        assert_eq!(event.batch.prev_l1_batch_hash, [0x80; 32]);
        assert_eq!(event.batch.da_identifier, "celestia");
        assert_eq!(event.batch.blob_id, "batch-blob");
    }

    #[test]
    fn unavailable_second_hop_is_a_typed_missing_dependency() {
        let engine = ViaProtocolEngine::new(Network::Regtest);
        let batch = batch_tx();
        let proof = proof_tx(batch.compute_txid());
        let env = envelope(111, vec![attestation_tx(proof.compute_txid())]);
        let ctx = context();
        let (draft, keys) = engine.inspect(&env, &ctx);
        let mut deps = all_known_absent(&keys);
        deps.insert(
            DependencyKey::RawTx(proof.compute_txid()),
            ResolvedDependency::RawTx(Resolution::Present(observed(&proof, 41))),
        );
        let (draft, requested) = match engine.finalize(draft, &deps).unwrap() {
            FinalizeOutcome::NeedDependencies { draft, keys } => (draft, keys),
            FinalizeOutcome::Complete(_) => panic!("batch dependency must require a second round"),
        };
        let batch_key = DependencyKey::RawTx(batch.compute_txid());
        assert_eq!(requested, vec![batch_key.clone()]);
        deps.insert(
            batch_key.clone(),
            ResolvedDependency::RawTx(Resolution::Unavailable),
        );
        assert!(matches!(
            engine.finalize(draft, &deps),
            Err(FinalizeFailure::MissingDependency(keys)) if keys == vec![batch_key]
        ));
    }

    #[test]
    fn sequencer_rotation_requires_governance_and_folds_context() {
        let engine = ViaProtocolEngine::new(Network::Regtest);
        let governance_outpoint = OutPoint {
            txid: Txid::from_byte_array([0x91; 32]),
            vout: 0,
        };
        let new_sequencer = p2wpkh(45, Network::Regtest).0;
        let tx = rotation_tx(
            governance_outpoint,
            b"VIA_PROTOCOL:SEQ",
            new_sequencer.to_string().as_bytes(),
        );
        let env = envelope(120, vec![tx]);
        let ctx = context();
        let authorized = complete_plan(
            &engine,
            &env,
            &ctx,
            [(
                DependencyKey::TrackedOutput(governance_outpoint),
                tracked_resolution(
                    governance_outpoint,
                    TrackedRole::Governance,
                    ctx.wallets.governance.clone(),
                ),
            )],
        );
        assert!(matches!(
            &authorized.events[..],
            [ProtocolEvent::WalletRotation(event)]
                if matches!(&event.rotation, WalletRotation::Sequencer { new_script }
                    if new_script == &new_sequencer.script_pubkey())
        ));
        assert_eq!(
            authorized.next_context.wallets.sequencer,
            new_sequencer.script_pubkey()
        );

        let unauthorized = complete_plan(&engine, &env, &ctx, []);
        assert!(unauthorized.events.is_empty());
        assert!(matches!(
            &unauthorized.dispositions[0].kind,
            DispositionKind::RejectedInvalid {
                code: RejectionCode::Unauthorized,
                ..
            }
        ));
    }

    #[test]
    fn second_same_role_rotation_is_rejected() {
        let engine = ViaProtocolEngine::new(Network::Regtest);
        let first_outpoint = OutPoint {
            txid: Txid::from_byte_array([0x92; 32]),
            vout: 0,
        };
        let second_outpoint = OutPoint {
            txid: Txid::from_byte_array([0x93; 32]),
            vout: 0,
        };
        let first_address = p2wpkh(46, Network::Regtest).0;
        let second_address = p2wpkh(47, Network::Regtest).0;
        let env = envelope(
            121,
            vec![
                rotation_tx(
                    first_outpoint,
                    b"VIA_PROTOCOL:SEQ",
                    first_address.to_string().as_bytes(),
                ),
                rotation_tx(
                    second_outpoint,
                    b"VIA_PROTOCOL:SEQ",
                    second_address.to_string().as_bytes(),
                ),
            ],
        );
        let ctx = context();
        let plan = complete_plan(
            &engine,
            &env,
            &ctx,
            [
                (
                    DependencyKey::TrackedOutput(first_outpoint),
                    tracked_resolution(
                        first_outpoint,
                        TrackedRole::Governance,
                        ctx.wallets.governance.clone(),
                    ),
                ),
                (
                    DependencyKey::TrackedOutput(second_outpoint),
                    tracked_resolution(
                        second_outpoint,
                        TrackedRole::Governance,
                        ctx.wallets.governance.clone(),
                    ),
                ),
            ],
        );
        assert_eq!(plan.events.len(), 1);
        assert!(matches!(
            &plan.dispositions[0].kind,
            DispositionKind::RejectedInvalid {
                code: RejectionCode::ConflictingRoleUpdate,
                ..
            }
        ));
        assert_eq!(
            plan.next_context.wallets.sequencer,
            first_address.script_pubkey()
        );
    }

    fn unchecked(address: &Address) -> Address<bitcoin::address::NetworkUnchecked> {
        address.to_string().parse().unwrap()
    }

    fn bootstrap_message() -> InscriptionMessage {
        InscriptionMessage::SystemBootstrapping(SystemBootstrappingInput {
            start_block_height: 130,
            protocol_version: ProtocolSemanticVersion::new(
                ProtocolVersionId::Version26,
                VersionPatch(0),
            ),
            bootloader_hash: H256::from([1; 32]),
            abstract_account_hash: H256::from([2; 32]),
            snark_wrapper_vk_hash: H256::from([3; 32]),
            evm_emulator_hash: H256::from([4; 32]),
            governance_address: unchecked(&p2wpkh(GOVERNANCE_SEED, Network::Regtest).0),
            sequencer_address: unchecked(&p2wpkh(SEQUENCER_SEED, Network::Regtest).0),
            bridge_musig2_address: unchecked(&p2tr(BRIDGE_SEED, Network::Regtest)),
            verifier_p2wpkh_addresses: vec![unchecked(&p2wpkh(VERIFIER_SEED, Network::Regtest).0)],
        })
    }

    #[test]
    fn bootstrap_is_valid_once() {
        let engine = ViaProtocolEngine::new(Network::Regtest);
        let tx = inscribed_tx(
            seeded_outpoint(51),
            50,
            51,
            bootstrap_message(),
            vec![],
            Network::Regtest,
        );
        let env = envelope(130, vec![tx]);
        let initial = unbootstrapped_context();
        let first = complete_plan(&engine, &env, &initial, []);
        assert!(matches!(
            &first.events[..],
            [ProtocolEvent::SystemBootstrapping(_)]
        ));
        assert!(first.next_context.wallets.is_bootstrapped());
        assert_eq!(
            first.next_context.protocol_version,
            ProtocolVersionTag {
                minor: 26,
                patch: 0
            }
        );

        let second = complete_plan(&engine, &env, &context(), []);
        assert!(second.events.is_empty());
        assert!(matches!(
            &second.dispositions[0].kind,
            DispositionKind::RejectedInvalid {
                code: RejectionCode::InvalidBootstrap,
                ..
            }
        ));
    }

    #[test]
    fn second_bootstrap_in_same_block_is_rejected() {
        let engine = ViaProtocolEngine::new(Network::Regtest);
        let first = inscribed_tx(
            seeded_outpoint(51),
            50,
            51,
            bootstrap_message(),
            vec![],
            Network::Regtest,
        );
        let second = inscribed_tx(
            seeded_outpoint(52),
            50,
            52,
            bootstrap_message(),
            vec![],
            Network::Regtest,
        );
        let env = envelope(130, vec![first, second]);
        let plan = complete_plan(&engine, &env, &unbootstrapped_context(), []);
        assert!(
            matches!(&plan.events[..], [ProtocolEvent::SystemBootstrapping(_)]),
            "only the first bootstrap may become an event"
        );
        assert!(matches!(
            &plan.dispositions[0].kind,
            DispositionKind::RejectedInvalid {
                code: RejectionCode::InvalidBootstrap,
                ..
            }
        ));
        assert!(plan.next_context.wallets.is_bootstrapped());
    }

    fn upgrade_proposal(version: ProtocolVersionId, seed: u8) -> Transaction {
        inscribed_tx(
            seeded_outpoint(seed),
            52,
            seed,
            InscriptionMessage::SystemContractUpgradeProposal(SystemContractUpgradeProposalInput {
                version: ProtocolSemanticVersion::new(version, VersionPatch(0)),
                bootloader_code_hash: H256::from([5; 32]),
                default_account_code_hash: H256::from([6; 32]),
                evm_emulator_code_hash: None,
                recursion_scheduler_level_vk_hash: H256::from([7; 32]),
                system_contracts: vec![],
            }),
            vec![],
            Network::Regtest,
        )
    }

    fn upgrade_plan(version: ProtocolVersionId, height: u64) -> via_btc_ingestion::BlockPlan {
        let engine = ViaProtocolEngine::new(Network::Regtest);
        let proposal = upgrade_proposal(version, height as u8);
        let governance_outpoint = OutPoint {
            txid: Txid::from_byte_array([height as u8; 32]),
            vout: 0,
        };
        let env = envelope(
            height,
            vec![activation_tx(governance_outpoint, proposal.compute_txid())],
        );
        let ctx = context();
        complete_plan(
            &engine,
            &env,
            &ctx,
            [
                (
                    DependencyKey::TrackedOutput(governance_outpoint),
                    tracked_resolution(
                        governance_outpoint,
                        TrackedRole::Governance,
                        ctx.wallets.governance.clone(),
                    ),
                ),
                (
                    DependencyKey::RawTx(proposal.compute_txid()),
                    ResolvedDependency::RawTx(Resolution::Present(observed(&proposal, 60))),
                ),
            ],
        )
    }

    #[test]
    fn upgrade_activation_must_be_monotonic() {
        let monotonic = upgrade_plan(ProtocolVersionId::Version27, 140);
        assert!(matches!(
            &monotonic.events[..],
            [ProtocolEvent::SystemContractUpgradeActivation(_)]
        ));
        assert_eq!(
            monotonic.next_context.protocol_version,
            ProtocolVersionTag {
                minor: 27,
                patch: 0
            }
        );

        let non_monotonic = upgrade_plan(ProtocolVersionId::Version26, 141);
        assert!(non_monotonic.events.is_empty());
        assert!(matches!(
            &non_monotonic.dispositions[0].kind,
            DispositionKind::RejectedInvalid {
                code: RejectionCode::NonMonotonicUpgrade,
                ..
            }
        ));
    }

    #[test]
    fn identical_inputs_produce_identical_canonical_plans() {
        let engine = ViaProtocolEngine::new(Network::Regtest);
        let receiver = [0xA1; 20];
        let env = envelope(
            150,
            vec![transaction(
                vec![external_input(
                    seeded_outpoint(70),
                    p2wpkh(71, Network::Regtest).1,
                )],
                vec![bridge_output(30_000), op_return(receiver.to_vec())],
            )],
        );
        let ctx = context();
        let first = complete_plan(&engine, &env, &ctx, []);
        let second = complete_plan(&engine, &env, &ctx, []);
        assert_eq!(
            first.canonical_bytes().unwrap(),
            second.canonical_bytes().unwrap()
        );
        assert_eq!(first.plan_hash().unwrap(), second.plan_hash().unwrap());
    }
}
