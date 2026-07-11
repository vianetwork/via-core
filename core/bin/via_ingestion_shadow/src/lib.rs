use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, bail, Context as _, Result};
use bitcoin::{address::NetworkUnchecked, consensus, Address, Network, OutPoint, ScriptBuf, Transaction, TxOut, Txid};
use clap::{Parser, ValueEnum};
use serde::Serialize;
use sqlx::{postgres::PgPoolOptions, PgPool};
use via_btc_client::{
    client::BitcoinClient,
    indexer::{
        positioned::{PositionedMessageParser, PositionedParseOutcome},
        BitcoinInscriptionIndexer,
    },
    ingestion_engine::ViaProtocolEngine,
    traits::BitcoinOps,
    types::{CommonFields, FullInscriptionMessage, NodeAuth, Vote},
};
use via_btc_ingestion::{
    AggregateAdapter, BatchReferenceSnapshot, BitcoinBlockEnvelope, BlockAnchor, BlockPlan, CoverageReport,
    DependencyKey, DepositEncoding, DispositionKind, EventKind, EventOrdinal, FinalizeOutcome, MessageLocation,
    ObservationReader, ProtocolContext, ProtocolEngine, ProtocolEvent, ProtocolVersionTag, RejectionCode, Resolution,
    ResolvedDependency, Role, WalletRotation,
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
    pub occurrence: FactOccurrence,
    pub payload: FactPayload,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct FactOccurrence {
    pub tx_index: u32,
    pub location: MessageLocation,
    pub kind: EventKind,
    pub subject: FactSubject,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum FactSubject {
    Output { txid: String, vout: u32 },
    Transaction { txid: String },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct BatchIdentity {
    pub reveal_txid: String,
    pub l1_batch_hash: [u8; 32],
    pub l1_batch_index: u64,
    pub prev_l1_batch_hash: [u8; 32],
    pub da_identifier: String,
    pub blob_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WalletIdentity {
    pub sequencer_script: Vec<u8>,
    pub bridge_script: Vec<u8>,
    pub governance_script: Vec<u8>,
    pub verifier_scripts: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct UpgradeIdentity {
    pub version: ProtocolVersionTag,
    pub bootloader_code_hash: [u8; 32],
    pub default_account_code_hash: [u8; 32],
    pub evm_emulator_code_hash: Option<[u8; 32]>,
    pub recursion_scheduler_level_vk_hash: [u8; 32],
    pub system_contracts: Vec<(Vec<u8>, [u8; 32])>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct OutPointIdentity {
    pub txid: String,
    pub vout: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WithdrawalIdentity {
    pub l2_id: [u8; 8],
    pub l2_tx_event_index: u16,
    pub receiver_script: Vec<u8>,
    pub amount_sat: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
/// Source semantics shared by the kernel event and legacy message.
///
/// Rotation input outpoints are absent because kernel rotation events do not retain them.
/// Upgrade activations compare the proposal txid and resolved version because the legacy
/// activation message does not carry the proposal payload; proposal payloads are compared
/// on their own occurrence.
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum FactPayload {
    Deposit {
        amount_sat: u64,
        receiver: Vec<u8>,
        l2_contract: Vec<u8>,
        call_data: Vec<u8>,
        sender_script: Option<Vec<u8>>,
        encoding: String,
    },
    BatchReference {
        batch_index: u64,
        l1_batch_hash: [u8; 32],
        prev_l1_batch_hash: [u8; 32],
        da_identifier: String,
        blob_id: String,
    },
    ProofReference {
        reveal_txid: String,
        da_identifier: String,
        blob_id: String,
        batch: BatchIdentity,
    },
    Attestation {
        reference_txid: String,
        vote: bool,
        attester_script: Vec<u8>,
        batch: BatchIdentity,
    },
    Withdrawal {
        version: u32,
        total_size: u64,
        v_size: u64,
        inputs: Vec<OutPointIdentity>,
        output_amount_sat: u64,
        payouts: Vec<WithdrawalIdentity>,
    },
    Bootstrap {
        start_height: u64,
        protocol_version: ProtocolVersionTag,
        bootloader_hash: [u8; 32],
        abstract_account_hash: [u8; 32],
        snark_wrapper_vk_hash: [u8; 32],
        evm_emulator_hash: [u8; 32],
        wallets: WalletIdentity,
    },
    UpgradeProposal {
        proposal: UpgradeIdentity,
    },
    UpgradeActivation {
        proposal_txid: String,
        resolved_version: ProtocolVersionTag,
    },
    BridgeProposal {
        bridge_script: Vec<u8>,
        verifier_scripts: Vec<Vec<u8>>,
    },
    SequencerRotation {
        new_script: Vec<u8>,
    },
    GovernanceRotation {
        new_script: Vec<u8>,
    },
    BridgeRotation {
        proposal_txid: String,
        new_bridge_script: Vec<u8>,
        new_verifier_scripts: Vec<Vec<u8>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeltaSide {
    KernelOnly,
    LegacyOnly,
    Context,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeltaRecord {
    pub height: u64,
    pub side: DeltaSide,
    #[serde(flatten)]
    pub detail: DeltaDetail,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum DeltaDetail {
    Fact { fact: Fact },
    ContextTransition { kernel: ProtocolContext, legacy: ProtocolContext },
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
    pub context_differences: u64,
    pub coverage: CoverageReport,
}

#[derive(Clone, Debug)]
struct LegacyBridgeProposal {
    bridge_script: ScriptBuf,
    verifier_scripts: Vec<ScriptBuf>,
}

#[derive(Clone, Debug)]
struct LegacyComparisonState {
    context: ProtocolContext,
    bridge_proposals: BTreeMap<Txid, LegacyBridgeProposal>,
    upgrade_proposals: BTreeMap<Txid, UpgradeIdentity>,
}

impl LegacyComparisonState {
    fn new(context: ProtocolContext) -> Self {
        Self { context, bridge_proposals: BTreeMap::new(), upgrade_proposals: BTreeMap::new() }
    }
}

#[derive(Clone, Debug)]
struct PositionedLegacyMessage {
    ordinal: EventOrdinal,
    subject_txid: Txid,
    signer_script: Option<ScriptBuf>,
    message: FullInscriptionMessage,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LegacyDepositKey {
    tx_index: u32,
    subject_txid: Txid,
    receiver: Vec<u8>,
    l2_contract: Vec<u8>,
    call_data: Vec<u8>,
    sender_script: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
struct LegacyDepositGroup {
    locations: Vec<MessageLocation>,
    outputs: Vec<TxOut>,
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
    let mut context_differences = 0;
    let mut legacy_state: Option<LegacyComparisonState> = None;

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
            let input_state = legacy_state.get_or_insert_with(|| LegacyComparisonState::new(context.clone()));
            let kernel_facts = normalize_kernel_events(&first_plan.events);
            let (legacy_facts, next_legacy_state) = compare_legacy_block(config, &reader, &envelope, input_state)
                .await
                .with_context(|| format!("height {height}: compare legacy block"))?;
            let (kernel_deltas, legacy_deltas) = diff_fact_sets(&kernel_facts, &legacy_facts);
            for fact in kernel_deltas {
                block_deltas.push(DeltaRecord {
                    height,
                    side: DeltaSide::KernelOnly,
                    detail: DeltaDetail::Fact { fact },
                });
            }
            for fact in legacy_deltas {
                block_deltas.push(DeltaRecord {
                    height,
                    side: DeltaSide::LegacyOnly,
                    detail: DeltaDetail::Fact { fact },
                });
            }
            if let Some(delta) = context_transition_delta(height, &first_plan.next_context, &next_legacy_state.context)
            {
                block_deltas.push(delta);
            }
            legacy_state = Some(next_legacy_state);
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
                DeltaSide::Context => context_differences += 1,
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
        context_differences,
        coverage,
    };
    println!("{}", serde_json::to_string_pretty(&summary)?);
    output.write_summary(&summary)?;
    output.flush()?;
    Ok(summary)
}

fn context_transition_delta(height: u64, kernel: &ProtocolContext, legacy: &ProtocolContext) -> Option<DeltaRecord> {
    (kernel != legacy).then(|| DeltaRecord {
        height,
        side: DeltaSide::Context,
        detail: DeltaDetail::ContextTransition { kernel: kernel.clone(), legacy: legacy.clone() },
    })
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

fn wallet_identity(wallets: &via_btc_ingestion::WalletSet) -> WalletIdentity {
    WalletIdentity {
        sequencer_script: wallets.sequencer.as_bytes().to_vec(),
        bridge_script: wallets.bridge.as_bytes().to_vec(),
        governance_script: wallets.governance.as_bytes().to_vec(),
        verifier_scripts: scripts(&wallets.verifiers),
    }
}

fn upgrade_identity(proposal: &via_btc_ingestion::UpgradePayload) -> UpgradeIdentity {
    UpgradeIdentity {
        version: proposal.version,
        bootloader_code_hash: proposal.bootloader_code_hash,
        default_account_code_hash: proposal.default_account_code_hash,
        evm_emulator_code_hash: proposal.evm_emulator_code_hash,
        recursion_scheduler_level_vk_hash: proposal.recursion_scheduler_level_vk_hash,
        system_contracts: proposal.system_contracts.iter().map(|(address, hash)| (address.to_vec(), *hash)).collect(),
    }
}

fn legacy_upgrade_identity(proposal: &via_btc_client::types::SystemContractUpgradeProposalInput) -> UpgradeIdentity {
    UpgradeIdentity {
        version: protocol_version_tag(&proposal.version),
        bootloader_code_hash: proposal.bootloader_code_hash.to_fixed_bytes(),
        default_account_code_hash: proposal.default_account_code_hash.to_fixed_bytes(),
        evm_emulator_code_hash: proposal.evm_emulator_code_hash.map(|hash| hash.to_fixed_bytes()),
        recursion_scheduler_level_vk_hash: proposal.recursion_scheduler_level_vk_hash.to_fixed_bytes(),
        system_contracts: proposal
            .system_contracts
            .iter()
            .map(|(address, hash)| (address.as_bytes().to_vec(), hash.to_fixed_bytes()))
            .collect(),
    }
}

fn protocol_version_tag(version: &zksync_types::protocol_version::ProtocolSemanticVersion) -> ProtocolVersionTag {
    ProtocolVersionTag { minor: version.minor as u32, patch: version.patch.0 }
}

fn batch_identity(batch: &BatchReferenceSnapshot) -> BatchIdentity {
    BatchIdentity {
        reveal_txid: batch.reveal_txid.to_string(),
        l1_batch_hash: batch.l1_batch_hash,
        l1_batch_index: batch.l1_batch_index,
        prev_l1_batch_hash: batch.prev_l1_batch_hash,
        da_identifier: batch.da_identifier.clone(),
        blob_id: batch.blob_id.clone(),
    }
}

fn legacy_batch_identity(txid: Txid, reference: &via_btc_client::types::L1BatchDAReferenceInput) -> BatchIdentity {
    BatchIdentity {
        reveal_txid: txid.to_string(),
        l1_batch_hash: reference.l1_batch_hash.to_fixed_bytes(),
        l1_batch_index: u64::from(reference.l1_batch_index.0),
        prev_l1_batch_hash: reference.prev_l1_batch_hash.to_fixed_bytes(),
        da_identifier: reference.da_identifier.clone(),
        blob_id: reference.blob_id.clone(),
    }
}

fn transaction_subject(txid: Txid) -> FactSubject {
    FactSubject::Transaction { txid: txid.to_string() }
}

fn output_subject(outpoint: OutPoint) -> FactSubject {
    FactSubject::Output { txid: outpoint.txid.to_string(), vout: outpoint.vout }
}

fn transaction_occurrence(ordinal: EventOrdinal, kind: EventKind, txid: Txid) -> FactOccurrence {
    FactOccurrence { tx_index: ordinal.tx_index, location: ordinal.location, kind, subject: transaction_subject(txid) }
}

fn deposit_encoding_name(encoding: DepositEncoding) -> String {
    match encoding {
        DepositEncoding::Inscription => "inscription",
        DepositEncoding::OpReturn => "op_return",
        DepositEncoding::Both => "both",
    }
    .into()
}

pub fn normalize_kernel_events(events: &[ProtocolEvent]) -> BTreeMap<Fact, u64> {
    let mut facts: BTreeMap<Fact, u64> = BTreeMap::new();
    for event in events {
        match event {
            ProtocolEvent::DepositObserved(deposit) => {
                *facts
                    .entry(Fact {
                        occurrence: FactOccurrence {
                            tx_index: deposit.ordinal.tx_index,
                            location: deposit.ordinal.location,
                            kind: EventKind::Deposit,
                            subject: output_subject(deposit.subject),
                        },
                        payload: FactPayload::Deposit {
                            amount_sat: deposit.amount.to_sat(),
                            receiver: deposit.receiver.to_vec(),
                            l2_contract: deposit.l2_contract.to_vec(),
                            call_data: deposit.call_data.clone(),
                            sender_script: deposit.sender_script.as_ref().map(|script| script.as_bytes().to_vec()),
                            encoding: deposit_encoding_name(deposit.encoding),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::L1BatchDAReference(reference) => {
                *facts
                    .entry(Fact {
                        occurrence: transaction_occurrence(
                            reference.ordinal,
                            EventKind::L1BatchDAReference,
                            reference.subject_txid,
                        ),
                        payload: FactPayload::BatchReference {
                            batch_index: reference.l1_batch_index,
                            l1_batch_hash: reference.l1_batch_hash,
                            prev_l1_batch_hash: reference.prev_l1_batch_hash,
                            da_identifier: reference.da_identifier.clone(),
                            blob_id: reference.blob_id.clone(),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::ProofDAReference(reference) => {
                *facts
                    .entry(Fact {
                        occurrence: transaction_occurrence(
                            reference.ordinal,
                            EventKind::ProofDAReference,
                            reference.subject_txid,
                        ),
                        payload: FactPayload::ProofReference {
                            reveal_txid: reference.batch.reveal_txid.to_string(),
                            da_identifier: reference.da_identifier.clone(),
                            blob_id: reference.blob_id.clone(),
                            batch: batch_identity(&reference.batch),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::ValidatorAttestation(attestation) => {
                *facts
                    .entry(Fact {
                        occurrence: transaction_occurrence(
                            attestation.ordinal,
                            EventKind::ValidatorAttestation,
                            attestation.subject_txid,
                        ),
                        payload: FactPayload::Attestation {
                            reference_txid: attestation.reference_txid.to_string(),
                            vote: attestation.ok,
                            attester_script: attestation.attester_script.as_bytes().to_vec(),
                            batch: batch_identity(&attestation.batch),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::SystemBootstrapping(bootstrap) => {
                *facts
                    .entry(Fact {
                        occurrence: transaction_occurrence(
                            bootstrap.ordinal,
                            EventKind::SystemBootstrapping,
                            bootstrap.subject_txid,
                        ),
                        payload: FactPayload::Bootstrap {
                            start_height: bootstrap.start_block_height,
                            protocol_version: bootstrap.protocol_version,
                            bootloader_hash: bootstrap.bootloader_hash,
                            abstract_account_hash: bootstrap.abstract_account_hash,
                            snark_wrapper_vk_hash: bootstrap.snark_wrapper_vk_hash,
                            evm_emulator_hash: bootstrap.evm_emulator_hash,
                            wallets: wallet_identity(&bootstrap.wallets),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::SystemContractUpgradeProposal(proposal) => {
                *facts
                    .entry(Fact {
                        occurrence: transaction_occurrence(
                            proposal.ordinal,
                            EventKind::SystemContractUpgradeProposal,
                            proposal.subject_txid,
                        ),
                        payload: FactPayload::UpgradeProposal { proposal: upgrade_identity(&proposal.proposal) },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::SystemContractUpgradeActivation(activation) => {
                *facts
                    .entry(Fact {
                        occurrence: transaction_occurrence(
                            activation.ordinal,
                            EventKind::SystemContractUpgradeActivation,
                            activation.subject_txid,
                        ),
                        payload: FactPayload::UpgradeActivation {
                            proposal_txid: activation.proposal_txid.to_string(),
                            resolved_version: activation.proposal.version,
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::BridgeWithdrawal(withdrawal) => {
                *facts
                    .entry(Fact {
                        occurrence: transaction_occurrence(
                            withdrawal.ordinal,
                            EventKind::BridgeWithdrawal,
                            withdrawal.subject_txid,
                        ),
                        payload: FactPayload::Withdrawal {
                            version: withdrawal.withdrawal_version,
                            total_size: withdrawal.total_size,
                            v_size: withdrawal.v_size,
                            inputs: withdrawal.inputs.iter().copied().map(outpoint_identity).collect(),
                            output_amount_sat: withdrawal.output_amount.to_sat(),
                            payouts: withdrawal
                                .withdrawals
                                .iter()
                                .map(|payout| WithdrawalIdentity {
                                    l2_id: payout.l2_id,
                                    l2_tx_event_index: payout.l2_tx_event_index,
                                    receiver_script: payout.receiver_script.as_bytes().to_vec(),
                                    amount_sat: payout.amount.to_sat(),
                                })
                                .collect(),
                        },
                    })
                    .or_default() += 1;
            }
            ProtocolEvent::UpdateBridgeProposal(proposal) => {
                *facts
                    .entry(Fact {
                        occurrence: transaction_occurrence(
                            proposal.ordinal,
                            EventKind::UpdateBridgeProposal,
                            proposal.subject_txid,
                        ),
                        payload: FactPayload::BridgeProposal {
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
                            occurrence: transaction_occurrence(
                                rotation.ordinal,
                                EventKind::WalletRotation,
                                rotation.subject_txid,
                            ),
                            payload: FactPayload::SequencerRotation { new_script: new_script.as_bytes().to_vec() },
                        })
                        .or_default() += 1;
                }
                WalletRotation::Governance { new_script } => {
                    *facts
                        .entry(Fact {
                            occurrence: transaction_occurrence(
                                rotation.ordinal,
                                EventKind::WalletRotation,
                                rotation.subject_txid,
                            ),
                            payload: FactPayload::GovernanceRotation { new_script: new_script.as_bytes().to_vec() },
                        })
                        .or_default() += 1;
                }
                WalletRotation::Bridge { proposal_txid, new_bridge_script, new_verifier_scripts } => {
                    *facts
                        .entry(Fact {
                            occurrence: transaction_occurrence(
                                rotation.ordinal,
                                EventKind::WalletRotation,
                                rotation.subject_txid,
                            ),
                            payload: FactPayload::BridgeRotation {
                                proposal_txid: proposal_txid.to_string(),
                                new_bridge_script: new_bridge_script.as_bytes().to_vec(),
                                new_verifier_scripts: scripts(new_verifier_scripts),
                            },
                        })
                        .or_default() += 1;
                }
            },
        }
    }
    facts
}

async fn compare_legacy_block(
    config: &RunConfig, reader: &PgObservationReader, envelope: &BitcoinBlockEnvelope,
    input_state: &LegacyComparisonState,
) -> Result<(BTreeMap<Fact, u64>, LegacyComparisonState)> {
    let candidates = parse_positioned_legacy_messages(envelope, &input_state.context, config.network)?;
    let processing_context = if input_state.context.wallets.is_bootstrapped() {
        Some(input_state.context.clone())
    } else {
        candidates
            .iter()
            .find_map(|candidate| match &candidate.message {
                FullInscriptionMessage::SystemBootstrapping(bootstrap) => Some(&bootstrap.input),
                _ => None,
            })
            .map(|bootstrap| legacy_bootstrap_context(&input_state.context, bootstrap, config.network))
            .transpose()?
    };

    let mut legacy_messages = if let Some(processing_context) = processing_context {
        let wallets = Arc::new(system_wallets_from_context(&processing_context, config.network)?);
        let legacy_height = u32::try_from(envelope.anchor.height)
            .with_context(|| format!("height {}: legacy indexer height exceeds u32", envelope.anchor.height))?;
        BitcoinInscriptionIndexer::new(config.client.clone(), wallets)
            .process_block(legacy_height)
            .await
            .context("legacy process_block")?
    } else {
        vec![]
    };
    if !input_state.context.wallets.is_bootstrapped() {
        legacy_messages.retain(|message| matches!(message, FullInscriptionMessage::SystemBootstrapping(_)));
    }
    let positioned = position_accepted_legacy_messages(&legacy_messages, &candidates)?;
    normalize_legacy_messages(&positioned, reader, envelope, input_state, config.network).await
}

fn parse_positioned_legacy_messages(
    envelope: &BitcoinBlockEnvelope, context: &ProtocolContext, network: Network,
) -> Result<Vec<PositionedLegacyMessage>> {
    let wallets =
        context.wallets.is_bootstrapped().then(|| system_wallets_from_context(context, network)).transpose()?;
    let mut parser = PositionedMessageParser::new(network);
    let mut messages = vec![];
    for (tx_index, transaction) in envelope.transactions.iter().enumerate() {
        for outcome in parser.parse_transaction(
            transaction,
            u32::try_from(tx_index).context("legacy transaction index exceeds u32")?,
            u32::try_from(envelope.anchor.height).context("legacy block height exceeds u32")?,
            wallets.as_ref(),
        ) {
            if let PositionedParseOutcome::Valid(message) = outcome {
                messages.push(PositionedLegacyMessage {
                    ordinal: EventOrdinal {
                        tx_index: u32::try_from(tx_index).context("legacy transaction index exceeds u32")?,
                        location: message.location,
                    },
                    subject_txid: transaction.compute_txid(),
                    signer_script: message.signer_script.clone(),
                    message: message.message,
                });
            }
        }
    }
    messages.sort_by_key(|message| message.ordinal);
    Ok(messages)
}

fn position_accepted_legacy_messages(
    accepted: &[FullInscriptionMessage], candidates: &[PositionedLegacyMessage],
) -> Result<Vec<PositionedLegacyMessage>> {
    let mut unused = vec![true; candidates.len()];
    let mut positioned = Vec::with_capacity(accepted.len());
    for message in accepted {
        let index = candidates
            .iter()
            .enumerate()
            .position(|(index, candidate)| unused[index] && legacy_messages_match(&candidate.message, message))
            .ok_or_else(|| {
                anyhow!("legacy indexer returned a message that cannot be bound to an input-context carrier")
            })?;
        unused[index] = false;
        positioned.push(candidates[index].clone());
    }
    positioned.sort_by_key(|message| message.ordinal);
    Ok(positioned)
}

fn legacy_messages_match(candidate: &FullInscriptionMessage, accepted: &FullInscriptionMessage) -> bool {
    if std::mem::discriminant(candidate) != std::mem::discriminant(accepted) {
        return false;
    }
    let mut normalized = candidate.clone();
    *common_fields_mut(&mut normalized) = common_fields(accepted).clone();
    normalized == *accepted
}

async fn normalize_legacy_messages(
    messages: &[PositionedLegacyMessage], reader: &PgObservationReader, envelope: &BitcoinBlockEnvelope,
    input_state: &LegacyComparisonState, network: Network,
) -> Result<(BTreeMap<Fact, u64>, LegacyComparisonState)> {
    let mut facts = BTreeMap::new();
    let mut next_state = input_state.clone();
    normalize_legacy_deposits(messages, &input_state.context, &mut facts);

    for positioned in messages {
        let occurrence = |kind| transaction_occurrence(positioned.ordinal, kind, positioned.subject_txid);
        let fact = match &positioned.message {
            FullInscriptionMessage::L1ToL2Message(_) => continue,
            FullInscriptionMessage::L1BatchDAReference(reference) => Some(Fact {
                occurrence: occurrence(EventKind::L1BatchDAReference),
                payload: FactPayload::BatchReference {
                    batch_index: u64::from(reference.input.l1_batch_index.0),
                    l1_batch_hash: reference.input.l1_batch_hash.to_fixed_bytes(),
                    prev_l1_batch_hash: reference.input.prev_l1_batch_hash.to_fixed_bytes(),
                    da_identifier: reference.input.da_identifier.clone(),
                    blob_id: reference.input.blob_id.clone(),
                },
            }),
            FullInscriptionMessage::ProofDAReference(reference) => {
                let batch = resolve_legacy_batch(
                    reader,
                    envelope,
                    &input_state.context,
                    reference.input.l1_batch_reveal_txid,
                    network,
                )
                .await?;
                Some(Fact {
                    occurrence: occurrence(EventKind::ProofDAReference),
                    payload: FactPayload::ProofReference {
                        reveal_txid: reference.input.l1_batch_reveal_txid.to_string(),
                        da_identifier: reference.input.da_identifier.clone(),
                        blob_id: reference.input.blob_id.clone(),
                        batch,
                    },
                })
            }
            FullInscriptionMessage::ValidatorAttestation(attestation) => {
                let batch = resolve_legacy_attestation_batch(
                    reader,
                    envelope,
                    &input_state.context,
                    attestation.input.reference_txid,
                    network,
                )
                .await?;
                Some(Fact {
                    occurrence: occurrence(EventKind::ValidatorAttestation),
                    payload: FactPayload::Attestation {
                        reference_txid: attestation.input.reference_txid.to_string(),
                        vote: matches!(attestation.input.attestation, Vote::Ok),
                        attester_script: positioned
                            .signer_script
                            .as_ref()
                            .map(|script| script.as_bytes().to_vec())
                            .ok_or_else(|| anyhow!("legacy attestation has no signer script"))?,
                        batch,
                    },
                })
            }
            FullInscriptionMessage::SystemBootstrapping(bootstrap) => {
                let context = legacy_bootstrap_context(&input_state.context, &bootstrap.input, network)?;
                if !next_state.context.wallets.is_bootstrapped() {
                    next_state.context = context.clone();
                }
                Some(Fact {
                    occurrence: occurrence(EventKind::SystemBootstrapping),
                    payload: FactPayload::Bootstrap {
                        start_height: u64::from(bootstrap.input.start_block_height),
                        protocol_version: protocol_version_tag(&bootstrap.input.protocol_version),
                        bootloader_hash: bootstrap.input.bootloader_hash.to_fixed_bytes(),
                        abstract_account_hash: bootstrap.input.abstract_account_hash.to_fixed_bytes(),
                        snark_wrapper_vk_hash: bootstrap.input.snark_wrapper_vk_hash.to_fixed_bytes(),
                        evm_emulator_hash: bootstrap.input.evm_emulator_hash.to_fixed_bytes(),
                        wallets: wallet_identity(&context.wallets),
                    },
                })
            }
            FullInscriptionMessage::SystemContractUpgradeProposal(proposal) => {
                let proposal = legacy_upgrade_identity(&proposal.input);
                next_state.upgrade_proposals.insert(positioned.subject_txid, proposal.clone());
                Some(Fact {
                    occurrence: occurrence(EventKind::SystemContractUpgradeProposal),
                    payload: FactPayload::UpgradeProposal { proposal },
                })
            }
            FullInscriptionMessage::SystemContractUpgrade(activation) => {
                let proposal = resolve_legacy_upgrade(
                    reader,
                    envelope,
                    &input_state.context,
                    &next_state,
                    activation.input.proposal_tx_id,
                    network,
                )
                .await?;
                next_state.context.protocol_version = proposal.version;
                Some(Fact {
                    occurrence: occurrence(EventKind::SystemContractUpgradeActivation),
                    payload: FactPayload::UpgradeActivation {
                        proposal_txid: activation.input.proposal_tx_id.to_string(),
                        resolved_version: proposal.version,
                    },
                })
            }
            FullInscriptionMessage::BridgeWithdrawal(withdrawal) => Some(Fact {
                occurrence: occurrence(EventKind::BridgeWithdrawal),
                payload: FactPayload::Withdrawal {
                    version: withdrawal.input.version.clone() as u32,
                    total_size: withdrawal.input.total_size.max(0) as u64,
                    v_size: withdrawal.input.v_size.max(0) as u64,
                    inputs: withdrawal.input.inputs.iter().copied().map(outpoint_identity).collect(),
                    output_amount_sat: withdrawal.input.output_amount,
                    payouts: withdrawal
                        .input
                        .withdrawals
                        .iter()
                        .map(|payout| {
                            let bytes = payout.l2_meta.to_bytes()?;
                            Ok(WithdrawalIdentity {
                                l2_id: bytes[..8].try_into().expect("withdrawal metadata always has eight id bytes"),
                                l2_tx_event_index: payout.l2_meta.l2_tx_event_index,
                                receiver_script: payout.receiver.script_pubkey().as_bytes().to_vec(),
                                amount_sat: payout.value.to_sat(),
                            })
                        })
                        .collect::<Result<Vec<_>>>()?,
                },
            }),
            FullInscriptionMessage::UpdateBridgeProposal(proposal) => {
                let proposal = LegacyBridgeProposal {
                    bridge_script: ScriptBuf::from_bytes(checked_script(
                        &proposal.input.bridge_musig2_address,
                        network,
                    )?),
                    verifier_scripts: proposal
                        .input
                        .verifier_p2wpkh_addresses
                        .iter()
                        .map(|address| checked_script(address, network).map(ScriptBuf::from_bytes))
                        .collect::<Result<Vec<_>>>()?,
                };
                next_state.bridge_proposals.insert(positioned.subject_txid, proposal.clone());
                Some(Fact {
                    occurrence: occurrence(EventKind::UpdateBridgeProposal),
                    payload: FactPayload::BridgeProposal {
                        bridge_script: proposal.bridge_script.as_bytes().to_vec(),
                        verifier_scripts: scripts(&proposal.verifier_scripts),
                    },
                })
            }
            FullInscriptionMessage::UpdateSequencer(rotation) => {
                let new_script = ScriptBuf::from_bytes(checked_script(&rotation.input.address, network)?);
                next_state.context.wallets.sequencer = new_script.clone();
                Some(Fact {
                    occurrence: occurrence(EventKind::WalletRotation),
                    payload: FactPayload::SequencerRotation { new_script: new_script.as_bytes().to_vec() },
                })
            }
            FullInscriptionMessage::UpdateGovernance(rotation) => {
                let new_script = ScriptBuf::from_bytes(checked_script(&rotation.input.address, network)?);
                next_state.context.wallets.governance = new_script.clone();
                Some(Fact {
                    occurrence: occurrence(EventKind::WalletRotation),
                    payload: FactPayload::GovernanceRotation { new_script: new_script.as_bytes().to_vec() },
                })
            }
            FullInscriptionMessage::UpdateBridge(rotation) => {
                let proposal = resolve_legacy_bridge_proposal(
                    reader,
                    envelope,
                    &input_state.context,
                    &next_state,
                    rotation.input.proposal_tx_id,
                    network,
                )
                .await?;
                next_state.context.wallets.bridge = proposal.bridge_script.clone();
                next_state.context.wallets.verifiers = proposal.verifier_scripts.clone();
                Some(Fact {
                    occurrence: occurrence(EventKind::WalletRotation),
                    payload: FactPayload::BridgeRotation {
                        proposal_txid: rotation.input.proposal_tx_id.to_string(),
                        new_bridge_script: proposal.bridge_script.as_bytes().to_vec(),
                        new_verifier_scripts: scripts(&proposal.verifier_scripts),
                    },
                })
            }
        };
        if let Some(fact) = fact {
            *facts.entry(fact).or_default() += 1;
        }
    }
    Ok((facts, next_state))
}

fn normalize_legacy_deposits(
    messages: &[PositionedLegacyMessage], context: &ProtocolContext, facts: &mut BTreeMap<Fact, u64>,
) {
    let mut groups: BTreeMap<LegacyDepositKey, LegacyDepositGroup> = BTreeMap::new();
    for positioned in messages {
        let FullInscriptionMessage::L1ToL2Message(deposit) = &positioned.message else {
            continue;
        };
        let key = LegacyDepositKey {
            tx_index: positioned.ordinal.tx_index,
            subject_txid: positioned.subject_txid,
            receiver: deposit.input.receiver_l2_address.as_bytes().to_vec(),
            l2_contract: deposit.input.l2_contract_address.as_bytes().to_vec(),
            call_data: deposit.input.call_data.clone(),
            sender_script: positioned.signer_script.as_ref().map(|script| script.as_bytes().to_vec()),
        };
        let group = groups
            .entry(key)
            .or_insert_with(|| LegacyDepositGroup { locations: vec![], outputs: deposit.tx_outputs.clone() });
        group.locations.push(positioned.ordinal.location);
    }

    for (key, group) in groups {
        let input_location = group
            .locations
            .iter()
            .filter_map(|location| match location {
                MessageLocation::Input(index) => Some(MessageLocation::Input(*index)),
                MessageLocation::Output(_) => None,
            })
            .min();
        let output_location = group
            .locations
            .iter()
            .filter_map(|location| match location {
                MessageLocation::Output(index) => Some(MessageLocation::Output(*index)),
                MessageLocation::Input(_) => None,
            })
            .min();
        let (location, encoding) = match (input_location, output_location) {
            (Some(input), Some(_)) => (input, DepositEncoding::Both),
            (Some(input), None) => (input, DepositEncoding::Inscription),
            (None, Some(output)) => (output, DepositEncoding::OpReturn),
            (None, None) => continue,
        };
        for (vout, output) in group.outputs.iter().enumerate() {
            if output.script_pubkey != context.wallets.bridge {
                continue;
            }
            let outpoint = OutPoint {
                txid: key.subject_txid,
                vout: u32::try_from(vout).expect("a Bitcoin transaction output index fits in u32"),
            };
            let fact = Fact {
                occurrence: FactOccurrence {
                    tx_index: key.tx_index,
                    location,
                    kind: EventKind::Deposit,
                    subject: output_subject(outpoint),
                },
                payload: FactPayload::Deposit {
                    amount_sat: output.value.to_sat(),
                    receiver: key.receiver.clone(),
                    l2_contract: key.l2_contract.clone(),
                    call_data: key.call_data.clone(),
                    sender_script: key.sender_script.clone(),
                    encoding: deposit_encoding_name(encoding),
                },
            };
            *facts.entry(fact).or_default() += 1;
        }
    }
}

fn legacy_bootstrap_context(
    input: &ProtocolContext, bootstrap: &via_btc_client::types::SystemBootstrappingInput, network: Network,
) -> Result<ProtocolContext> {
    let wallets = via_btc_ingestion::WalletSet {
        sequencer: ScriptBuf::from_bytes(checked_script(&bootstrap.sequencer_address, network)?),
        bridge: ScriptBuf::from_bytes(checked_script(&bootstrap.bridge_musig2_address, network)?),
        governance: ScriptBuf::from_bytes(checked_script(&bootstrap.governance_address, network)?),
        verifiers: bootstrap
            .verifier_p2wpkh_addresses
            .iter()
            .map(|address| checked_script(address, network).map(ScriptBuf::from_bytes))
            .collect::<Result<Vec<_>>>()?,
    };
    wallets.validate().map_err(|error| anyhow!("invalid legacy bootstrap wallets: {error}"))?;
    Ok(ProtocolContext {
        version: input.version,
        wallets,
        protocol_version: protocol_version_tag(&bootstrap.protocol_version),
    })
}

async fn resolve_legacy_batch(
    reader: &PgObservationReader, envelope: &BitcoinBlockEnvelope, context: &ProtocolContext, txid: Txid,
    network: Network,
) -> Result<BatchIdentity> {
    let transaction = legacy_transaction(reader, envelope, txid).await?;
    let messages = parse_referenced_legacy_transaction(&transaction, context, network)?;
    messages
        .iter()
        .find_map(|message| match message {
            FullInscriptionMessage::L1BatchDAReference(reference) => {
                Some(legacy_batch_identity(txid, &reference.input))
            }
            _ => None,
        })
        .ok_or_else(|| anyhow!("referenced transaction {txid} is not a legacy batch reference"))
}

async fn resolve_legacy_attestation_batch(
    reader: &PgObservationReader, envelope: &BitcoinBlockEnvelope, context: &ProtocolContext, proof_txid: Txid,
    network: Network,
) -> Result<BatchIdentity> {
    let proof = legacy_transaction(reader, envelope, proof_txid).await?;
    let messages = parse_referenced_legacy_transaction(&proof, context, network)?;
    let reveal_txid = messages
        .iter()
        .find_map(|message| match message {
            FullInscriptionMessage::ProofDAReference(reference) => Some(reference.input.l1_batch_reveal_txid),
            _ => None,
        })
        .ok_or_else(|| anyhow!("referenced transaction {proof_txid} is not a legacy proof reference"))?;
    resolve_legacy_batch(reader, envelope, context, reveal_txid, network).await
}

async fn resolve_legacy_upgrade(
    reader: &PgObservationReader, envelope: &BitcoinBlockEnvelope, context: &ProtocolContext,
    state: &LegacyComparisonState, proposal_txid: Txid, network: Network,
) -> Result<UpgradeIdentity> {
    if let Some(proposal) = state.upgrade_proposals.get(&proposal_txid) {
        return Ok(proposal.clone());
    }
    let transaction = legacy_transaction(reader, envelope, proposal_txid).await?;
    parse_referenced_legacy_transaction(&transaction, context, network)?
        .iter()
        .find_map(|message| match message {
            FullInscriptionMessage::SystemContractUpgradeProposal(proposal) => {
                Some(legacy_upgrade_identity(&proposal.input))
            }
            _ => None,
        })
        .ok_or_else(|| anyhow!("referenced transaction {proposal_txid} is not a legacy upgrade proposal"))
}

async fn resolve_legacy_bridge_proposal(
    reader: &PgObservationReader, envelope: &BitcoinBlockEnvelope, context: &ProtocolContext,
    state: &LegacyComparisonState, proposal_txid: Txid, network: Network,
) -> Result<LegacyBridgeProposal> {
    if let Some(proposal) = state.bridge_proposals.get(&proposal_txid) {
        return Ok(proposal.clone());
    }
    let transaction = legacy_transaction(reader, envelope, proposal_txid).await?;
    let proposal = parse_referenced_legacy_transaction(&transaction, context, network)?
        .into_iter()
        .find_map(|message| match message {
            FullInscriptionMessage::UpdateBridgeProposal(proposal) => Some(proposal.input),
            _ => None,
        })
        .ok_or_else(|| anyhow!("referenced transaction {proposal_txid} is not a legacy bridge proposal"))?;
    Ok(LegacyBridgeProposal {
        bridge_script: ScriptBuf::from_bytes(checked_script(&proposal.bridge_musig2_address, network)?),
        verifier_scripts: proposal
            .verifier_p2wpkh_addresses
            .iter()
            .map(|address| checked_script(address, network).map(ScriptBuf::from_bytes))
            .collect::<Result<Vec<_>>>()?,
    })
}

async fn legacy_transaction(
    reader: &PgObservationReader, envelope: &BitcoinBlockEnvelope, txid: Txid,
) -> Result<Transaction> {
    if let Some(transaction) = envelope.transactions.iter().find(|transaction| transaction.compute_txid() == txid) {
        return Ok(transaction.clone());
    }
    let observed = reader
        .canonical_observed_tx(&txid)
        .await
        .with_context(|| format!("read referenced legacy transaction {txid}"))?
        .ok_or_else(|| anyhow!("referenced legacy transaction {txid} is absent from the observation store"))?;
    consensus::deserialize(observed.variant().raw())
        .with_context(|| format!("decode referenced legacy transaction {txid}"))
}

fn parse_referenced_legacy_transaction(
    transaction: &Transaction, context: &ProtocolContext, network: Network,
) -> Result<Vec<FullInscriptionMessage>> {
    let wallets =
        context.wallets.is_bootstrapped().then(|| system_wallets_from_context(context, network)).transpose()?;
    let mut parser = PositionedMessageParser::new(network);
    Ok(parser
        .parse_transaction(transaction, 0, 0, wallets.as_ref())
        .into_iter()
        .filter_map(|outcome| match outcome {
            PositionedParseOutcome::Valid(message) => Some(message.message),
            _ => None,
        })
        .collect())
}

fn outpoint_identity(outpoint: OutPoint) -> OutPointIdentity {
    OutPointIdentity { txid: outpoint.txid.to_string(), vout: outpoint.vout }
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

fn common_fields_mut(message: &mut FullInscriptionMessage) -> &mut CommonFields {
    match message {
        FullInscriptionMessage::L1BatchDAReference(message) => &mut message.common,
        FullInscriptionMessage::ProofDAReference(message) => &mut message.common,
        FullInscriptionMessage::ValidatorAttestation(message) => &mut message.common,
        FullInscriptionMessage::SystemBootstrapping(message) => &mut message.common,
        FullInscriptionMessage::L1ToL2Message(message) => &mut message.common,
        FullInscriptionMessage::SystemContractUpgradeProposal(message) => &mut message.common,
        FullInscriptionMessage::BridgeWithdrawal(message) => &mut message.common,
        FullInscriptionMessage::UpdateBridgeProposal(message) => &mut message.common,
        FullInscriptionMessage::UpdateGovernance(message) => &mut message.common,
        FullInscriptionMessage::UpdateSequencer(message) => &mut message.common,
        FullInscriptionMessage::SystemContractUpgrade(message) => &mut message.common,
        FullInscriptionMessage::UpdateBridge(message) => &mut message.common,
    }
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
    use bitcoin::hashes::Hash;

    use super::*;

    fn subject_txid(seed: u8) -> Txid {
        Txid::from_byte_array([seed; 32])
    }

    fn batch(seed: u8) -> BatchIdentity {
        BatchIdentity {
            reveal_txid: subject_txid(seed).to_string(),
            l1_batch_hash: [seed + 1; 32],
            l1_batch_index: u64::from(seed),
            prev_l1_batch_hash: [seed + 2; 32],
            da_identifier: format!("da-{seed}"),
            blob_id: format!("blob-{seed}"),
        }
    }

    fn upgrade(seed: u8) -> UpgradeIdentity {
        UpgradeIdentity {
            version: ProtocolVersionTag { minor: u32::from(seed), patch: 1 },
            bootloader_code_hash: [seed; 32],
            default_account_code_hash: [seed + 1; 32],
            evm_emulator_code_hash: Some([seed + 2; 32]),
            recursion_scheduler_level_vk_hash: [seed + 3; 32],
            system_contracts: vec![(vec![seed; 20], [seed + 4; 32])],
        }
    }

    fn fact(kind: EventKind, payload: FactPayload, output_subject: bool) -> Fact {
        let txid = subject_txid(7);
        Fact {
            occurrence: FactOccurrence {
                tx_index: 3,
                location: MessageLocation::Input(2),
                kind,
                subject: if output_subject {
                    FactSubject::Output { txid: txid.to_string(), vout: 4 }
                } else {
                    FactSubject::Transaction { txid: txid.to_string() }
                },
            },
            payload,
        }
    }

    fn changed(base: &Fact, change: impl FnOnce(&mut Fact)) -> Fact {
        let mut fact = base.clone();
        change(&mut fact);
        fact
    }

    fn identity_mutations(base: &Fact) -> Vec<Fact> {
        let mut mutations = vec![
            changed(base, |fact| fact.occurrence.tx_index += 1),
            changed(base, |fact| fact.occurrence.location = MessageLocation::Output(9)),
            changed(base, |fact| {
                fact.occurrence.kind = if fact.occurrence.kind == EventKind::Deposit {
                    EventKind::L1BatchDAReference
                } else {
                    EventKind::Deposit
                }
            }),
            changed(base, |fact| match &mut fact.occurrence.subject {
                FactSubject::Output { txid, .. } | FactSubject::Transaction { txid } => *txid = "different".into(),
            }),
        ];
        if matches!(base.occurrence.subject, FactSubject::Output { .. }) {
            mutations.push(changed(base, |fact| {
                let FactSubject::Output { vout, .. } = &mut fact.occurrence.subject else { unreachable!() };
                *vout += 1;
            }));
        }
        mutations
    }

    fn assert_all_mutations_are_deltas(base: Fact, mutations: Vec<Fact>) {
        let baseline = BTreeMap::from([(base.clone(), 1)]);
        for mutation in mutations {
            assert_ne!(base, mutation);
            let changed = BTreeMap::from([(mutation.clone(), 1)]);
            assert_eq!(diff_fact_sets(&baseline, &changed), (vec![base.clone()], vec![mutation]));
        }
    }

    fn mutate_payload(base: &Fact, mutation: impl FnOnce(&mut FactPayload)) -> Fact {
        changed(base, |fact| mutation(&mut fact.payload))
    }

    #[test]
    fn deposit_fact_detects_every_identity_and_semantic_mutation() {
        let base = fact(
            EventKind::Deposit,
            FactPayload::Deposit {
                amount_sat: 42,
                receiver: vec![1; 20],
                l2_contract: vec![2; 20],
                call_data: vec![3],
                sender_script: Some(vec![4]),
                encoding: "inscription".into(),
            },
            true,
        );
        let mut mutations = identity_mutations(&base);
        mutations.extend([
            mutate_payload(&base, |payload| match payload {
                FactPayload::Deposit { amount_sat, .. } => *amount_sat += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |payload| match payload {
                FactPayload::Deposit { receiver, .. } => receiver[0] += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |payload| match payload {
                FactPayload::Deposit { l2_contract, .. } => l2_contract[0] += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |payload| match payload {
                FactPayload::Deposit { call_data, .. } => call_data.push(9),
                _ => unreachable!(),
            }),
            mutate_payload(&base, |payload| match payload {
                FactPayload::Deposit { sender_script, .. } => *sender_script = None,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |payload| match payload {
                FactPayload::Deposit { encoding, .. } => *encoding = "op_return".into(),
                _ => unreachable!(),
            }),
        ]);
        assert_all_mutations_are_deltas(base, mutations);
    }

    #[test]
    fn batch_fact_detects_every_identity_and_semantic_mutation() {
        let base = fact(
            EventKind::L1BatchDAReference,
            FactPayload::BatchReference {
                batch_index: 1,
                l1_batch_hash: [2; 32],
                prev_l1_batch_hash: [3; 32],
                da_identifier: "da".into(),
                blob_id: "blob".into(),
            },
            false,
        );
        let mut mutations = identity_mutations(&base);
        mutations.extend([
            mutate_payload(&base, |p| match p {
                FactPayload::BatchReference { batch_index, .. } => *batch_index += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::BatchReference { l1_batch_hash, .. } => l1_batch_hash[0] += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::BatchReference { prev_l1_batch_hash, .. } => prev_l1_batch_hash[0] += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::BatchReference { da_identifier, .. } => da_identifier.push('x'),
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::BatchReference { blob_id, .. } => blob_id.push('x'),
                _ => unreachable!(),
            }),
        ]);
        assert_all_mutations_are_deltas(base, mutations);
    }

    #[test]
    fn proof_fact_detects_every_identity_and_semantic_mutation() {
        let base = fact(
            EventKind::ProofDAReference,
            FactPayload::ProofReference {
                reveal_txid: subject_txid(8).to_string(),
                da_identifier: "da".into(),
                blob_id: "proof".into(),
                batch: batch(10),
            },
            false,
        );
        let mut mutations = identity_mutations(&base);
        mutations.extend([
            mutate_payload(&base, |p| match p {
                FactPayload::ProofReference { reveal_txid, .. } => *reveal_txid = "other".into(),
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::ProofReference { da_identifier, .. } => da_identifier.push('x'),
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::ProofReference { blob_id, .. } => blob_id.push('x'),
                _ => unreachable!(),
            }),
        ]);
        mutations.extend(batch_mutations(&base, |p| match p {
            FactPayload::ProofReference { batch, .. } => batch,
            _ => unreachable!(),
        }));
        assert_all_mutations_are_deltas(base, mutations);
    }

    #[test]
    fn attestation_fact_detects_every_identity_and_semantic_mutation() {
        let base = fact(
            EventKind::ValidatorAttestation,
            FactPayload::Attestation {
                reference_txid: subject_txid(8).to_string(),
                vote: true,
                attester_script: vec![1],
                batch: batch(10),
            },
            false,
        );
        let mut mutations = identity_mutations(&base);
        mutations.extend([
            mutate_payload(&base, |p| match p {
                FactPayload::Attestation { reference_txid, .. } => *reference_txid = "other".into(),
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Attestation { vote, .. } => *vote = false,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Attestation { attester_script, .. } => attester_script.push(2),
                _ => unreachable!(),
            }),
        ]);
        mutations.extend(batch_mutations(&base, |p| match p {
            FactPayload::Attestation { batch, .. } => batch,
            _ => unreachable!(),
        }));
        assert_all_mutations_are_deltas(base, mutations);
    }

    #[test]
    fn bootstrap_fact_detects_every_identity_and_semantic_mutation() {
        let base = fact(
            EventKind::SystemBootstrapping,
            FactPayload::Bootstrap {
                start_height: 10,
                protocol_version: ProtocolVersionTag { minor: 26, patch: 0 },
                bootloader_hash: [1; 32],
                abstract_account_hash: [2; 32],
                snark_wrapper_vk_hash: [3; 32],
                evm_emulator_hash: [4; 32],
                wallets: WalletIdentity {
                    sequencer_script: vec![5],
                    bridge_script: vec![6],
                    governance_script: vec![7],
                    verifier_scripts: vec![vec![8]],
                },
            },
            false,
        );
        let mut mutations = identity_mutations(&base);
        mutations.extend([
            mutate_payload(&base, |p| match p {
                FactPayload::Bootstrap { start_height, .. } => *start_height += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Bootstrap { protocol_version, .. } => protocol_version.patch += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Bootstrap { bootloader_hash, .. } => bootloader_hash[0] += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Bootstrap { abstract_account_hash, .. } => abstract_account_hash[0] += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Bootstrap { snark_wrapper_vk_hash, .. } => snark_wrapper_vk_hash[0] += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Bootstrap { evm_emulator_hash, .. } => evm_emulator_hash[0] += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Bootstrap { wallets, .. } => wallets.sequencer_script.push(1),
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Bootstrap { wallets, .. } => wallets.bridge_script.push(1),
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Bootstrap { wallets, .. } => wallets.governance_script.push(1),
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Bootstrap { wallets, .. } => wallets.verifier_scripts.push(vec![9]),
                _ => unreachable!(),
            }),
        ]);
        assert_all_mutations_are_deltas(base, mutations);
    }

    #[test]
    fn upgrade_proposal_fact_detects_every_identity_and_semantic_mutation() {
        let base = fact(
            EventKind::SystemContractUpgradeProposal,
            FactPayload::UpgradeProposal { proposal: upgrade(10) },
            false,
        );
        let mut mutations = identity_mutations(&base);
        mutations.extend(upgrade_mutations(&base));
        assert_all_mutations_are_deltas(base, mutations);
    }

    #[test]
    fn upgrade_activation_fact_detects_every_identity_and_semantic_mutation() {
        let base = fact(
            EventKind::SystemContractUpgradeActivation,
            FactPayload::UpgradeActivation {
                proposal_txid: subject_txid(9).to_string(),
                resolved_version: ProtocolVersionTag { minor: 27, patch: 0 },
            },
            false,
        );
        let mut mutations = identity_mutations(&base);
        mutations.extend([
            mutate_payload(&base, |p| match p {
                FactPayload::UpgradeActivation { proposal_txid, .. } => *proposal_txid = "other".into(),
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::UpgradeActivation { resolved_version, .. } => resolved_version.patch += 1,
                _ => unreachable!(),
            }),
        ]);
        assert_all_mutations_are_deltas(base, mutations);
    }

    #[test]
    fn withdrawal_fact_detects_every_identity_and_semantic_mutation() {
        let base = fact(
            EventKind::BridgeWithdrawal,
            FactPayload::Withdrawal {
                version: 0,
                total_size: 100,
                v_size: 80,
                inputs: vec![OutPointIdentity { txid: subject_txid(8).to_string(), vout: 1 }],
                output_amount_sat: 50,
                payouts: vec![WithdrawalIdentity {
                    l2_id: [1; 8],
                    l2_tx_event_index: 2,
                    receiver_script: vec![3],
                    amount_sat: 40,
                }],
            },
            false,
        );
        let mut mutations = identity_mutations(&base);
        mutations.extend([
            mutate_payload(&base, |p| match p {
                FactPayload::Withdrawal { version, .. } => *version += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Withdrawal { total_size, .. } => *total_size += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Withdrawal { v_size, .. } => *v_size += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Withdrawal { inputs, .. } => inputs[0].txid = "other".into(),
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Withdrawal { inputs, .. } => inputs[0].vout += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Withdrawal { output_amount_sat, .. } => *output_amount_sat += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Withdrawal { payouts, .. } => payouts[0].l2_id[0] += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Withdrawal { payouts, .. } => payouts[0].l2_tx_event_index += 1,
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Withdrawal { payouts, .. } => payouts[0].receiver_script.push(1),
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::Withdrawal { payouts, .. } => payouts[0].amount_sat += 1,
                _ => unreachable!(),
            }),
        ]);
        assert_all_mutations_are_deltas(base, mutations);
    }

    #[test]
    fn bridge_proposal_fact_detects_every_identity_and_semantic_mutation() {
        let base = fact(
            EventKind::UpdateBridgeProposal,
            FactPayload::BridgeProposal { bridge_script: vec![1], verifier_scripts: vec![vec![2]] },
            false,
        );
        let mut mutations = identity_mutations(&base);
        mutations.extend([
            mutate_payload(&base, |p| match p {
                FactPayload::BridgeProposal { bridge_script, .. } => bridge_script.push(3),
                _ => unreachable!(),
            }),
            mutate_payload(&base, |p| match p {
                FactPayload::BridgeProposal { verifier_scripts, .. } => verifier_scripts.push(vec![4]),
                _ => unreachable!(),
            }),
        ]);
        assert_all_mutations_are_deltas(base, mutations);
    }

    #[test]
    fn rotation_facts_detect_every_identity_and_semantic_mutation() {
        let sequencer = fact(EventKind::WalletRotation, FactPayload::SequencerRotation { new_script: vec![1] }, false);
        let mut sequencer_mutations = identity_mutations(&sequencer);
        sequencer_mutations.push(mutate_payload(&sequencer, |p| match p {
            FactPayload::SequencerRotation { new_script } => new_script.push(2),
            _ => unreachable!(),
        }));
        assert_all_mutations_are_deltas(sequencer, sequencer_mutations);

        let governance =
            fact(EventKind::WalletRotation, FactPayload::GovernanceRotation { new_script: vec![1] }, false);
        let mut governance_mutations = identity_mutations(&governance);
        governance_mutations.push(mutate_payload(&governance, |p| match p {
            FactPayload::GovernanceRotation { new_script } => new_script.push(2),
            _ => unreachable!(),
        }));
        assert_all_mutations_are_deltas(governance, governance_mutations);

        let bridge = fact(
            EventKind::WalletRotation,
            FactPayload::BridgeRotation {
                proposal_txid: subject_txid(9).to_string(),
                new_bridge_script: vec![1],
                new_verifier_scripts: vec![vec![2]],
            },
            false,
        );
        let mut bridge_mutations = identity_mutations(&bridge);
        bridge_mutations.extend([
            mutate_payload(&bridge, |p| match p {
                FactPayload::BridgeRotation { proposal_txid, .. } => *proposal_txid = "other".into(),
                _ => unreachable!(),
            }),
            mutate_payload(&bridge, |p| match p {
                FactPayload::BridgeRotation { new_bridge_script, .. } => new_bridge_script.push(3),
                _ => unreachable!(),
            }),
            mutate_payload(&bridge, |p| match p {
                FactPayload::BridgeRotation { new_verifier_scripts, .. } => new_verifier_scripts.push(vec![4]),
                _ => unreachable!(),
            }),
        ]);
        assert_all_mutations_are_deltas(bridge, bridge_mutations);
    }

    #[test]
    fn multiset_diff_detects_duplicate_occurrences() {
        let fact = fact(
            EventKind::Deposit,
            FactPayload::Deposit {
                amount_sat: 1,
                receiver: vec![1],
                l2_contract: vec![2],
                call_data: vec![],
                sender_script: None,
                encoding: "op_return".into(),
            },
            true,
        );
        assert_eq!(
            diff_fact_sets(&BTreeMap::from([(fact.clone(), 2)]), &BTreeMap::from([(fact.clone(), 1)])),
            (vec![fact], vec![])
        );
    }

    #[test]
    fn divergent_context_fold_produces_context_delta() {
        let kernel = comparison_context(1);
        let legacy = comparison_context(2);
        let delta = context_transition_delta(9, &kernel, &legacy).expect("different contexts must produce a delta");
        assert_eq!(delta.side, DeltaSide::Context);
        assert!(matches!(
            delta.detail,
            DeltaDetail::ContextTransition { kernel: found_kernel, legacy: found_legacy }
                if found_kernel == kernel && found_legacy == legacy
        ));
        assert!(context_transition_delta(9, &kernel, &kernel).is_none());
    }

    fn batch_mutations(base: &Fact, select: impl Copy + Fn(&mut FactPayload) -> &mut BatchIdentity) -> Vec<Fact> {
        vec![
            mutate_payload(base, |p| select(p).reveal_txid = "other".into()),
            mutate_payload(base, |p| select(p).l1_batch_hash[0] += 1),
            mutate_payload(base, |p| select(p).l1_batch_index += 1),
            mutate_payload(base, |p| select(p).prev_l1_batch_hash[0] += 1),
            mutate_payload(base, |p| select(p).da_identifier.push('x')),
            mutate_payload(base, |p| select(p).blob_id.push('x')),
        ]
    }

    fn upgrade_mutations(base: &Fact) -> Vec<Fact> {
        vec![
            mutate_payload(base, |p| select_upgrade(p).version.patch += 1),
            mutate_payload(base, |p| select_upgrade(p).bootloader_code_hash[0] += 1),
            mutate_payload(base, |p| select_upgrade(p).default_account_code_hash[0] += 1),
            mutate_payload(base, |p| select_upgrade(p).evm_emulator_code_hash = None),
            mutate_payload(base, |p| select_upgrade(p).recursion_scheduler_level_vk_hash[0] += 1),
            mutate_payload(base, |p| select_upgrade(p).system_contracts[0].0.push(1)),
            mutate_payload(base, |p| select_upgrade(p).system_contracts[0].1[0] += 1),
        ]
    }

    fn select_upgrade(payload: &mut FactPayload) -> &mut UpgradeIdentity {
        match payload {
            FactPayload::UpgradeProposal { proposal } => proposal,
            _ => unreachable!(),
        }
    }

    fn comparison_context(script_byte: u8) -> ProtocolContext {
        let script = ScriptBuf::from_bytes(vec![script_byte]);
        ProtocolContext {
            version: 1,
            wallets: via_btc_ingestion::WalletSet {
                sequencer: script.clone(),
                bridge: script.clone(),
                governance: script,
                verifiers: vec![],
            },
            protocol_version: ProtocolVersionTag { minor: 26, patch: 0 },
        }
    }
}
