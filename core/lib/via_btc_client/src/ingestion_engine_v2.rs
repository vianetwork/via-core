use std::collections::{BTreeMap, BTreeSet};

use bitcoin::{
    consensus::encode::deserialize, Address, Network, OutPoint, ScriptBuf, Transaction, Txid,
};
use via_btc_ingestion::{
    fold_context, validate_resolutions, BatchReferenceSnapshot, BitcoinBlockEnvelope, BlockPlan,
    BridgeWithdrawalObserved, DependencyKey, DepositEncoding, DepositObserved, Disposition,
    DispositionKind, EventOrdinal, FinalizeFailure, FinalizeOutcome, Inclusion, KernelVersion,
    L1BatchDAReferenceObserved, MessageLocation, ObservationRuleVersion, ProofDAReferenceObserved,
    ProtocolContext, ProtocolEngine, ProtocolEvent, ProtocolVersionTag, RawTxVariant,
    RejectionCode, Resolution, ResolvedDependency, SystemBootstrappingObserved,
    SystemContractUpgradeActivationObserved, SystemContractUpgradeProposalObserved,
    TrackedOutputCreate, TrackedOutputSpend, TrackedRole, UpdateBridgeProposalObserved,
    UpgradePayload, ValidatorAttestationObserved, WalletRotation, WalletRotationObserved,
    WalletSet, WithdrawalOutput,
};
use zksync_types::via_wallet::SystemWallets;

use crate::{
    indexer::{
        positioned::{PositionedMessage, PositionedMessageParser, PositionedParseOutcome},
        withdrawal::WithdrawalVersion,
    },
    types::{FullInscriptionMessage, Vote},
};

pub const ENGINE_V2_KERNEL_VERSION: KernelVersion = KernelVersion(1);
pub const ENGINE_V2_OBSERVATION_RULE_VERSION: ObservationRuleVersion = ObservationRuleVersion(1);

#[derive(Clone, Debug)]
pub struct ViaProtocolEngineV2 {
    network: Network,
}

#[derive(Clone, Debug)]
pub struct ViaProtocolDraftV2 {
    envelope: BitcoinBlockEnvelope,
    context: ProtocolContext,
}

#[derive(Clone)]
struct Candidate {
    tx_index: u32,
    txid: Txid,
    positioned: PositionedMessage,
    input_roles: Vec<TrackedRole>,
}

type CurrentTransactions = BTreeMap<Txid, u32>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RotationRole {
    Sequencer,
    Governance,
    Bridge,
}

impl ViaProtocolEngineV2 {
    pub fn new(network: Network) -> Self {
        Self { network }
    }

    fn parser_wallets(
        &self,
        context: &ProtocolContext,
    ) -> Result<Option<SystemWallets>, FinalizeFailure> {
        context.wallets.validate().map_err(|err| {
            FinalizeFailure::Infrastructure(format!("invalid protocol wallet set: {err}"))
        })?;

        let all_primary_empty = context.wallets.sequencer.is_empty()
            && context.wallets.bridge.is_empty()
            && context.wallets.governance.is_empty();
        if all_primary_empty && context.wallets.verifiers.is_empty() {
            return Ok(None);
        }
        if !context.wallets.is_bootstrapped() {
            return Err(FinalizeFailure::Infrastructure(
                "protocol context contains a partially initialized wallet set".into(),
            ));
        }

        let address = |script: &ScriptBuf, role: &str| {
            Address::from_script(script, self.network).map_err(|err| {
                FinalizeFailure::Infrastructure(format!(
                    "{role} script is not a standard address: {err}"
                ))
            })
        };
        Ok(Some(SystemWallets {
            sequencer: address(&context.wallets.sequencer, "sequencer")?,
            bridge: address(&context.wallets.bridge, "bridge")?,
            governance: address(&context.wallets.governance, "governance")?,
            verifiers: context
                .wallets
                .verifiers
                .iter()
                .enumerate()
                .map(|(index, script)| address(script, &format!("verifier {index}")))
                .collect::<Result<_, _>>()?,
        }))
    }

    fn parse_envelope(
        &self,
        envelope: &BitcoinBlockEnvelope,
        wallets: Option<&SystemWallets>,
    ) -> Result<Vec<Vec<PositionedParseOutcome>>, FinalizeFailure> {
        let height = u32::try_from(envelope.anchor.height).map_err(|_| {
            FinalizeFailure::Infrastructure(format!(
                "block height {} exceeds the positioned parser range",
                envelope.anchor.height
            ))
        })?;
        let mut parser = PositionedMessageParser::new(self.network);
        Ok(envelope
            .transactions
            .iter()
            .enumerate()
            .map(|(tx_index, tx)| parser.parse_transaction(tx, tx_index as u32, height, wallets))
            .collect())
    }

    fn tracked_dependency_keys(envelope: &BitcoinBlockEnvelope) -> BTreeSet<DependencyKey> {
        let created: BTreeSet<OutPoint> = envelope
            .transactions
            .iter()
            .flat_map(|tx| {
                let txid = tx.compute_txid();
                (0..tx.output.len()).map(move |vout| OutPoint {
                    txid,
                    vout: vout as u32,
                })
            })
            .collect();
        envelope
            .transactions
            .iter()
            .flat_map(|tx| tx.input.iter().map(|input| input.previous_output))
            .filter(|outpoint| !outpoint.is_null() && !created.contains(outpoint))
            .map(DependencyKey::TrackedOutput)
            .collect()
    }

    fn direct_reference(message: &FullInscriptionMessage) -> Option<Txid> {
        match message {
            FullInscriptionMessage::ProofDAReference(message) => {
                Some(message.input.l1_batch_reveal_txid)
            }
            FullInscriptionMessage::ValidatorAttestation(message) => {
                Some(message.input.reference_txid)
            }
            FullInscriptionMessage::SystemContractUpgrade(message) => {
                Some(message.input.proposal_tx_id)
            }
            FullInscriptionMessage::UpdateBridge(message) => Some(message.input.proposal_tx_id),
            _ => None,
        }
    }

    fn reference_dependency_keys(
        envelope: &BitcoinBlockEnvelope,
        parsed: &[Vec<PositionedParseOutcome>],
    ) -> BTreeSet<DependencyKey> {
        let current: BTreeSet<Txid> = envelope
            .transactions
            .iter()
            .map(Transaction::compute_txid)
            .collect();
        parsed
            .iter()
            .flat_map(|outcomes| outcomes.iter())
            .filter_map(|outcome| match outcome {
                PositionedParseOutcome::Valid(positioned) => {
                    Self::direct_reference(&positioned.message)
                }
                _ => None,
            })
            .filter(|txid| !current.contains(txid))
            .map(DependencyKey::RawTx)
            .collect()
    }

    fn initial_keys(
        envelope: &BitcoinBlockEnvelope,
        parsed: &[Vec<PositionedParseOutcome>],
    ) -> Vec<DependencyKey> {
        let mut keys = Self::tracked_dependency_keys(envelope);
        keys.extend(Self::reference_dependency_keys(envelope, parsed));
        keys.into_iter().collect()
    }

    fn current_transactions(envelope: &BitcoinBlockEnvelope) -> CurrentTransactions {
        let mut current = BTreeMap::new();
        for (tx_index, tx) in envelope.transactions.iter().enumerate() {
            current.entry(tx.compute_txid()).or_insert(tx_index as u32);
        }
        current
    }

