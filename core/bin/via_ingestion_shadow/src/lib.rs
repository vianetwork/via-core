use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, bail, Context as _, Result};
use bitcoin::{address::NetworkUnchecked, Address, Network, ScriptBuf, Transaction, TxOut, Txid};
use clap::{Parser, ValueEnum};
use serde::Serialize;
use sqlx::{postgres::PgPoolOptions, PgPool};
use via_btc_client::{
    client::BitcoinClient,
    indexer::BitcoinInscriptionIndexer,
    ingestion_engine::ViaProtocolEngine,
    traits::BitcoinOps,
    types::{CommonFields, FullInscriptionMessage, NodeAuth, Vote},
};
use via_btc_ingestion::{
    AggregateAdapter, BitcoinBlockEnvelope, BlockAnchor, BlockPlan, CoverageReport, DependencyKey, DispositionKind,
    EventKind, FinalizeOutcome, ObservationReader, ProtocolContext, ProtocolEngine, ProtocolEvent, RejectionCode,
    Resolution, ResolvedDependency, Role, WalletRotation,
};
use via_ingestion_adapter::{PgIngestionAdapter, PgObservationReader};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;
use zksync_types::via_wallet::SystemWallets;

const MAX_FINALIZE_ROUNDS: usize = 4;

#[derive(Parser)]
#[command(name = "via_ingestion_shadow")]
#[command(about = "Replay Bitcoin blocks through the Via ingestion kernel and optionally compare the legacy indexer")]
pub struct Cli {
    #[arg(long, env = "VIA_BTC_CLIENT_RPC_URL", hide_env_values = true)]
    rpc_url: String,
    #[arg(long, env = "VIA_BTC_CLIENT_RPC_USER", hide_env_values = true)]
    rpc_user: String,
    #[arg(long, env = "VIA_BTC_CLIENT_RPC_PASSWORD", hide_env_values = true)]
    rpc_password: String,
    #[arg(long, value_enum)]
    network: NetworkArg,
    #[arg(long)]
    database_url: String,
    #[arg(long, value_enum, default_value_t = RoleArg::Core)]
    role: RoleArg,
    #[arg(long)]
    from_height: u64,
    #[arg(long)]
    to_height: u64,
    #[arg(long, value_enum)]
    mode: Mode,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    bootstrap_context_json: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum NetworkArg {
    Bitcoin,
    Testnet,
    Testnet4,
    Regtest,
}

impl From<NetworkArg> for Network {
    fn from(value: NetworkArg) -> Self {
        match value {
            NetworkArg::Bitcoin => Network::Bitcoin,
            NetworkArg::Testnet => Network::Testnet,
            NetworkArg::Testnet4 => Network::Testnet4,
            NetworkArg::Regtest => Network::Regtest,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum RoleArg {
    Core,
    Verifier,
    Indexer,
}

impl From<RoleArg> for Role {
    fn from(value: RoleArg) -> Self {
        match value {
            RoleArg::Core => Role::CoreSequencer,
            RoleArg::Verifier => Role::Verifier,
            RoleArg::Indexer => Role::StandaloneIndexer,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Replay,
    Shadow,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Replay => "replay",
            Self::Shadow => "shadow",
        }
    }
}

pub struct RunConfig {
    pub client: Arc<BitcoinClient>,
    pub network: Network,
    pub database_url: String,
    pub role: Role,
    pub from_height: u64,
    pub to_height: u64,
    pub mode: Mode,
    pub out: Option<PathBuf>,
    pub bootstrap_context: Option<ProtocolContext>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Fact {
    pub kind: String,
    pub txid: String,
    pub key_fields: FactFields,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum FactFields {
    Deposit {
        amount_sat: u64,
        receiver: Vec<u8>,
    },
    BatchReference {
        batch_index: u64,
        blob_id: String,
    },
    ProofReference {
        reveal_txid: String,
        blob_id: String,
    },
    Attestation {
        reference_txid: String,
        vote: bool,
    },
    Withdrawal {
        receiver_script: Vec<u8>,
        amount_sat: u64,
    },
    Bootstrap {
        sequencer_script: Vec<u8>,
        bridge_script: Vec<u8>,
        governance_script: Vec<u8>,
        verifier_scripts: Vec<Vec<u8>>,
    },
    UpgradeProposal {
        contract_addresses: Vec<Vec<u8>>,
    },
    ProposalReference {
        proposal_txid: String,
    },
    BridgeProposal {
        bridge_script: Vec<u8>,
        verifier_scripts: Vec<Vec<u8>>,
    },
    WalletAddress {
        address_script: Vec<u8>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeltaSide {
    KernelOnly,
    LegacyOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeltaRecord {
    pub height: u64,
    pub side: DeltaSide,
    pub fact: Fact,
}

#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub record: &'static str,
    pub mode: &'static str,
    pub role: &'static str,
    pub from_height: u64,
    pub to_height: u64,
    pub blocks_applied: u64,
    pub events_per_kind: BTreeMap<String, u64>,
    pub rejections_per_code: BTreeMap<String, u64>,
    pub kernel_only: u64,
    pub legacy_only: u64,
    pub coverage: CoverageReport,
}

pub async fn run_cli(cli: Cli) -> Result<RunSummary> {
    let network = Network::from(cli.network);
    let bootstrap_context = cli.bootstrap_context_json.as_deref().map(read_protocol_context).transpose()?;
    let client = Arc::new(
        BitcoinClient::new(
            &cli.rpc_url,
            NodeAuth::UserPass(cli.rpc_user, cli.rpc_password),
            ViaBtcClientConfig {
                network: network.to_string(),
                external_apis: vec![],
                fee_strategies: vec![],
                use_rpc_for_fee_rate: Some(true),
            },
        )
        .context("construct Bitcoin RPC client")?,
    );

    execute(RunConfig {
        client,
        network,
        database_url: cli.database_url,
        role: cli.role.into(),
        from_height: cli.from_height,
        to_height: cli.to_height,
        mode: cli.mode,
        out: cli.out,
        bootstrap_context,
    })
    .await
}

fn read_protocol_context(path: &Path) -> Result<ProtocolContext> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read bootstrap protocol context from {}", path.display()))?;
    let context: ProtocolContext = serde_json::from_slice(&bytes)
        .with_context(|| format!("decode bootstrap protocol context from {}", path.display()))?;
    context.wallets.validate().map_err(|err| anyhow!("invalid bootstrap protocol context: {err}"))?;
    Ok(context)
}

pub async fn execute(config: RunConfig) -> Result<RunSummary> {
    if config.from_height > config.to_height {
        bail!("invalid range: from height {} exceeds to height {}", config.from_height, config.to_height);
    }
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database_url)
        .await
        .context("connect to shadow database")?;
    let result = execute_with_pool(&config, pool.clone()).await;
    pool.close().await;
    result
}

async fn execute_with_pool(config: &RunConfig, pool: PgPool) -> Result<RunSummary> {
    let adapter = PgIngestionAdapter::new(pool.clone(), config.role);
    let reader = PgObservationReader::new(pool);
    let engine = ViaProtocolEngine::new(config.network);
    match adapter.load_checkpoint_and_context().await.context("validate replay starting checkpoint")? {
        Some((checkpoint, _)) => {
            let expected =
                checkpoint.height.checked_add(1).ok_or_else(|| anyhow!("checkpoint height cannot be incremented"))?;
            if config.from_height != expected {
                bail!("replay must start at checkpoint height + 1: expected {expected}, got {}", config.from_height);
            }
        }
        None => {
            let context = config
                .bootstrap_context
                .as_ref()
                .ok_or_else(|| anyhow!("shadow store is empty; --bootstrap-context-json is required"))?;
            context.wallets.validate().map_err(|err| anyhow!("invalid bootstrap protocol context: {err}"))?;
        }
    }
    let mut output = OutputSink::new(config.out.as_deref())?;
    let mut blocks_applied = 0;
    let mut events_per_kind = BTreeMap::new();
    let mut rejections_per_code = BTreeMap::new();
    let mut kernel_only = 0;
    let mut legacy_only = 0;

    for height in config.from_height..=config.to_height {
        let block = config
            .client
            .fetch_block(u128::from(height))
            .await
            .with_context(|| format!("height {height}: fetch Bitcoin block"))?;
        let envelope = BitcoinBlockEnvelope {
            network: config.network,
            anchor: BlockAnchor {
                height,
                hash: block.block_hash(),
                prev_hash: block.header.prev_blockhash,
                time: block.header.time,
            },
            transactions: block.txdata,
        };
        let stored = adapter
            .load_checkpoint_and_context()
            .await
            .with_context(|| format!("height {height}: load checkpoint and protocol context"))?;
        let (checkpoint, context) = match stored {
            Some((checkpoint, context)) => (Some(checkpoint), context),
            None => (
                None,
                config.bootstrap_context.clone().ok_or_else(|| {
                    anyhow!("height {height}: shadow store is empty; --bootstrap-context-json is required")
                })?,
            ),
        };

        let first_plan = finalize_plan(&engine, &reader, &envelope, &context)
            .await
            .with_context(|| format!("height {height}: first kernel inspect/finalize run"))?;
        let second_plan = finalize_plan(&engine, &reader, &envelope, &context)
            .await
            .with_context(|| format!("height {height}: determinism inspect/finalize run"))?;
        let first_hash = first_plan.plan_hash().with_context(|| format!("height {height}: hash first kernel plan"))?;
        let second_hash =
            second_plan.plan_hash().with_context(|| format!("height {height}: hash determinism kernel plan"))?;
        if first_hash != second_hash {
            bail!(
                "height {height}: nondeterministic kernel plan hashes: {} != {}",
                hash_hex(&first_hash),
                hash_hex(&second_hash)
            );
        }

        let mut block_deltas = Vec::new();
        if config.mode == Mode::Shadow {
            let legacy_context = legacy_comparison_context(&context, &first_plan.next_context);
            let wallets = Arc::new(
                system_wallets_from_context(legacy_context, config.network)
                    .with_context(|| format!("height {height}: derive legacy system wallets from kernel context"))?,
            );
            let legacy_height =
                u32::try_from(height).with_context(|| format!("height {height}: legacy indexer height exceeds u32"))?;
            let mut legacy_indexer = BitcoinInscriptionIndexer::new(config.client.clone(), wallets);
            let legacy_messages = legacy_indexer
                .process_block(legacy_height)
                .await
                .with_context(|| format!("height {height}: legacy process_block"))?;
            let kernel_facts = normalize_kernel_events(&first_plan.events);
            let legacy_facts =
                normalize_legacy_messages(&legacy_messages, &envelope.transactions, legacy_context, config.network)
                    .with_context(|| format!("height {height}: normalize legacy messages"))?;
            let (kernel_deltas, legacy_deltas) = diff_fact_sets(&kernel_facts, &legacy_facts);
            for fact in kernel_deltas {
                block_deltas.push(DeltaRecord { height, side: DeltaSide::KernelOnly, fact });
            }
            for fact in legacy_deltas {
                block_deltas.push(DeltaRecord { height, side: DeltaSide::LegacyOnly, fact });
            }
        }

        let receipt = adapter
            .apply_block(checkpoint, &first_plan)
            .await
            .with_context(|| format!("height {height}: apply kernel block plan"))?;
        receipt.validate(&first_plan).map_err(|err| anyhow!("height {height}: invalid projection receipt: {err}"))?;
        for delta in block_deltas {
            match &delta.side {
                DeltaSide::KernelOnly => kernel_only += 1,
                DeltaSide::LegacyOnly => legacy_only += 1,
            }
            print_delta(&delta)?;
            output.write_delta(&delta)?;
        }
        blocks_applied += 1;
        count_plan(&first_plan, &mut events_per_kind, &mut rejections_per_code);
    }

    let coverage =
        adapter.audit_coverage(config.from_height, config.to_height).await.context("audit replay coverage")?;
    if !coverage.contiguous || !coverage.unresolved_dependencies.is_empty() {
        bail!(
            "coverage audit failed for {}..={}: contiguous={}, unresolved_dependencies={:?}",
            config.from_height,
            config.to_height,
            coverage.contiguous,
            coverage.unresolved_dependencies
        );
    }
    let summary = RunSummary {
        record: "summary",
        mode: config.mode.as_str(),
        role: role_name(config.role),
        from_height: config.from_height,
        to_height: config.to_height,
        blocks_applied,
        events_per_kind,
        rejections_per_code,
        kernel_only,
        legacy_only,
        coverage,
    };
    println!("{}", serde_json::to_string_pretty(&summary)?);
    output.write_summary(&summary)?;
    output.flush()?;
    Ok(summary)
}

async fn finalize_plan(
    engine: &ViaProtocolEngine, reader: &PgObservationReader, envelope: &BitcoinBlockEnvelope,
    context: &ProtocolContext,
) -> Result<BlockPlan> {
    let (mut draft, initial_keys) = engine.inspect(envelope, context);
    let mut resolved = BTreeMap::new();
    resolve_dependencies(reader, &initial_keys, &mut resolved).await?;
    for round in 1..=MAX_FINALIZE_ROUNDS {
        match engine.finalize(draft, &resolved)? {
            FinalizeOutcome::Complete(plan) => return Ok(*plan),
            FinalizeOutcome::NeedDependencies { draft: next, keys } => {
                if keys.is_empty() {
                    bail!("kernel requested an empty dependency round {round}");
                }
                if round == MAX_FINALIZE_ROUNDS {
                    bail!("kernel did not finalize within {MAX_FINALIZE_ROUNDS} dependency rounds");
                }
                if keys.iter().any(|key| resolved.contains_key(key)) {
                    bail!("kernel re-requested an already resolved dependency in round {round}");
                }
                resolve_dependencies(reader, &keys, &mut resolved).await?;
                draft = next;
            }
        }
    }
    unreachable!("the bounded finalize loop always returns")
}

async fn resolve_dependencies(
    reader: &PgObservationReader, keys: &[DependencyKey], resolved: &mut BTreeMap<DependencyKey, ResolvedDependency>,
) -> Result<()> {
    for key in keys {
        let value = match key {
            DependencyKey::RawTx(txid) => {
                let resolution = match reader
                    .canonical_observed_tx(txid)
                    .await
                    .with_context(|| format!("read raw transaction dependency {txid} from shadow store"))?
                {
                    Some(observed) => Resolution::Present(observed),
                    None => Resolution::KnownAbsent,
                };
                ResolvedDependency::RawTx(resolution)
            }
            DependencyKey::TrackedOutput(outpoint) => {
                let resolution = match reader
                    .tracked_output(outpoint)
                    .await
                    .with_context(|| format!("read tracked output dependency {outpoint} from shadow store"))?
                {
                    Some(output) => Resolution::Present(output),
                    None => Resolution::KnownAbsent,
                };
                ResolvedDependency::TrackedOutput(resolution)
            }
        };
        if resolved.insert(key.clone(), value).is_some() {
            bail!("dependency {key:?} was resolved more than once");
        }
    }
    Ok(())
}

/// The legacy indexer needs configured wallets to discover the signer of the bootstrap transaction itself.
fn legacy_comparison_context<'a>(input: &'a ProtocolContext, next: &'a ProtocolContext) -> &'a ProtocolContext {
    if !input.wallets.is_bootstrapped() && next.wallets.is_bootstrapped() {
        next
    } else {
        input
    }
}

fn system_wallets_from_context(context: &ProtocolContext, network: Network) -> Result<SystemWallets> {
    let address = |script: &ScriptBuf, role: &str| {
        Address::from_script(script, network)
            .with_context(|| format!("{role} script is not a standard address for {network}"))
    };
    Ok(SystemWallets {
        sequencer: address(&context.wallets.sequencer, "sequencer")?,
        bridge: address(&context.wallets.bridge, "bridge")?,
        governance: address(&context.wallets.governance, "governance")?,
        verifiers: context
            .wallets
            .verifiers
            .iter()
            .enumerate()
            .map(|(index, script)| address(script, &format!("verifier {index}")))
            .collect::<Result<Vec<_>>>()?,
    })
}

pub fn normalize_kernel_events(events: &[ProtocolEvent]) -> BTreeMap<Fact, u64> {
    let mut facts: BTreeMap<Fact, u64> = BTreeMap::new();
    for event in events {
        match event {
            ProtocolEvent::DepositObserved(deposit) => {
                *facts
                    .entry(Fact {
                        kind: "deposit".into(),
                        txid: deposit.subject.txid.to_string(),
                        key_fields: FactFields::Deposit {
                            amount_sat: deposit.amount.to_sat(),
                            receiver: deposit.receiver.to_vec(),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::L1BatchDAReference(reference) => {
                *facts
                    .entry(Fact {
                        kind: "l1_batch_da_reference".into(),
                        txid: reference.subject_txid.to_string(),
                        key_fields: FactFields::BatchReference {
                            batch_index: reference.l1_batch_index,
                            blob_id: reference.blob_id.clone(),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::ProofDAReference(reference) => {
                *facts
                    .entry(Fact {
                        kind: "proof_da_reference".into(),
                        txid: reference.subject_txid.to_string(),
                        key_fields: FactFields::ProofReference {
                            reveal_txid: reference.batch.reveal_txid.to_string(),
                            blob_id: reference.blob_id.clone(),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::ValidatorAttestation(attestation) => {
                *facts
                    .entry(Fact {
                        kind: "validator_attestation".into(),
                        txid: attestation.subject_txid.to_string(),
                        key_fields: FactFields::Attestation {
                            reference_txid: attestation.reference_txid.to_string(),
                            vote: attestation.ok,
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::SystemBootstrapping(bootstrap) => {
                *facts
                    .entry(Fact {
                        kind: "system_bootstrapping".into(),
                        txid: bootstrap.subject_txid.to_string(),
                        key_fields: FactFields::Bootstrap {
                            sequencer_script: bootstrap.wallets.sequencer.as_bytes().to_vec(),
                            bridge_script: bootstrap.wallets.bridge.as_bytes().to_vec(),
                            governance_script: bootstrap.wallets.governance.as_bytes().to_vec(),
                            verifier_scripts: scripts(&bootstrap.wallets.verifiers),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::SystemContractUpgradeProposal(proposal) => {
                *facts
                    .entry(Fact {
                        kind: "system_contract_upgrade_proposal".into(),
                        txid: proposal.subject_txid.to_string(),
                        key_fields: FactFields::UpgradeProposal {
                            contract_addresses: proposal
                                .proposal
                                .system_contracts
                                .iter()
                                .map(|(address, _)| address.to_vec())
                                .collect(),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::SystemContractUpgradeActivation(activation) => {
                *facts
                    .entry(Fact {
                        kind: "system_contract_upgrade_activation".into(),
                        txid: activation.subject_txid.to_string(),
                        key_fields: FactFields::ProposalReference {
                            proposal_txid: activation.proposal_txid.to_string(),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::BridgeWithdrawal(withdrawal) => {
                for payout in &withdrawal.withdrawals {
                    *facts
                        .entry(Fact {
                            kind: "bridge_withdrawal".into(),
                            txid: withdrawal.subject_txid.to_string(),
                            key_fields: FactFields::Withdrawal {
                                receiver_script: payout.receiver_script.as_bytes().to_vec(),
                                amount_sat: payout.amount.to_sat(),
                            },
                        })
                        .or_default() += 1;
                }
            }
            ProtocolEvent::UpdateBridgeProposal(proposal) => {
                *facts
                    .entry(Fact {
                        kind: "update_bridge_proposal".into(),
                        txid: proposal.subject_txid.to_string(),
                        key_fields: FactFields::BridgeProposal {
                            bridge_script: proposal.bridge_script.as_bytes().to_vec(),
                            verifier_scripts: scripts(&proposal.verifier_scripts),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::WalletRotation(rotation) => match &rotation.rotation {
                WalletRotation::Sequencer { new_script } => {
                    *facts
                        .entry(Fact {
                            kind: "rotate_sequencer".into(),
                            txid: rotation.subject_txid.to_string(),
                            key_fields: FactFields::WalletAddress { address_script: new_script.as_bytes().to_vec() },
                        })
                        .or_default() += 1;
                }
                WalletRotation::Governance { new_script } => {
                    *facts
                        .entry(Fact {
                            kind: "rotate_governance".into(),
                            txid: rotation.subject_txid.to_string(),
                            key_fields: FactFields::WalletAddress { address_script: new_script.as_bytes().to_vec() },
                        })
                        .or_default() += 1;
                }
                WalletRotation::Bridge { proposal_txid, .. } => {
                    *facts
                        .entry(Fact {
                            kind: "rotate_bridge".into(),
                            txid: rotation.subject_txid.to_string(),
                            key_fields: FactFields::ProposalReference { proposal_txid: proposal_txid.to_string() },
                        })
                        .or_default() += 1;
                }
            },
        }
    }
    facts
}

pub fn normalize_legacy_messages(
    messages: &[FullInscriptionMessage], block_transactions: &[Transaction], context: &ProtocolContext,
    network: Network,
) -> Result<BTreeMap<Fact, u64>> {
    let mut facts: BTreeMap<Fact, u64> = BTreeMap::new();
    for message in messages {
        let txid = canonical_legacy_txid(common_fields(message), block_transactions)?;
        match message {
            FullInscriptionMessage::L1ToL2Message(deposit) => {
                for fact in normalize_legacy_deposit(
                    txid,
                    deposit.input.receiver_l2_address.as_bytes(),
                    &context.wallets.bridge,
                    &deposit.tx_outputs,
                ) {
                    *facts.entry(fact).or_default() += 1;
                }
            }
            FullInscriptionMessage::L1BatchDAReference(reference) => {
                *facts
                    .entry(Fact {
                        kind: "l1_batch_da_reference".into(),
                        txid: txid.to_string(),
                        key_fields: FactFields::BatchReference {
                            batch_index: u64::from(reference.input.l1_batch_index.0),
                            blob_id: reference.input.blob_id.clone(),
                        },
                    })
                    .or_default() += 1;
            }
            FullInscriptionMessage::ProofDAReference(reference) => {
                *facts
                    .entry(Fact {
                        kind: "proof_da_reference".into(),
                        txid: txid.to_string(),
                        key_fields: FactFields::ProofReference {
                            reveal_txid: reference.input.l1_batch_reveal_txid.to_string(),
                            blob_id: reference.input.blob_id.clone(),
                        },
                    })
                    .or_default() += 1;
            }
            FullInscriptionMessage::ValidatorAttestation(attestation) => {
                *facts
                    .entry(Fact {
                        kind: "validator_attestation".into(),
                        txid: txid.to_string(),
                        key_fields: FactFields::Attestation {
                            reference_txid: attestation.input.reference_txid.to_string(),
                            vote: matches!(attestation.input.attestation, Vote::Ok),
                        },
                    })
                    .or_default() += 1;
            }
            FullInscriptionMessage::SystemBootstrapping(bootstrap) => {
                *facts
                    .entry(Fact {
                        kind: "system_bootstrapping".into(),
                        txid: txid.to_string(),
                        key_fields: FactFields::Bootstrap {
                            sequencer_script: checked_script(&bootstrap.input.sequencer_address, network)?,
                            bridge_script: checked_script(&bootstrap.input.bridge_musig2_address, network)?,
                            governance_script: checked_script(&bootstrap.input.governance_address, network)?,
                            verifier_scripts: bootstrap
                                .input
                                .verifier_p2wpkh_addresses
                                .iter()
                                .map(|address| checked_script(address, network))
                                .collect::<Result<Vec<_>>>()?,
                        },
                    })
                    .or_default() += 1;
            }
            FullInscriptionMessage::SystemContractUpgradeProposal(proposal) => {
                *facts
                    .entry(Fact {
                        kind: "system_contract_upgrade_proposal".into(),
                        txid: txid.to_string(),
                        key_fields: FactFields::UpgradeProposal {
                            contract_addresses: proposal
                                .input
                                .system_contracts
                                .iter()
                                .map(|(address, _)| address.as_bytes().to_vec())
                                .collect(),
                        },
                    })
                    .or_default() += 1;
            }
            FullInscriptionMessage::SystemContractUpgrade(activation) => {
                *facts
                    .entry(Fact {
                        kind: "system_contract_upgrade_activation".into(),
                        txid: txid.to_string(),
                        key_fields: FactFields::ProposalReference {
                            proposal_txid: activation.input.proposal_tx_id.to_string(),
                        },
                    })
                    .or_default() += 1;
            }
            FullInscriptionMessage::BridgeWithdrawal(withdrawal) => {
                for payout in &withdrawal.input.withdrawals {
                    *facts
                        .entry(Fact {
                            kind: "bridge_withdrawal".into(),
                            txid: txid.to_string(),
                            key_fields: FactFields::Withdrawal {
                                receiver_script: payout.receiver.script_pubkey().as_bytes().to_vec(),
                                amount_sat: payout.value.to_sat(),
                            },
                        })
                        .or_default() += 1;
                }
            }
            FullInscriptionMessage::UpdateBridgeProposal(proposal) => {
                *facts
                    .entry(Fact {
                        kind: "update_bridge_proposal".into(),
                        txid: txid.to_string(),
                        key_fields: FactFields::BridgeProposal {
                            bridge_script: checked_script(&proposal.input.bridge_musig2_address, network)?,
                            verifier_scripts: proposal
                                .input
                                .verifier_p2wpkh_addresses
                                .iter()
                                .map(|address| checked_script(address, network))
                                .collect::<Result<Vec<_>>>()?,
                        },
                    })
                    .or_default() += 1;
            }
            FullInscriptionMessage::UpdateSequencer(rotation) => {
                *facts
                    .entry(Fact {
                        kind: "rotate_sequencer".into(),
                        txid: txid.to_string(),
                        key_fields: FactFields::WalletAddress {
                            address_script: checked_script(&rotation.input.address, network)?,
                        },
                    })
                    .or_default() += 1;
            }
            FullInscriptionMessage::UpdateGovernance(rotation) => {
                *facts
                    .entry(Fact {
                        kind: "rotate_governance".into(),
                        txid: txid.to_string(),
                        key_fields: FactFields::WalletAddress {
                            address_script: checked_script(&rotation.input.address, network)?,
                        },
                    })
                    .or_default() += 1;
            }
            FullInscriptionMessage::UpdateBridge(rotation) => {
                *facts
                    .entry(Fact {
                        kind: "rotate_bridge".into(),
                        txid: txid.to_string(),
                        key_fields: FactFields::ProposalReference {
                            proposal_txid: rotation.input.proposal_tx_id.to_string(),
                        },
                    })
                    .or_default() += 1;
            }
        }
    }
    Ok(facts)
}

fn common_fields(message: &FullInscriptionMessage) -> &CommonFields {
    match message {
        FullInscriptionMessage::L1BatchDAReference(message) => &message.common,
        FullInscriptionMessage::ProofDAReference(message) => &message.common,
        FullInscriptionMessage::ValidatorAttestation(message) => &message.common,
        FullInscriptionMessage::SystemBootstrapping(message) => &message.common,
        FullInscriptionMessage::L1ToL2Message(message) => &message.common,
        FullInscriptionMessage::SystemContractUpgradeProposal(message) => &message.common,
        FullInscriptionMessage::BridgeWithdrawal(message) => &message.common,
        FullInscriptionMessage::UpdateBridgeProposal(message) => &message.common,
        FullInscriptionMessage::UpdateGovernance(message) => &message.common,
        FullInscriptionMessage::UpdateSequencer(message) => &message.common,
        FullInscriptionMessage::SystemContractUpgrade(message) => &message.common,
        FullInscriptionMessage::UpdateBridge(message) => &message.common,
    }
}

fn canonical_legacy_txid(common: &CommonFields, block_transactions: &[Transaction]) -> Result<Txid> {
    if let Some(index) = common.tx_index {
        let transaction = block_transactions
            .get(index)
            .ok_or_else(|| anyhow!("legacy message tx_index {index} is outside the fetched block"))?;
        let normalized: Txid = transaction.compute_ntxid().into();
        if normalized != common.tx_id {
            bail!("legacy message normalized txid {} does not match transaction at index {index}", common.tx_id);
        }
        return Ok(transaction.compute_txid());
    }

    let mut matching = block_transactions.iter().filter(|transaction| {
        let normalized: Txid = transaction.compute_ntxid().into();
        normalized == common.tx_id
    });
    let transaction = matching
        .next()
        .ok_or_else(|| anyhow!("legacy message normalized txid {} is absent from the fetched block", common.tx_id))?;
    if matching.next().is_some() {
        bail!("legacy message normalized txid {} matches multiple transactions in the fetched block", common.tx_id);
    }
    Ok(transaction.compute_txid())
}

fn normalize_legacy_deposit(txid: Txid, receiver: &[u8], bridge_script: &ScriptBuf, outputs: &[TxOut]) -> Vec<Fact> {
    outputs
        .iter()
        .filter(|output| output.script_pubkey == *bridge_script)
        .map(|output| Fact {
            kind: "deposit".into(),
            txid: txid.to_string(),
            key_fields: FactFields::Deposit { amount_sat: output.value.to_sat(), receiver: receiver.to_vec() },
        })
        .collect()
}

fn checked_script(address: &Address<NetworkUnchecked>, network: Network) -> Result<Vec<u8>> {
    Ok(address
        .clone()
        .require_network(network)
        .with_context(|| format!("legacy address is not valid for {network}"))?
        .script_pubkey()
        .as_bytes()
        .to_vec())
}

fn scripts(values: &[ScriptBuf]) -> Vec<Vec<u8>> {
    values.iter().map(|script| script.as_bytes().to_vec()).collect()
}

/// Counted multiset difference: a fact appearing twice on one side and once
/// on the other yields one delta, so identical outputs in one transaction
/// cannot mask each other.
pub fn diff_fact_sets(kernel: &BTreeMap<Fact, u64>, legacy: &BTreeMap<Fact, u64>) -> (Vec<Fact>, Vec<Fact>) {
    let mut kernel_only = Vec::new();
    let mut legacy_only = Vec::new();
    for (fact, k) in kernel {
        let l = legacy.get(fact).copied().unwrap_or(0);
        for _ in l..*k {
            kernel_only.push(fact.clone());
        }
    }
    for (fact, l) in legacy {
        let k = kernel.get(fact).copied().unwrap_or(0);
        for _ in k..*l {
            legacy_only.push(fact.clone());
        }
    }
    (kernel_only, legacy_only)
}

fn count_plan(plan: &BlockPlan, events: &mut BTreeMap<String, u64>, rejections: &mut BTreeMap<String, u64>) {
    for event in &plan.events {
        *events.entry(event_kind_name(event.kind()).into()).or_default() += 1;
    }
    for disposition in &plan.dispositions {
        if let DispositionKind::RejectedInvalid { code, .. } = disposition.kind {
            *rejections.entry(rejection_code_name(code).into()).or_default() += 1;
        }
    }
}

fn event_kind_name(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Deposit => "deposit",
        EventKind::L1BatchDAReference => "l1_batch_da_reference",
        EventKind::ProofDAReference => "proof_da_reference",
        EventKind::ValidatorAttestation => "validator_attestation",
        EventKind::SystemBootstrapping => "system_bootstrapping",
        EventKind::SystemContractUpgradeProposal => "system_contract_upgrade_proposal",
        EventKind::SystemContractUpgradeActivation => "system_contract_upgrade_activation",
        EventKind::BridgeWithdrawal => "bridge_withdrawal",
        EventKind::UpdateBridgeProposal => "update_bridge_proposal",
        EventKind::WalletRotation => "wallet_rotation",
    }
}

fn rejection_code_name(code: RejectionCode) -> &'static str {
    match code {
        RejectionCode::MalformedMessage => "malformed_message",
        RejectionCode::ConflictingReceiverEncodings => "conflicting_receiver_encodings",
        RejectionCode::ConflictingRoleUpdate => "conflicting_role_update",
        RejectionCode::Unauthorized => "unauthorized",
        RejectionCode::InvalidBootstrap => "invalid_bootstrap",
        RejectionCode::NonMonotonicUpgrade => "non_monotonic_upgrade",
        RejectionCode::InvalidReference => "invalid_reference",
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::CoreSequencer => "core",
        Role::Verifier => "verifier",
        Role::StandaloneIndexer => "indexer",
    }
}

fn hash_hex(hash: &[u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in hash {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn print_delta(delta: &DeltaRecord) -> Result<()> {
    println!("{}", serde_json::to_string(delta)?);
    Ok(())
}

struct OutputSink {
    writer: Option<BufWriter<File>>,
}

impl OutputSink {
    fn new(path: Option<&Path>) -> Result<Self> {
        let writer = path
            .map(|path| {
                File::create(path).map(BufWriter::new).with_context(|| format!("create output file {}", path.display()))
            })
            .transpose()?;
        Ok(Self { writer })
    }

    fn write_delta(&mut self, delta: &DeltaRecord) -> Result<()> {
        self.write_json_line(delta)
    }

    fn write_summary(&mut self, summary: &RunSummary) -> Result<()> {
        self.write_json_line(summary)
    }

    fn write_json_line(&mut self, value: &impl Serialize) -> Result<()> {
        let Some(writer) = &mut self.writer else {
            return Ok(());
        };
        serde_json::to_writer(&mut *writer, value).context("serialize output JSON")?;
        writer.write_all(b"\n").context("write output newline")?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if let Some(writer) = &mut self.writer {
            writer.flush().context("flush output file")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::{
        absolute, hashes::Hash, script::PushBytesBuf, taproot::Signature as TaprootSignature, transaction, Amount,
        OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
    };
    use via_btc_ingestion::{DepositEncoding, DepositObserved, EventOrdinal, MessageLocation};

    use super::*;

    fn fact(kind: &str, txid: &str, amount: u64) -> Fact {
        Fact {
            kind: kind.into(),
            txid: txid.into(),
            key_fields: FactFields::Deposit { amount_sat: amount, receiver: vec![1; 20] },
        }
    }

    fn comparison_context(script_byte: Option<u8>) -> ProtocolContext {
        let script = script_byte.map_or_else(ScriptBuf::new, |byte| ScriptBuf::from_bytes(vec![byte]));
        ProtocolContext {
            version: 1,
            wallets: via_btc_ingestion::WalletSet {
                sequencer: script.clone(),
                bridge: script.clone(),
                governance: script,
                verifiers: vec![],
            },
            protocol_version: via_btc_ingestion::ProtocolVersionTag { minor: 0, patch: 0 },
        }
    }

    fn legacy_transaction(script_byte: u8) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::from_bytes(vec![script_byte]),
                sequence: Sequence::MAX,
                witness: Witness::default(),
            }],
            output: vec![],
        }
    }

    fn legacy_common(transaction: &Transaction, tx_index: Option<usize>) -> CommonFields {
        CommonFields {
            schnorr_signature: TaprootSignature::from_slice(&[0; 64]).unwrap(),
            encoded_public_key: PushBytesBuf::new(),
            block_height: 1,
            tx_id: transaction.compute_ntxid().into(),
            tx_index,
            output_vout: None,
            p2wpkh_address: None,
        }
    }

    #[test]
    fn kernel_deposit_normalizes_to_amount_and_receiver() {
        let txid = Txid::from_byte_array([7; 32]);
        let event = ProtocolEvent::DepositObserved(DepositObserved {
            ordinal: EventOrdinal { tx_index: 3, location: MessageLocation::Output(1) },
            subject: OutPoint { txid, vout: 2 },
            amount: Amount::from_sat(42),
            receiver: [9; 20],
            l2_contract: [0; 20],
            call_data: vec![],
            sender_script: None,
            encoding: DepositEncoding::OpReturn,
        });

        assert_eq!(
            normalize_kernel_events(&[event]),
            BTreeMap::from([(
                Fact {
                    kind: "deposit".into(),
                    txid: txid.to_string(),
                    key_fields: FactFields::Deposit { amount_sat: 42, receiver: vec![9; 20] },
                },
                1,
            )])
        );
    }

    #[test]
    fn legacy_deposit_counts_every_bridge_output_including_duplicates() {
        let txid = Txid::from_byte_array([8; 32]);
        let bridge = ScriptBuf::from_bytes(vec![0x51]);
        let other = ScriptBuf::from_bytes(vec![0x52]);
        let outputs = vec![
            TxOut { value: Amount::from_sat(10), script_pubkey: bridge.clone() },
            TxOut { value: Amount::from_sat(99), script_pubkey: other },
            TxOut { value: Amount::from_sat(20), script_pubkey: bridge.clone() },
            TxOut { value: Amount::from_sat(10), script_pubkey: bridge.clone() },
        ];

        let facts = normalize_legacy_deposit(txid, &[3; 20], &bridge, &outputs);
        // Two 10-sat outputs plus one 20-sat output: duplicates preserved.
        assert_eq!(facts.len(), 3);
        assert_eq!(
            facts.iter().filter(|f| matches!(f.key_fields, FactFields::Deposit { amount_sat: 10, .. })).count(),
            2
        );
        assert!(facts.contains(&Fact {
            kind: "deposit".into(),
            txid: txid.to_string(),
            key_fields: FactFields::Deposit { amount_sat: 10, receiver: vec![3; 20] },
        }));
        assert!(facts.contains(&Fact {
            kind: "deposit".into(),
            txid: txid.to_string(),
            key_fields: FactFields::Deposit { amount_sat: 20, receiver: vec![3; 20] },
        }));
    }

    #[test]
    fn legacy_normalization_recovers_canonical_txid_and_rejects_ambiguous_ntxid() {
        let first = legacy_transaction(0x51);
        let second = legacy_transaction(0x52);
        assert_eq!(first.compute_ntxid(), second.compute_ntxid());
        assert_ne!(first.compute_txid(), second.compute_txid());

        assert_eq!(
            canonical_legacy_txid(&legacy_common(&first, Some(0)), &[first.clone()]).unwrap(),
            first.compute_txid()
        );
        assert_eq!(
            canonical_legacy_txid(&legacy_common(&first, None), &[first.clone()]).unwrap(),
            first.compute_txid()
        );
        assert!(canonical_legacy_txid(&legacy_common(&first, None), &[first, second]).is_err());
    }

    #[test]
    fn set_diff_ignores_order_and_classifies_both_sides() {
        let shared = fact("deposit", "shared", 1);
        let kernel = fact("deposit", "kernel", 2);
        let legacy = fact("deposit", "legacy", 3);
        let kernel_set = BTreeMap::from([(kernel.clone(), 1), (shared.clone(), 1)]);
        let legacy_set = BTreeMap::from([(shared, 1), (legacy.clone(), 1)]);

        assert_eq!(diff_fact_sets(&kernel_set, &legacy_set), (vec![kernel], vec![legacy]));
    }

    #[test]
    fn multiset_diff_detects_duplicate_facts() {
        let f = fact("deposit", "dup", 1);
        let kernel_set = BTreeMap::from([(f.clone(), 2)]);
        let legacy_set = BTreeMap::from([(f.clone(), 1)]);
        assert_eq!(diff_fact_sets(&kernel_set, &legacy_set), (vec![f], vec![]));
    }

    #[test]
    fn bootstrap_transition_supplies_wallets_for_legacy_comparison() {
        let empty = comparison_context(None);
        let bootstrapped = comparison_context(Some(0x51));

        assert!(std::ptr::eq(legacy_comparison_context(&empty, &bootstrapped), &bootstrapped));
        assert!(std::ptr::eq(legacy_comparison_context(&bootstrapped, &empty), &bootstrapped));
    }
}