    fn referenced_transaction(
        &self,
        txid: Txid,
        envelope: &BitcoinBlockEnvelope,
        current: &CurrentTransactions,
        dependencies: &BTreeMap<DependencyKey, ResolvedDependency>,
    ) -> Result<Option<(u32, u32, Transaction)>, FinalizeFailure> {
        if let Some(tx_index) = current.get(&txid) {
            let height = u32::try_from(envelope.anchor.height).map_err(|_| {
                FinalizeFailure::Infrastructure("current block height exceeds parser range".into())
            })?;
            let tx = envelope
                .transactions
                .get(*tx_index as usize)
                .ok_or_else(|| {
                    FinalizeFailure::Infrastructure("current transaction index is invalid".into())
                })?;
            return Ok(Some((height, *tx_index, tx.clone())));
        }

        let key = DependencyKey::RawTx(txid);
        match dependencies.get(&key) {
            Some(ResolvedDependency::RawTx(Resolution::Present(observed))) => {
                let height = u32::try_from(observed.inclusion().height).map_err(|_| {
                    FinalizeFailure::Infrastructure(format!(
                        "observed transaction {txid} has a height outside the parser range"
                    ))
                })?;
                let tx = deserialize(observed.variant().raw())
                    .map_err(|_| FinalizeFailure::CorruptDependency(DependencyKey::RawTx(txid)))?;
                Ok(Some((height, observed.inclusion().tx_index, tx)))
            }
            Some(ResolvedDependency::RawTx(Resolution::KnownAbsent)) => Ok(None),
            Some(ResolvedDependency::RawTx(Resolution::Unavailable)) => {
                Err(FinalizeFailure::MissingDependency(vec![key]))
            }
            Some(_) => Err(FinalizeFailure::CorruptDependency(key)),
            None => Err(FinalizeFailure::MissingDependency(vec![key])),
        }
    }

    fn referenced_messages(
        &self,
        txid: Txid,
        envelope: &BitcoinBlockEnvelope,
        current: &CurrentTransactions,
        dependencies: &BTreeMap<DependencyKey, ResolvedDependency>,
        wallets: Option<&SystemWallets>,
    ) -> Result<Option<Vec<PositionedMessage>>, FinalizeFailure> {
        let Some((height, tx_index, tx)) =
            self.referenced_transaction(txid, envelope, current, dependencies)?
        else {
            return Ok(None);
        };
        let mut parser = PositionedMessageParser::new(self.network);
        let messages = parser
            .parse_transaction(&tx, tx_index, height, wallets)
            .into_iter()
            .filter_map(|outcome| match outcome {
                PositionedParseOutcome::Valid(positioned) => Some(*positioned),
                _ => None,
            })
            .collect();
        Ok(Some(messages))
    }

    fn batch_snapshot(
        &self,
        txid: Txid,
        envelope: &BitcoinBlockEnvelope,
        current: &CurrentTransactions,
        dependencies: &BTreeMap<DependencyKey, ResolvedDependency>,
        wallets: Option<&SystemWallets>,
    ) -> Result<Option<BatchReferenceSnapshot>, FinalizeFailure> {
        let Some(messages) =
            self.referenced_messages(txid, envelope, current, dependencies, wallets)?
        else {
            return Ok(None);
        };
        Ok(messages
            .into_iter()
            .find_map(|positioned| match positioned.message {
                FullInscriptionMessage::L1BatchDAReference(message) => {
                    Some(BatchReferenceSnapshot {
                        reveal_txid: txid,
                        l1_batch_hash: message.input.l1_batch_hash.0,
                        l1_batch_index: u64::from(message.input.l1_batch_index.0),
                        da_identifier: message.input.da_identifier,
                        blob_id: message.input.blob_id,
                        prev_l1_batch_hash: message.input.prev_l1_batch_hash.0,
                    })
                }
                _ => None,
            }))
    }

    fn proof_batch_snapshot(
        &self,
        proof_txid: Txid,
        envelope: &BitcoinBlockEnvelope,
        current: &CurrentTransactions,
        dependencies: &BTreeMap<DependencyKey, ResolvedDependency>,
        wallets: Option<&SystemWallets>,
    ) -> Result<Option<BatchReferenceSnapshot>, FinalizeFailure> {
        let Some(messages) =
            self.referenced_messages(proof_txid, envelope, current, dependencies, wallets)?
        else {
            return Ok(None);
        };
        let Some(batch_txid) =
            messages
                .into_iter()
                .find_map(|positioned| match positioned.message {
                    FullInscriptionMessage::ProofDAReference(message) => {
                        Some(message.input.l1_batch_reveal_txid)
                    }
                    _ => None,
                })
        else {
            return Ok(None);
        };
        self.batch_snapshot(batch_txid, envelope, current, dependencies, wallets)
    }

    fn upgrade_payload(
        &self,
        proposal_txid: Txid,
        envelope: &BitcoinBlockEnvelope,
        current: &CurrentTransactions,
        dependencies: &BTreeMap<DependencyKey, ResolvedDependency>,
        wallets: Option<&SystemWallets>,
    ) -> Result<Option<UpgradePayload>, FinalizeFailure> {
        let Some(messages) =
            self.referenced_messages(proposal_txid, envelope, current, dependencies, wallets)?
        else {
            return Ok(None);
        };
        Ok(messages
            .into_iter()
            .find_map(|positioned| match positioned.message {
                FullInscriptionMessage::SystemContractUpgradeProposal(message) => {
                    Some(upgrade_payload_from(message.input))
                }
                _ => None,
            }))
    }

    fn bridge_proposal(
        &self,
        proposal_txid: Txid,
        envelope: &BitcoinBlockEnvelope,
        current: &CurrentTransactions,
        dependencies: &BTreeMap<DependencyKey, ResolvedDependency>,
        wallets: Option<&SystemWallets>,
    ) -> Result<Option<(ScriptBuf, Vec<ScriptBuf>)>, FinalizeFailure> {
        let Some(messages) =
            self.referenced_messages(proposal_txid, envelope, current, dependencies, wallets)?
        else {
            return Ok(None);
        };
        for positioned in messages {
            if let FullInscriptionMessage::UpdateBridgeProposal(message) = positioned.message {
                let Ok(bridge) =
                    checked_address_script(message.input.bridge_musig2_address, self.network)
                else {
                    return Ok(None);
                };
                let Ok(verifiers) = message
                    .input
                    .verifier_p2wpkh_addresses
                    .into_iter()
                    .map(|address| checked_address_script(address, self.network))
                    .collect::<Result<Vec<_>, _>>()
                else {
                    return Ok(None);
                };
                return Ok(Some((bridge, verifiers)));
            }
        }
        Ok(None)
    }

    fn nested_dependency_keys(
        &self,
        envelope: &BitcoinBlockEnvelope,
        parsed: &[Vec<PositionedParseOutcome>],
        current: &CurrentTransactions,
        dependencies: &BTreeMap<DependencyKey, ResolvedDependency>,
        wallets: Option<&SystemWallets>,
    ) -> Result<Vec<DependencyKey>, FinalizeFailure> {
        let mut keys = BTreeSet::new();
        for outcome in parsed.iter().flat_map(|outcomes| outcomes.iter()) {
            let PositionedParseOutcome::Valid(positioned) = outcome else {
                continue;
            };
            let FullInscriptionMessage::ValidatorAttestation(attestation) = &positioned.message
            else {
                continue;
            };
            let proof_txid = attestation.input.reference_txid;
            let Some(messages) =
                self.referenced_messages(proof_txid, envelope, current, dependencies, wallets)?
            else {
                continue;
            };
            let Some(batch_txid) =
                messages
                    .into_iter()
                    .find_map(|positioned| match positioned.message {
                        FullInscriptionMessage::ProofDAReference(proof) => {
                            Some(proof.input.l1_batch_reveal_txid)
                        }
                        _ => None,
                    })
            else {
                continue;
            };
            let key = DependencyKey::RawTx(batch_txid);
            if !current.contains_key(&batch_txid) && !dependencies.contains_key(&key) {
                keys.insert(key);
            }
        }
        Ok(keys.into_iter().collect())
    }

    fn finalize_plan(
        &self,
        draft: ViaProtocolDraftV2,
        parsed: Vec<Vec<PositionedParseOutcome>>,
        dependencies: &BTreeMap<DependencyKey, ResolvedDependency>,
        wallets: Option<&SystemWallets>,
    ) -> Result<BlockPlan, FinalizeFailure> {
        let ViaProtocolDraftV2 { envelope, context } = draft;
        let current = Self::current_transactions(&envelope);
        let mut tracked_creates = BTreeMap::<OutPoint, TrackedOutputCreate>::new();
        let mut tracked_spends = BTreeMap::<OutPoint, TrackedOutputSpend>::new();
        let mut created_so_far = BTreeMap::<OutPoint, TrackedOutputCreate>::new();
        let mut input_roles = Vec::<Vec<TrackedRole>>::with_capacity(envelope.transactions.len());
        let mut relevant = vec![false; envelope.transactions.len()];

        for (tx_index, tx) in envelope.transactions.iter().enumerate() {
            let txid = tx.compute_txid();
            let wtxid = tx.compute_wtxid();
            let mut roles = Vec::new();
            for (input_index, input) in tx.input.iter().enumerate() {
                let outpoint = input.previous_output;
                if outpoint.is_null() {
                    continue;
                }
                let create = if let Some(create) = created_so_far.get(&outpoint) {
                    Some(create.clone())
                } else {
                    match dependencies.get(&DependencyKey::TrackedOutput(outpoint)) {
                        Some(ResolvedDependency::TrackedOutput(Resolution::Present(create))) => {
                            Some(create.clone())
                        }
                        Some(ResolvedDependency::TrackedOutput(Resolution::KnownAbsent)) | None => {
                            None
                        }
                        Some(ResolvedDependency::TrackedOutput(Resolution::Unavailable)) => {
                            return Err(FinalizeFailure::MissingDependency(vec![
                                DependencyKey::TrackedOutput(outpoint),
                            ]));
                        }
                        Some(_) => {
                            return Err(FinalizeFailure::CorruptDependency(
                                DependencyKey::TrackedOutput(outpoint),
                            ));
                        }
                    }
                };
                if let Some(create) = create {
                    if !roles.contains(&create.role) {
                        roles.push(create.role);
                    }
                    tracked_spends
                        .entry(outpoint)
                        .or_insert(TrackedOutputSpend {
                            outpoint,
                            spending_txid: txid,
                            spending_wtxid: wtxid,
                            input_index: input_index as u32,
                        });
                    relevant[tx_index] = true;
                }
            }
            input_roles.push(roles);

            for (vout, output) in tx.output.iter().enumerate() {
                let Some(role) = tracked_role_for_script(&output.script_pubkey, &context.wallets)
                else {
                    continue;
                };
                let outpoint = OutPoint {
                    txid,
                    vout: vout as u32,
                };
                let create = TrackedOutputCreate {
                    outpoint,
                    value: output.value,
                    script_pubkey: output.script_pubkey.clone(),
                    role,
                };
                tracked_creates.insert(outpoint, create.clone());
                created_so_far.insert(outpoint, create);
                relevant[tx_index] = true;
            }
        }

        let mut events = Vec::new();
        let mut dispositions = Vec::new();
        let mut candidates = Vec::new();

        for (tx_index, outcomes) in parsed.into_iter().enumerate() {
            let tx = &envelope.transactions[tx_index];
            let txid = tx.compute_txid();
            let mut deposits = Vec::new();
            for outcome in outcomes {
                match outcome {
                    PositionedParseOutcome::Valid(positioned) => {
                        relevant[tx_index] = true;
                        if let Some(reference) = Self::direct_reference(&positioned.message) {
                            if let Some(referenced_index) = current.get(&reference) {
                                relevant[*referenced_index as usize] = true;
                            }
                            if matches!(
                                &positioned.message,
                                FullInscriptionMessage::ValidatorAttestation(_)
                            ) {
                                if let Some(messages) = self.referenced_messages(
                                    reference,
                                    &envelope,
                                    &current,
                                    dependencies,
                                    wallets,
                                )? {
                                    if let Some(batch_txid) =
                                        messages.into_iter().find_map(|positioned| match positioned
                                            .message
                                        {
                                            FullInscriptionMessage::ProofDAReference(proof) => {
                                                Some(proof.input.l1_batch_reveal_txid)
                                            }
                                            _ => None,
                                        })
                                    {
                                        if let Some(batch_index) = current.get(&batch_txid) {
                                            relevant[*batch_index as usize] = true;
                                        }
                                    }
                                }
                            }
                        }
                        if matches!(
                            &positioned.message,
                            FullInscriptionMessage::L1ToL2Message(_)
                        ) {
                            deposits.push(*positioned);
                        } else {
                            candidates.push(Candidate {
                                tx_index: tx_index as u32,
                                txid,
                                positioned: *positioned,
                                input_roles: input_roles[tx_index].clone(),
                            });
                        }
                    }
                    PositionedParseOutcome::Malformed(malformed) => {
                        relevant[tx_index] = true;
                        dispositions.push(rejection(
                            tx_index as u32,
                            malformed.location,
                            malformed.code,
                            malformed.detail,
                        ));
                    }
                    PositionedParseOutcome::Unsupported(unsupported) => {
                        return Err(FinalizeFailure::UnsupportedValidEvent {
                            ordinal: EventOrdinal {
                                tx_index: tx_index as u32,
                                location: unsupported.location,
                            },
                            kind: String::from_utf8_lossy(&unsupported.kind).into_owned(),
                        });
                    }
                    PositionedParseOutcome::ContextRequired { location, .. } => {
                        relevant[tx_index] = true;
                        dispositions.push(rejection(
                            tx_index as u32,
                            location,
                            RejectionCode::Unauthorized,
                            "message requires a bootstrapped input context",
                        ));
                    }
                    PositionedParseOutcome::Irrelevant { .. } => {}
                }
            }
            self.finish_deposits(
                tx_index as u32,
                tx,
                deposits,
                &context.wallets,
                &mut events,
                &mut dispositions,
            )?;
        }

        candidates.sort_by_key(|candidate| EventOrdinal {
            tx_index: candidate.tx_index,
            location: candidate.positioned.location,
        });
        let mut bootstrap_seen = false;
        let mut rotations = BTreeSet::new();
        let mut upgrade_version = context.protocol_version;
        for candidate in candidates {
            self.process_candidate(
                candidate,
                &envelope,
                &context,
                &current,
                dependencies,
                wallets,
                &mut bootstrap_seen,
                &mut rotations,
                &mut upgrade_version,
                &mut events,
                &mut dispositions,
            )?;
        }

        events.sort_by_key(|event| (event.ordinal(), event.kind(), event.subject()));
        dispositions.sort_by_key(|disposition| disposition.ordinal);
        if dispositions
            .windows(2)
            .any(|pair| pair[0].ordinal == pair[1].ordinal)
        {
            return Err(FinalizeFailure::Infrastructure(
                "two dispositions were produced for one message carrier".into(),
            ));
        }

        let mut raw_variants = BTreeMap::new();
        let mut inclusions = Vec::new();
        for (tx_index, tx) in envelope.transactions.iter().enumerate() {
            if !relevant[tx_index] {
                continue;
            }
            let variant = RawTxVariant::from_transaction(tx);
            inclusions.push(Inclusion {
                block_hash: envelope.anchor.hash,
                height: envelope.anchor.height,
                tx_index: tx_index as u32,
                txid: variant.txid(),
                wtxid: variant.wtxid(),
            });
            raw_variants
                .entry((variant.txid(), variant.wtxid()))
                .or_insert(variant);
        }

        let next_context = fold_context(&context, &events);
        let plan = BlockPlan {
            kernel_version: ENGINE_V2_KERNEL_VERSION,
            observation_rule_version: ENGINE_V2_OBSERVATION_RULE_VERSION,
            network: envelope.network,
            anchor: envelope.anchor,
            input_context_hash: context.context_hash(),
            raw_variants: raw_variants.into_values().collect(),
            inclusions,
            tracked_creates: tracked_creates.into_values().collect(),
            tracked_spends: tracked_spends.into_values().collect(),
            events,
            dispositions,
            next_context,
        };
        plan.validate().map_err(|err| {
            FinalizeFailure::Infrastructure(format!(
                "engine produced a structurally invalid plan: {err}"
            ))
        })?;
        Ok(plan)
    }

    fn finish_deposits(
        &self,
        tx_index: u32,
        tx: &Transaction,
        mut carriers: Vec<PositionedMessage>,
        wallets: &WalletSet,
        events: &mut Vec<ProtocolEvent>,
        dispositions: &mut Vec<Disposition>,
    ) -> Result<(), FinalizeFailure> {
        if carriers.is_empty() {
            return Ok(());
        }
        carriers.sort_by_key(|carrier| carrier.location);
        let ordinal = EventOrdinal {
            tx_index,
            location: carriers[0].location,
        };
        let mut receivers = BTreeSet::new();
        for carrier in &carriers {
            if let FullInscriptionMessage::L1ToL2Message(message) = &carrier.message {
                receivers.insert(message.input.receiver_l2_address.0);
            }
        }
        if receivers.len() != 1 {
            dispositions.push(rejection(
                tx_index,
                ordinal.location,
                RejectionCode::ConflictingReceiverEncodings,
                "deposit encodings name different receivers",
            ));
            return Ok(());
        }

        let inscription = carriers
            .iter()
            .find(|carrier| matches!(carrier.location, MessageLocation::Input(_)));
        let output = carriers
            .iter()
            .find(|carrier| matches!(carrier.location, MessageLocation::Output(_)));
        let Some(selected) = inscription.or(output) else {
            return Err(FinalizeFailure::Infrastructure(
                "deposit carriers have no physical location".into(),
            ));
        };
        let FullInscriptionMessage::L1ToL2Message(message) = &selected.message else {
            return Err(FinalizeFailure::Infrastructure(
                "non-deposit message entered deposit aggregation".into(),
            ));
        };
        let encoding = match (inscription.is_some(), output.is_some()) {
            (true, true) => DepositEncoding::Both,
            (true, false) => DepositEncoding::Inscription,
            (false, true) => DepositEncoding::OpReturn,
            (false, false) => {
                return Err(FinalizeFailure::Infrastructure(
                    "deposit carriers have no supported encoding".into(),
                ));
            }
        };
        let txid = tx.compute_txid();
        for (vout, txout) in tx.output.iter().enumerate() {
            if wallets.bridge.is_empty() || txout.script_pubkey != wallets.bridge {
                continue;
            }
            events.push(ProtocolEvent::DepositObserved(DepositObserved {
                ordinal,
                subject: OutPoint {
                    txid,
                    vout: vout as u32,
                },
                amount: txout.value,
                receiver: message.input.receiver_l2_address.0,
                l2_contract: message.input.l2_contract_address.0,
                call_data: message.input.call_data.clone(),
                sender_script: selected.signer_script.clone(),
                encoding,
            }));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn process_candidate(
        &self,
        candidate: Candidate,
        envelope: &BitcoinBlockEnvelope,
        context: &ProtocolContext,
        current: &CurrentTransactions,
        dependencies: &BTreeMap<DependencyKey, ResolvedDependency>,
        wallets: Option<&SystemWallets>,
        bootstrap_seen: &mut bool,
        rotations: &mut BTreeSet<RotationRole>,
        upgrade_version: &mut ProtocolVersionTag,
        events: &mut Vec<ProtocolEvent>,
        dispositions: &mut Vec<Disposition>,
    ) -> Result<(), FinalizeFailure> {
        let Candidate {
            tx_index,
            txid,
            positioned,
            input_roles,
        } = candidate;
        let ordinal = EventOrdinal {
            tx_index,
            location: positioned.location,
        };
        let unauthorized = |detail: &str, dispositions: &mut Vec<Disposition>| {
            dispositions.push(rejection(
                tx_index,
                positioned.location,
                RejectionCode::Unauthorized,
                detail,
            ));
        };
        let sequencer_signed =
            positioned.signer_script.as_ref() == Some(&context.wallets.sequencer);
        let verifier_signed = positioned
            .signer_script
            .as_ref()
            .is_some_and(|script| context.wallets.verifiers.contains(script));
        let governance_input = input_roles.contains(&TrackedRole::Governance);
        let bridge_input = input_roles.contains(&TrackedRole::Bridge);

        match positioned.message {
            FullInscriptionMessage::L1BatchDAReference(message) => {
                if !sequencer_signed {
                    unauthorized(
                        "batch DA reference is not signed by the sequencer",
                        dispositions,
                    );
                } else {
                    events.push(ProtocolEvent::L1BatchDAReference(
                        L1BatchDAReferenceObserved {
                            ordinal,
                            subject_txid: txid,
                            l1_batch_hash: message.input.l1_batch_hash.0,
                            l1_batch_index: u64::from(message.input.l1_batch_index.0),
                            da_identifier: message.input.da_identifier,
                            blob_id: message.input.blob_id,
                            prev_l1_batch_hash: message.input.prev_l1_batch_hash.0,
                        },
                    ));
                }
            }
            FullInscriptionMessage::ProofDAReference(message) => {
                if !sequencer_signed {
                    unauthorized(
                        "proof DA reference is not signed by the sequencer",
                        dispositions,
                    );
                } else if let Some(batch) = self.batch_snapshot(
                    message.input.l1_batch_reveal_txid,
                    envelope,
                    current,
                    dependencies,
                    wallets,
                )? {
                    events.push(ProtocolEvent::ProofDAReference(ProofDAReferenceObserved {
                        ordinal,
                        subject_txid: txid,
                        da_identifier: message.input.da_identifier,
                        blob_id: message.input.blob_id,
                        batch,
                    }));
                } else {
                    dispositions.push(rejection(
                        tx_index,
                        positioned.location,
                        RejectionCode::MalformedMessage,
                        "proof references no decodable batch DA transaction",
                    ));
                }
            }
            FullInscriptionMessage::ValidatorAttestation(message) => {
                if !verifier_signed {
                    unauthorized(
                        "attestation signer is not in the verifier set",
                        dispositions,
                    );
                } else if let Some(batch) = self.proof_batch_snapshot(
                    message.input.reference_txid,
                    envelope,
                    current,
                    dependencies,
                    wallets,
                )? {
                    events.push(ProtocolEvent::ValidatorAttestation(
                        ValidatorAttestationObserved {
                            ordinal,
                            subject_txid: txid,
                            reference_txid: message.input.reference_txid,
                            ok: matches!(message.input.attestation, Vote::Ok),
                            attester_script: positioned.signer_script.clone().ok_or_else(|| {
                                FinalizeFailure::Infrastructure(
                                    "authorized attestation has no signer script".into(),
                                )
                            })?,
                            batch,
                        },
                    ));
                } else {
                    dispositions.push(rejection(
                        tx_index,
                        positioned.location,
                        RejectionCode::MalformedMessage,
                        "attestation references no decodable proof and batch chain",
                    ));
                }
            }
            FullInscriptionMessage::SystemBootstrapping(message) => {
                if context.wallets.is_bootstrapped() || *bootstrap_seen {
                    dispositions.push(rejection(
                        tx_index,
                        positioned.location,
                        RejectionCode::InvalidBootstrap,
                        "protocol wallets are already bootstrapped",
                    ));
                } else {
                    match bootstrap_event(message, ordinal, txid, self.network) {
                        Ok(event) => {
                            *bootstrap_seen = true;
                            events.push(ProtocolEvent::SystemBootstrapping(event));
                        }
                        Err(detail) => dispositions.push(rejection(
                            tx_index,
                            positioned.location,
                            RejectionCode::MalformedMessage,
                            detail,
                        )),
                    }
                }
            }
            FullInscriptionMessage::SystemContractUpgradeProposal(message) => {
                if !sequencer_signed {
                    unauthorized(
                        "upgrade proposal is not signed by the sequencer",
                        dispositions,
                    );
                } else {
                    events.push(ProtocolEvent::SystemContractUpgradeProposal(
                        SystemContractUpgradeProposalObserved {
                            ordinal,
                            subject_txid: txid,
                            proposal: upgrade_payload_from(message.input),
                        },
                    ));
                }
            }
            FullInscriptionMessage::BridgeWithdrawal(message) => {
                if !bridge_input {
                    unauthorized("withdrawal spends no bridge-tracked output", dispositions);
                } else {
                    match withdrawal_event(message, ordinal, txid) {
                        Ok(event) => events.push(ProtocolEvent::BridgeWithdrawal(event)),
                        Err(detail) => dispositions.push(rejection(
                            tx_index,
                            positioned.location,
                            RejectionCode::MalformedMessage,
                            detail,
                        )),
                    }
                }
            }
            FullInscriptionMessage::UpdateBridgeProposal(message) => {
                if !sequencer_signed {
                    unauthorized(
                        "bridge proposal is not signed by the sequencer",
                        dispositions,
                    );
                } else {
                    match update_bridge_proposal_event(message, ordinal, txid, self.network) {
                        Ok(event) => events.push(ProtocolEvent::UpdateBridgeProposal(event)),
                        Err(detail) => dispositions.push(rejection(
                            tx_index,
                            positioned.location,
                            RejectionCode::MalformedMessage,
                            detail,
                        )),
                    }
                }
            }
            FullInscriptionMessage::UpdateSequencer(message) => {
                if !governance_input {
                    unauthorized(
                        "sequencer rotation spends no governance-tracked output",
                        dispositions,
                    );
                } else if rotations.contains(&RotationRole::Sequencer) {
                    dispositions.push(rejection(
                        tx_index,
                        positioned.location,
                        RejectionCode::ConflictingRoleUpdate,
                        "a sequencer rotation is already accepted in this block",
                    ));
                } else {
                    match checked_address_script(message.input.address, self.network) {
                        Ok(new_script) => {
                            rotations.insert(RotationRole::Sequencer);
                            events.push(ProtocolEvent::WalletRotation(WalletRotationObserved {
                                ordinal,
                                subject_txid: txid,
                                rotation: WalletRotation::Sequencer { new_script },
                            }));
                        }
                        Err(detail) => dispositions.push(rejection(
                            tx_index,
                            positioned.location,
                            RejectionCode::MalformedMessage,
                            detail,
                        )),
                    }
                }
            }
            FullInscriptionMessage::UpdateGovernance(message) => {
                if !governance_input {
                    unauthorized(
                        "governance rotation spends no governance-tracked output",
                        dispositions,
                    );
                } else if rotations.contains(&RotationRole::Governance) {
                    dispositions.push(rejection(
                        tx_index,
                        positioned.location,
                        RejectionCode::ConflictingRoleUpdate,
                        "a governance rotation is already accepted in this block",
                    ));
                } else {
                    match checked_address_script(message.input.address, self.network) {
                        Ok(new_script) => {
                            rotations.insert(RotationRole::Governance);
                            events.push(ProtocolEvent::WalletRotation(WalletRotationObserved {
                                ordinal,
                                subject_txid: txid,
                                rotation: WalletRotation::Governance { new_script },
                            }));
                        }
                        Err(detail) => dispositions.push(rejection(
                            tx_index,
                            positioned.location,
                            RejectionCode::MalformedMessage,
                            detail,
                        )),
                    }
                }
            }
            FullInscriptionMessage::SystemContractUpgrade(message) => {
                if !governance_input {
                    unauthorized(
                        "upgrade activation spends no governance-tracked output",
                        dispositions,
                    );
                } else if let Some(proposal) = self.upgrade_payload(
                    message.input.proposal_tx_id,
                    envelope,
                    current,
                    dependencies,
                    wallets,
                )? {
                    if proposal.version <= *upgrade_version {
                        dispositions.push(rejection(
                            tx_index,
                            positioned.location,
                            RejectionCode::NonMonotonicUpgrade,
                            "upgrade version does not strictly increase the accepted block version",
                        ));
                    } else {
                        *upgrade_version = proposal.version;
                        events.push(ProtocolEvent::SystemContractUpgradeActivation(
                            SystemContractUpgradeActivationObserved {
                                ordinal,
                                subject_txid: txid,
                                proposal_txid: message.input.proposal_tx_id,
                                proposal,
                            },
                        ));
                    }
                } else {
                    dispositions.push(rejection(
                        tx_index,
                        positioned.location,
                        RejectionCode::InvalidProposal,
                        "upgrade activation does not reference an upgrade proposal",
                    ));
                }
            }
            FullInscriptionMessage::UpdateBridge(message) => {
                if !governance_input {
                    unauthorized(
                        "bridge rotation spends no governance-tracked output",
                        dispositions,
                    );
                } else if rotations.contains(&RotationRole::Bridge) {
                    dispositions.push(rejection(
                        tx_index,
                        positioned.location,
                        RejectionCode::ConflictingRoleUpdate,
                        "a bridge rotation is already accepted in this block",
                    ));
                } else if let Some((new_bridge_script, new_verifier_scripts)) = self
                    .bridge_proposal(
                        message.input.proposal_tx_id,
                        envelope,
                        current,
                        dependencies,
                        wallets,
                    )?
                {
                    rotations.insert(RotationRole::Bridge);
                    events.push(ProtocolEvent::WalletRotation(WalletRotationObserved {
                        ordinal,
                        subject_txid: txid,
                        rotation: WalletRotation::Bridge {
                            proposal_txid: message.input.proposal_tx_id,
                            new_bridge_script,
                            new_verifier_scripts,
                        },
                    }));
                } else {
                    dispositions.push(rejection(
                        tx_index,
                        positioned.location,
                        RejectionCode::InvalidProposal,
                        "bridge activation does not reference a bridge proposal",
                    ));
                }
            }
            FullInscriptionMessage::L1ToL2Message(_) => {
                return Err(FinalizeFailure::Infrastructure(
                    "deposit bypassed transaction-level aggregation".into(),
                ));
            }
        }
        Ok(())
    }
}

impl ProtocolEngine for ViaProtocolEngineV2 {
    type Draft = ViaProtocolDraftV2;

    fn inspect(
        &self,
        envelope: &BitcoinBlockEnvelope,
        context: &ProtocolContext,
    ) -> (Self::Draft, Vec<DependencyKey>) {
        let parsed = self
            .parser_wallets(context)
            .and_then(|wallets| self.parse_envelope(envelope, wallets.as_ref()))
            .unwrap_or_default();
        let keys = Self::initial_keys(envelope, &parsed);
        (
            ViaProtocolDraftV2 {
                envelope: envelope.clone(),
                context: context.clone(),
            },
            keys,
        )
    }

    fn finalize(
        &self,
        draft: Self::Draft,
        dependencies: &BTreeMap<DependencyKey, ResolvedDependency>,
    ) -> Result<FinalizeOutcome<Self::Draft>, FinalizeFailure> {
        validate_resolutions(dependencies)?;
        if draft.envelope.network != self.network {
            return Err(FinalizeFailure::Infrastructure(format!(
                "engine network {:?} does not match envelope network {:?}",
                self.network, draft.envelope.network
            )));
        }
        let wallets = self.parser_wallets(&draft.context)?;
        let parsed = self.parse_envelope(&draft.envelope, wallets.as_ref())?;
        let initial_keys = Self::initial_keys(&draft.envelope, &parsed);
        let missing: Vec<_> = initial_keys
            .iter()
            .filter(|key| !dependencies.contains_key(*key))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Ok(FinalizeOutcome::NeedDependencies {
                draft,
                keys: missing,
            });
        }
        let unavailable: Vec<_> = initial_keys
            .iter()
            .filter(|key| {
                matches!(
                    (key, dependencies.get(*key)),
                    (
                        DependencyKey::RawTx(_),
                        Some(ResolvedDependency::RawTx(Resolution::Unavailable))
                    ) | (
                        DependencyKey::TrackedOutput(_),
                        Some(ResolvedDependency::TrackedOutput(Resolution::Unavailable))
                    )
                )
            })
            .cloned()
            .collect();
        if !unavailable.is_empty() {
            return Err(FinalizeFailure::MissingDependency(unavailable));
        }

        let current = Self::current_transactions(&draft.envelope);
        let nested = self.nested_dependency_keys(
            &draft.envelope,
            &parsed,
            &current,
            dependencies,
            wallets.as_ref(),
        )?;
        if !nested.is_empty() {
            return Ok(FinalizeOutcome::NeedDependencies {
                draft,
                keys: nested,
            });
        }
        let plan = self.finalize_plan(draft, parsed, dependencies, wallets.as_ref())?;
        Ok(FinalizeOutcome::Complete(Box::new(plan)))
    }
}

fn tracked_role_for_script(script: &ScriptBuf, wallets: &WalletSet) -> Option<TrackedRole> {
    if !wallets.bridge.is_empty() && script == &wallets.bridge {
        Some(TrackedRole::Bridge)
    } else if !wallets.governance.is_empty() && script == &wallets.governance {
        Some(TrackedRole::Governance)
    } else {
        None
    }
}

fn rejection(
    tx_index: u32,
    location: MessageLocation,
    code: RejectionCode,
    detail: impl Into<String>,
) -> Disposition {
    Disposition {
        ordinal: EventOrdinal { tx_index, location },
        kind: DispositionKind::RejectedInvalid {
            code,
            detail: detail.into(),
        },
    }
}

fn checked_address_script(
    address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
    network: Network,
) -> Result<ScriptBuf, String> {
    address
        .require_network(network)
        .map(|address| address.script_pubkey())
        .map_err(|err| format!("address belongs to the wrong Bitcoin network: {err}"))
}

fn protocol_version(
    version: zksync_types::protocol_version::ProtocolSemanticVersion,
) -> ProtocolVersionTag {
    ProtocolVersionTag {
        minor: u32::from(version.minor as u16),
        patch: version.patch.0,
    }
}

fn upgrade_payload_from(input: crate::types::SystemContractUpgradeProposalInput) -> UpgradePayload {
    UpgradePayload {
        version: protocol_version(input.version),
        bootloader_code_hash: input.bootloader_code_hash.0,
        default_account_code_hash: input.default_account_code_hash.0,
        evm_emulator_code_hash: input.evm_emulator_code_hash.map(|hash| hash.0),
        recursion_scheduler_level_vk_hash: input.recursion_scheduler_level_vk_hash.0,
        system_contracts: input
            .system_contracts
            .into_iter()
            .map(|(address, hash)| (address.0, hash.0))
            .collect(),
    }
}

fn bootstrap_event(
    message: crate::types::SystemBootstrapping,
    ordinal: EventOrdinal,
    subject_txid: Txid,
    network: Network,
) -> Result<SystemBootstrappingObserved, String> {
    let input = message.input;
    let wallets = WalletSet {
        sequencer: checked_address_script(input.sequencer_address, network)?,
        bridge: checked_address_script(input.bridge_musig2_address, network)?,
        governance: checked_address_script(input.governance_address, network)?,
        verifiers: input
            .verifier_p2wpkh_addresses
            .into_iter()
            .map(|address| checked_address_script(address, network))
            .collect::<Result<_, _>>()?,
    };
    wallets.validate()?;
    Ok(SystemBootstrappingObserved {
        ordinal,
        subject_txid,
        start_block_height: u64::from(input.start_block_height),
        protocol_version: protocol_version(input.protocol_version),
        bootloader_hash: input.bootloader_hash.0,
        abstract_account_hash: input.abstract_account_hash.0,
        snark_wrapper_vk_hash: input.snark_wrapper_vk_hash.0,
        evm_emulator_hash: input.evm_emulator_hash.0,
        wallets,
    })
}

fn update_bridge_proposal_event(
    message: crate::types::UpdateBridgeProposal,
    ordinal: EventOrdinal,
    subject_txid: Txid,
    network: Network,
) -> Result<UpdateBridgeProposalObserved, String> {
    Ok(UpdateBridgeProposalObserved {
        ordinal,
        subject_txid,
        bridge_script: checked_address_script(message.input.bridge_musig2_address, network)?,
        verifier_scripts: message
            .input
            .verifier_p2wpkh_addresses
            .into_iter()
            .map(|address| checked_address_script(address, network))
            .collect::<Result<_, _>>()?,
    })
}

fn withdrawal_event(
    message: crate::types::BridgeWithdrawal,
    ordinal: EventOrdinal,
    subject_txid: Txid,
) -> Result<BridgeWithdrawalObserved, String> {
    let input = message.input;
    let withdrawal_version = match input.version {
        WithdrawalVersion::Version0 => 0,
    };
    let total_size =
        u64::try_from(input.total_size).map_err(|_| "withdrawal total size is negative")?;
    let v_size = u64::try_from(input.v_size).map_err(|_| "withdrawal virtual size is negative")?;
    let withdrawals = input
        .withdrawals
        .into_iter()
        .map(|withdrawal| {
            let l2_id = hex::decode(&withdrawal.l2_meta.l2_id)
                .map_err(|err| format!("withdrawal L2 id is not hex: {err}"))?
                .try_into()
                .map_err(|_| "withdrawal L2 id is not 8 bytes".to_string())?;
            Ok(WithdrawalOutput {
                l2_id,
                l2_tx_event_index: withdrawal.l2_meta.l2_tx_event_index,
                receiver_script: withdrawal.receiver.script_pubkey(),
                amount: withdrawal.value,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(BridgeWithdrawalObserved {
        ordinal,
        subject_txid,
        withdrawal_version,
        total_size,
        v_size,
        inputs: input.inputs,
        output_amount: bitcoin::Amount::from_sat(input.output_amount),
        withdrawals,
    })
}

#[cfg(test)]
mod tests {
    use bitcoin::{
        hashes::Hash, Amount, BlockHash, Network, OutPoint, ScriptBuf, TxOut, Txid, Witness,
    };
    use via_btc_ingestion::{
        BitcoinBlockEnvelope, BlockAnchor, CanonicalObservedTx, DepositEncoding, DispositionKind,
        FinalizeOutcome, ProtocolEvent, ProtocolVersionTag, Resolution, TrackedRole,
    };
    use zksync_types::Address as EvmAddress;

    use super::*;
    use crate::{
        test_message_encoder::{
            external_input, inscribed_tx, op_return, p2wpkh, rotation_tx, seeded_outpoint,
            transaction, TestMessageEncoder, SEQUENCER_SEED,
        },
        types::{InscriptionMessage, L1ToL2MessageInput},
    };

    const HEIGHT: u64 = 100;

    fn context() -> ProtocolContext {
        ProtocolContext {
            version: 1,
            wallets: TestMessageEncoder::new(Network::Regtest).wallet_set(),
            protocol_version: ProtocolVersionTag {
                minor: 26,
                patch: 0,
            },
        }
    }

    fn envelope(transactions: Vec<Transaction>) -> BitcoinBlockEnvelope {
        BitcoinBlockEnvelope {
            network: Network::Regtest,
            anchor: BlockAnchor {
                height: HEIGHT,
                hash: BlockHash::from_byte_array([0xA0; 32]),
                prev_hash: BlockHash::from_byte_array([0x9F; 32]),
                time: 1_700_000_000,
            },
            transactions,
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

    fn plan_with_absent(
        engine: &ViaProtocolEngineV2,
        envelope: &BitcoinBlockEnvelope,
        context: &ProtocolContext,
    ) -> BlockPlan {
        let (mut draft, keys) = engine.inspect(envelope, context);
        let mut dependencies: BTreeMap<_, _> =
            keys.iter().map(|key| (key.clone(), absent(key))).collect();
        for _ in 0..4 {
            match engine.finalize(draft, &dependencies).expect("finalize") {
                FinalizeOutcome::Complete(plan) => return *plan,
                FinalizeOutcome::NeedDependencies { draft: next, keys } => {
                    dependencies.extend(keys.iter().map(|key| (key.clone(), absent(key))));
                    draft = next;
                }
            }
        }
        panic!("finalization did not converge")
    }

    fn deposit_message(
        receiver: [u8; 20],
        contract: [u8; 20],
        call_data: Vec<u8>,
    ) -> InscriptionMessage {
        InscriptionMessage::L1ToL2Message(L1ToL2MessageInput {
            receiver_l2_address: EvmAddress::from(receiver),
            l2_contract_address: EvmAddress::from(contract),
            call_data,
        })
    }

    #[test]
    fn agreeing_op_returns_collapse_to_lowest_output_location() {
        let engine = ViaProtocolEngineV2::new(Network::Regtest);
        let context = context();
        let tx = transaction(
            vec![external_input(seeded_outpoint(1), Witness::new())],
            vec![
                TxOut {
                    value: Amount::from_sat(50_000),
                    script_pubkey: context.wallets.bridge.clone(),
                },
                op_return(vec![0xAA; 20]),
                op_return(vec![0xAA; 20]),
            ],
        );
        let plan = plan_with_absent(&engine, &envelope(vec![tx]), &context);
        let [ProtocolEvent::DepositObserved(deposit)] = &plan.events[..] else {
            panic!("expected one collapsed deposit event")
        };
        assert_eq!(deposit.ordinal.location, MessageLocation::Output(1));
        assert_eq!(deposit.encoding, DepositEncoding::OpReturn);
        assert!(plan.dispositions.is_empty());
    }

    #[test]
    fn agreeing_inscription_and_op_return_use_inscription_payload_and_location() {
        let engine = ViaProtocolEngineV2::new(Network::Regtest);
        let context = context();
        let tx = inscribed_tx(
            seeded_outpoint(2),
            SEQUENCER_SEED,
            92,
            deposit_message([0xAA; 20], [0xCC; 20], vec![1, 2, 3]),
            vec![
                TxOut {
                    value: Amount::from_sat(50_000),
                    script_pubkey: context.wallets.bridge.clone(),
                },
                op_return(vec![0xAA; 20]),
            ],
            Network::Regtest,
        );
        let plan = plan_with_absent(&engine, &envelope(vec![tx]), &context);
        let [ProtocolEvent::DepositObserved(deposit)] = &plan.events[..] else {
            panic!("expected one deposit event")
        };
        assert_eq!(deposit.ordinal.location, MessageLocation::Input(1));
        assert_eq!(deposit.encoding, DepositEncoding::Both);
        assert_eq!(deposit.l2_contract, [0xCC; 20]);
        assert_eq!(deposit.call_data, vec![1, 2, 3]);
    }

    #[test]
    fn context_required_message_is_rejected_as_unauthorized() {
        let engine = ViaProtocolEngineV2::new(Network::Regtest);
        let context = ProtocolContext {
            version: 1,
            wallets: WalletSet {
                sequencer: ScriptBuf::new(),
                bridge: ScriptBuf::new(),
                governance: ScriptBuf::new(),
                verifiers: vec![],
            },
            protocol_version: ProtocolVersionTag { minor: 0, patch: 0 },
        };
        let tx = inscribed_tx(
            seeded_outpoint(3),
            SEQUENCER_SEED,
            93,
            deposit_message([0xAA; 20], [0; 20], vec![]),
            vec![],
            Network::Regtest,
        );
        let plan = plan_with_absent(&engine, &envelope(vec![tx]), &context);
        assert!(plan.events.is_empty());
        assert!(matches!(
            &plan.dispositions[..],
            [Disposition {
                ordinal: EventOrdinal {
                    location: MessageLocation::Input(1),
                    ..
                },
                kind: DispositionKind::RejectedInvalid {
                    code: RejectionCode::Unauthorized,
                    ..
                },
            }]
        ));
    }

    #[test]
    fn unauthorized_rotation_does_not_consume_the_role_quota() {
        let engine = ViaProtocolEngineV2::new(Network::Regtest);
        let context = context();
        let funding = transaction(
            vec![external_input(seeded_outpoint(4), Witness::new())],
            vec![TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey: context.wallets.governance.clone(),
            }],
        );
        let governance_outpoint = OutPoint {
            txid: funding.compute_txid(),
            vout: 0,
        };
        let first_script = p2wpkh(77, Network::Regtest).0;
        let second_script = p2wpkh(78, Network::Regtest).0;
        let unauthorized = rotation_tx(
            seeded_outpoint(5),
            b"VIA_PROTOCOL:SEQ",
            first_script.to_string().as_bytes(),
        );
        let authorized = rotation_tx(
            governance_outpoint,
            b"VIA_PROTOCOL:SEQ",
            second_script.to_string().as_bytes(),
        );
        let plan = plan_with_absent(
            &engine,
            &envelope(vec![funding, unauthorized, authorized]),
            &context,
        );
        let rotations: Vec<_> = plan
            .events
            .iter()
            .filter_map(|event| match event {
                ProtocolEvent::WalletRotation(rotation) => Some(rotation),
                _ => None,
            })
            .collect();
        assert_eq!(rotations.len(), 1);
        assert!(matches!(
            &rotations[0].rotation,
            WalletRotation::Sequencer { new_script } if new_script == &second_script.script_pubkey()
        ));
        assert!(plan.dispositions.iter().any(|disposition| matches!(
            disposition.kind,
            DispositionKind::RejectedInvalid {
                code: RejectionCode::Unauthorized,
                ..
            }
        )));
        assert!(!plan.dispositions.iter().any(|disposition| matches!(
            disposition.kind,
            DispositionKind::RejectedInvalid {
                code: RejectionCode::ConflictingRoleUpdate,
                ..
            }
        )));
    }

    #[test]
    fn known_absent_reference_is_invalid_but_unavailable_reference_halts() {
        let engine = ViaProtocolEngineV2::new(Network::Regtest);
        let context = context();
        let encoder = TestMessageEncoder::new(Network::Regtest);
        let missing = Txid::from_byte_array([0xD1; 32]);
        let proof = encoder.proof_da_reference_tx(missing, "proof", seeded_outpoint(6));
        let proof_envelope = envelope(vec![proof]);
        let plan = plan_with_absent(&engine, &proof_envelope, &context);
        assert!(plan.events.is_empty());
        assert!(matches!(
            plan.dispositions[0].kind,
            DispositionKind::RejectedInvalid {
                code: RejectionCode::MalformedMessage,
                ..
            }
        ));

        let unauthorized = encoder.unauthorized_attestation_tx(missing, seeded_outpoint(7));
        let unavailable_envelope = envelope(vec![unauthorized]);
        let (draft, keys) = engine.inspect(&unavailable_envelope, &context);
        let dependencies = keys
            .iter()
            .map(|key| {
                let value = match key {
                    DependencyKey::RawTx(_) => ResolvedDependency::RawTx(Resolution::Unavailable),
                    DependencyKey::TrackedOutput(_) => absent(key),
                };
                (key.clone(), value)
            })
            .collect();
        assert!(matches!(
            engine.finalize(draft, &dependencies),
            Err(FinalizeFailure::MissingDependency(_))
        ));
    }

    #[test]
    fn historical_proposal_is_parsed_structurally_not_reauthorized_against_current_wallets() {
        let engine = ViaProtocolEngineV2::new(Network::Regtest);
        let encoder = TestMessageEncoder::new(Network::Regtest);
        let mut context = context();
        context.wallets.sequencer = p2wpkh(88, Network::Regtest).0.script_pubkey();

        let proposal = encoder.upgrade_proposal_tx(
            ProtocolVersionTag {
                minor: 27,
                patch: 0,
            },
            seeded_outpoint(8),
        );
        let proposal_variant = RawTxVariant::from_transaction(&proposal);
        let proposal_txid = proposal_variant.txid();
        let proposal_observation = CanonicalObservedTx::new(
            Inclusion {
                block_hash: BlockHash::from_byte_array([0x81; 32]),
                height: HEIGHT - 1,
                tx_index: 3,
                txid: proposal_txid,
                wtxid: proposal_variant.wtxid(),
            },
            proposal_variant,
        )
        .unwrap();
        let governance_outpoint = seeded_outpoint(9);
        let activation = encoder.upgrade_activation_tx(proposal_txid, governance_outpoint);
        let activation_envelope = envelope(vec![activation]);
        let (draft, keys) = engine.inspect(&activation_envelope, &context);
        let dependencies = keys
            .into_iter()
            .map(|key| {
                let value = match key {
                    DependencyKey::RawTx(txid) if txid == proposal_txid => {
                        ResolvedDependency::RawTx(Resolution::Present(proposal_observation.clone()))
                    }
                    DependencyKey::TrackedOutput(outpoint) if outpoint == governance_outpoint => {
                        ResolvedDependency::TrackedOutput(Resolution::Present(
                            TrackedOutputCreate {
                                outpoint,
                                value: Amount::from_sat(10_000),
                                script_pubkey: context.wallets.governance.clone(),
                                role: TrackedRole::Governance,
                            },
                        ))
                    }
                    _ => absent(&key),
                };
                (key, value)
            })
            .collect();
        let FinalizeOutcome::Complete(plan) = engine.finalize(draft, &dependencies).unwrap() else {
            panic!("activation should finalize in one round")
        };
        assert!(matches!(
            &plan.events[..],
            [ProtocolEvent::SystemContractUpgradeActivation(event)]
                if event.proposal.version == ProtocolVersionTag { minor: 27, patch: 0 }
        ));
    }

    #[test]
    fn same_block_reference_can_resolve_a_later_transaction() {
        let engine = ViaProtocolEngineV2::new(Network::Regtest);
        let context = context();
        let encoder = TestMessageEncoder::new(Network::Regtest);
        let batch = encoder.batch_da_reference_tx(7, [3; 32], "batch", seeded_outpoint(11));
        let batch_txid = batch.compute_txid();
        let proof = encoder.proof_da_reference_tx(batch_txid, "proof", seeded_outpoint(12));
        let plan = plan_with_absent(&engine, &envelope(vec![proof, batch]), &context);
        assert!(plan.events.iter().any(|event| matches!(
            event,
            ProtocolEvent::ProofDAReference(proof) if proof.batch.reveal_txid == batch_txid
        )));
    }

    #[test]
    fn first_bootstrap_does_not_require_a_preexisting_authorized_signer() {
        let engine = ViaProtocolEngineV2::new(Network::Regtest);
        let encoder = TestMessageEncoder::new(Network::Regtest);
        let wallets = encoder.wallet_set();
        let context = ProtocolContext {
            version: 1,
            wallets: WalletSet {
                sequencer: ScriptBuf::new(),
                bridge: ScriptBuf::new(),
                governance: ScriptBuf::new(),
                verifiers: vec![],
            },
            protocol_version: ProtocolVersionTag { minor: 0, patch: 0 },
        };
        let mut bootstrap = encoder.bootstrap_tx(
            &wallets,
            ProtocolVersionTag {
                minor: 26,
                patch: 0,
            },
            seeded_outpoint(13),
        );
        bootstrap.input[0].witness = p2wpkh(99, Network::Regtest).1;
        let plan = plan_with_absent(&engine, &envelope(vec![bootstrap]), &context);
        assert!(matches!(
            &plan.events[..],
            [ProtocolEvent::SystemBootstrapping(event)] if event.wallets == wallets
        ));
    }

    #[test]
    fn bridge_role_wins_when_tracked_scripts_overlap() {
        let engine = ViaProtocolEngineV2::new(Network::Regtest);
        let mut context = context();
        context.wallets.governance = context.wallets.bridge.clone();
        let funding = transaction(
            vec![external_input(seeded_outpoint(10), Witness::new())],
            vec![TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey: context.wallets.bridge.clone(),
            }],
        );
        let plan = plan_with_absent(&engine, &envelope(vec![funding]), &context);
        assert_eq!(plan.tracked_creates.len(), 1);
        assert_eq!(plan.tracked_creates[0].role, TrackedRole::Bridge);
    }
}
