// jscpd:ignore-start Each binary retains its own config, schema, migration and concrete transaction adapter.
use std::{
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use anyhow::Context;
use bitcoin::{hashes::Hash, BlockHash, Txid};
use clap::{Args, Subcommand};
use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, WithdrawalScanMode},
    migration::{
        ChainArchive, EvidenceCensus, GroupCertificate, MessageIdentity, MigrationCertificate,
        Reconstruction, SourcePoint,
    },
    traits::BitcoinOps,
    types::{FullInscriptionMessage, NodeAuth},
};
use via_reorg::reader::ScanGroup;
use zksync_config::{
    configs::{
        via_btc_client::ViaBtcClientConfig, via_consensus::ViaGenesisConfig,
        via_secrets::ViaL1Secrets, DatabaseSecrets,
    },
    PostgresConfig,
};
use zksync_dal::{via_indexer_dal::SemanticMigrationInput, ConnectionPool, Core, CoreDal};
use zksync_env_config::FromEnv;
use zksync_types::via_wallet::SystemWalletsDetails;

type Certificate = MigrationCertificate<SemanticMigrationInput>;

struct PreparedDeposit {
    inclusion: via_reorg::chain::MessageRef<BlockHash, Txid>,
    vout: u32,
    payload: serde_json::Value,
    operation: zksync_types::l1::L1Tx,
    protocol_txid: zksync_types::H256,
}

#[derive(Default)]
struct PreparedWrites {
    deposits: Vec<PreparedDeposit>,
    wallets: Vec<via_btc_client::migration::PreparedWalletUpdate>,
    proofs: Vec<via_btc_client::migration::PreparedProof>,
    upgrades: Vec<via_btc_client::migration::PreparedUpgrade>,
    votes: Vec<(via_btc_client::types::ValidatorAttestation, serde_json::Value)>,
}

#[derive(Debug, Args)]
pub(crate) struct MigrationArgs {
    #[command(subcommand)]
    mode: Mode,
}
#[derive(Debug, Subcommand)]
enum Mode {
    /// Prints a read-only structural certificate; gaps prohibit application.
    Inspect {
        #[arg(long)]
        first: u32,
        #[arg(long)]
        through: u32,
    },
    /// Applies only the exact inspected, complete certificate while all writers are stopped.
    Apply {
        #[arg(long)]
        certificate: PathBuf,
        #[arg(long, required = true)]
        writers_stopped: bool,
        #[arg(long)]
        verified_backup: String,
    },
}

pub(crate) async fn run(
    args: &MigrationArgs,
    config: Option<&Path>,
    secrets: Option<&Path>,
) -> anyhow::Result<()> {
    let (database, source) = if let Some(path) = secrets {
        let secrets: zksync_config::configs::Secrets =
            zksync_core_leftovers::temp_config_store::read_yaml_repr::<
                zksync_protobuf_config::proto::secrets::Secrets,
            >(&path.to_path_buf())?;
        (secrets.database.context("database secrets are absent")?, ViaL1Secrets::from_env()?)
    } else {
        (DatabaseSecrets::from_env()?, ViaL1Secrets::from_env()?)
    };
    let postgres = if let Some(path) = config {
        let config: zksync_config::configs::GeneralConfig =
            zksync_core_leftovers::temp_config_store::read_yaml_repr::<
                zksync_protobuf_config::proto::general::GeneralConfig,
            >(&path.to_path_buf())?;
        config.postgres_config.context("Postgres config is absent")?
    } else {
        PostgresConfig::from_env()?
    };
    let pool = ConnectionPool::<Core>::builder(database.master_url()?, postgres.max_connections()?)
        .build()
        .await?;
    let client = Arc::new(BitcoinClient::new(
        source.rpc_url.expose_str(),
        NodeAuth::UserPass(source.rpc_user, source.rpc_password),
        ViaBtcClientConfig::from_env()?,
    )?);
    match &args.mode {
        Mode::Inspect { first, through } => {
            let bootstrap = ViaGenesisConfig::from_env()?
                .bootstrap_txids()?
                .first()
                .copied()
                .context("bootstrap transaction is absent")?;
            let source = ChainArchive::prepare(&client, *first, *through, bootstrap).await?;
            let mut connection = pool.connection().await?;
            let mut snapshot = connection.transaction_builder()?.set_readonly().build().await?;
            let input = snapshot.via_indexer_dal().semantic_migration_input().await?;
            snapshot.rollback().await?;
            let certificate = inspect(input, source, bootstrap, client.clone()).await?;
            certificate.source.revalidate(&client).await?;
            serde_json::to_writer_pretty(std::io::stdout().lock(), &certificate)?;
            println!();
        }
        Mode::Apply { certificate, writers_stopped, verified_backup } => {
            anyhow::ensure!(
                *writers_stopped && !verified_backup.trim().is_empty(),
                "application requires stopped writers and a verified backup reference"
            );
            anyhow::ensure!(
                std::fs::metadata(certificate)?.len()
                    <= via_btc_client::migration::MAX_CERTIFICATE_BYTES,
                "certificate exceeds the input bound"
            );
            let certificate: Certificate = serde_json::from_slice(&std::fs::read(certificate)?)?;
            apply(&pool, &client, &certificate, verified_backup).await?;
        }
    }
    Ok(())
}

async fn inspect(
    input: SemanticMigrationInput,
    source: ChainArchive,
    bootstrap_txid: Txid,
    client: Arc<BitcoinClient>,
) -> anyhow::Result<Certificate> {
    Ok(prepare(input, source, bootstrap_txid, client).await?.0)
}

async fn prepare(
    input: SemanticMigrationInput,
    source: ChainArchive,
    bootstrap_txid: Txid,
    client: Arc<BitcoinClient>,
) -> anyhow::Result<(Certificate, PreparedWrites)> {
    let bootstrap = source.bootstrap(bootstrap_txid, &client).await?;
    let nonempty_domain = [
        input.census.l2_batches,
        input.census.l2_blocks,
        input.census.delivery_requests,
        input.census.delivery_history,
        input.census.delivery_subjects,
        input.census.snapshots,
        input.census.recovery_barriers,
    ]
    .into_iter()
    .any(|count| count != 0);
    let fresh = !nonempty_domain
        && input.legacy_votes == 0
        && input.deposits.is_empty()
        && input.wallets.is_empty()
        && input.protocols.is_empty()
        && input.canonical.is_empty()
        && input.progress.is_empty()
        && input.reader_evidence == 0;
    let baseline = source
        .headers
        .iter()
        .find(|header| header.height == bootstrap.starting_block_number)
        .context("archive omits the bootstrap completed baseline")?;
    let last = if fresh { baseline } else { source.headers.last().unwrap() };
    let baseline = SourcePoint { height: baseline.height, hash: baseline.header.block_hash() };
    let completed = SourcePoint { height: last.height, hash: last.header.block_hash() };
    let mut groups = ScanGroup::ALL
        .into_iter()
        .map(|group| GroupCertificate {
            group: group.as_str().into(),
            baseline,
            completed,
            reconstruction: if fresh { Reconstruction::Fresh } else { Reconstruction::Retained },
            evidence: EvidenceCensus {
                blocks: completed.height - baseline.height,
                ..EvidenceCensus::default()
            },
            gaps: vec![],
        })
        .collect::<Vec<_>>();
    let mut conflicts = vec![];
    let migrations = sqlx::migrate!("../../lib/dal/migrations");
    if migrations
        .iter()
        .filter(|migration| {
            migration.version < 20261023030434 && !migration.migration_type.is_down_migration()
        })
        .any(|migration| {
            !input.schema.iter().any(|revision| {
                revision.version == migration.version
                    && revision.success
                    && revision.checksum == migration.checksum.as_ref()
            })
        })
    {
        conflicts.push("baseline schema is incomplete or its checksum differs; upgrade only the existing schema before inspection".into());
    }
    if input.progress.iter().any(|position| {
        position.module == "via_btc_watch" && position.height < i64::from(baseline.height)
    }) {
        conflicts.push("legacy reader is below the bootstrap baseline; an explicit proved reconstruction is required".into());
    }

    if input.schema.iter().any(|revision| !revision.success) {
        conflicts.push("database has a failed schema migration".into());
    }
    if input.reader_certificates != 0 {
        conflicts.push("semantic certificates already exist; resolve their retained identities before preparing another application".into());
    }
    if input.reader_positions != 0 {
        conflicts.push("reader format already has active positions".into());
    }
    if input.reader_evidence != 0 {
        conflicts.push("retained reader evidence requires full domain reconciliation".into());
    }
    if input.progress.iter().any(|position| position.height > i64::from(completed.height)) {
        conflicts.push("archive ends below a retained module position".into());
    }
    for retained in &input.canonical {
        if !source.matches_persisted_source(retained.height, &retained.hash) {
            conflicts.push(format!(
                "retained canonical header {} has no matching checked source",
                retained.height
            ));
        }
    }
    let details = SystemWalletsDetails::try_from(&bootstrap)?;
    if nonempty_domain {
        for group in &mut groups {
            group.gaps.push(
                "nonempty retained domain requires its full semantic replay/reconciliation".into(),
            );
        }
    }
    let mut indexer =
        BitcoinInscriptionIndexer::new(client.clone(), Arc::new(bootstrap.wallets.clone()))
            .with_withdrawal_mode(WithdrawalScanMode::Required);
    let mut writes = PreparedWrites::default();
    let mut current_wallets = bootstrap.wallets.clone();
    let mut proof_prefix = via_btc_client::migration::MigrationProofPrefix::default();
    let mut matched = std::collections::BTreeSet::new();
    let mut census_bytes = 0_u64;
    for header in source
        .headers
        .iter()
        .filter(|block| block.height > baseline.height && block.height <= completed.height)
    {
        let block = header.checked_block(&client).await?;
        let messages = indexer.process_checked_block(block.height, &block.block).await?;
        let block_proofs = proof_prefix.prepare_block(&mut indexer, &messages).await?;
        let prefix = if messages
            .iter()
            .any(|message| matches!(message, FullInscriptionMessage::UpdateBridge(_)))
        {
            proof_prefix
                .rotation_admission(via_consensus::consensus::BATCH_FINALIZATION_THRESHOLD)?
        } else {
            via_reorg::configuration::RotationAdmission::Accepted
        };
        let updates =
            via_btc_client::migration::prepare_wallet_updates(&client, &messages, prefix).await?;
        for message in messages {
            if matches!(
                message,
                FullInscriptionMessage::SystemBootstrapping(_)
                    | FullInscriptionMessage::L1BatchDAReference(_)
                    | FullInscriptionMessage::UpdateBridgeProposal(_)
                    | FullInscriptionMessage::SystemContractUpgradeProposal(_)
            ) {
                continue;
            }
            let index = match &message {
                FullInscriptionMessage::L1ToL2Message(_) => 1,
                FullInscriptionMessage::ProofDAReference(_)
                | FullInscriptionMessage::ValidatorAttestation(_) => 2,
                FullInscriptionMessage::BridgeWithdrawal(_) => 3,
                _ => 0,
            };
            let inclusion = message.common().inclusion()?;
            let payload = if matches!(message, FullInscriptionMessage::ProofDAReference(_)) {
                block_proofs
                    .iter()
                    .find(|proof| proof.inclusion == inclusion)
                    .context("prepared linked proof is absent")?
                    .payload
                    .clone()
            } else {
                via_btc_client::indexer::replay::semantic_payload(&message)?
            };
            let identity = MessageIdentity {
                source: SourcePoint { height: block.height, hash: block.block.block_hash() },
                carrier: inclusion.carrier_txid,
                transaction_index: inclusion.tx_index,
                message_index: inclusion.message_index,
                payload: payload.clone(),
            };
            census_bytes += u64::try_from(serde_json::to_vec(&identity)?.len())?;
            anyhow::ensure!(
                census_bytes <= via_btc_client::migration::MAX_CERTIFICATE_BYTES,
                "decoded migration census exceeds its bound"
            );
            groups[index].evidence.messages.push(identity);
            if let FullInscriptionMessage::L1ToL2Message(message) = message {
                let deposit = zksync_types::l1::via_l1::ViaL1Deposit {
                    l2_receiver_address: message.input.receiver_l2_address,
                    amount: message.amount.to_sat(),
                    calldata: message.input.call_data,
                    l1_block_number: u64::from(block.height),
                    tx_index: usize::try_from(inclusion.tx_index)?,
                    output_vout: message.common.output_vout.context("deposit output is absent")?,
                };
                let Some(operation) = deposit.l1_tx() else { continue };
                let protocol_txid =
                    zksync_types::H256::from_str(&message.common.tx_id.to_string())?;
                if let Some(retained) = input
                    .deposits
                    .iter()
                    .find(|retained| retained.hash == operation.hash().as_bytes())
                {
                    matched.insert(retained.hash.clone());
                    if !retained.source_equivalent(&operation, protocol_txid) {
                        groups[1].gaps.push(format!(
                            "deposit {} differs from its complete checked source payload",
                            operation.serial_id().0
                        ));
                    }
                    if retained.protected_execution
                        || retained.l1_batch_number.is_some()
                        || retained.miniblock_number.is_some()
                    {
                        groups[1].evidence.protected_records += 1;
                        groups[1].gaps.push(format!(
                            "deposit {} requires executed L2 input reconciliation",
                            operation.serial_id().0
                        ));
                    }
                    groups[1].evidence.retained_records += 1;
                } else {
                    groups[1].evidence.rebuilt_records += 1;
                }
                groups[1].reconstruction = Reconstruction::RebuiltReversible;
                writes.deposits.push(PreparedDeposit {
                    inclusion,
                    vout: u32::try_from(deposit.output_vout)?,
                    payload,
                    operation,
                    protocol_txid,
                });
            } else if matches!(
                message,
                FullInscriptionMessage::UpdateGovernance(_)
                    | FullInscriptionMessage::UpdateSequencer(_)
                    | FullInscriptionMessage::UpdateBridge(_)
            ) {
                let update = updates
                    .iter()
                    .find(|update| update.inclusion == inclusion)
                    .context("prepared wallet update is absent")?;
                groups[0].evidence.messages.last_mut().unwrap().payload = serde_json::json!({
                    "message":update.payload,"decision":update.decision,"roles":update.roles,
                });
                census_bytes -= u64::try_from(serde_json::to_vec(&payload)?.len())?;
                census_bytes += u64::try_from(
                    serde_json::to_vec(&groups[0].evidence.messages.last().unwrap().payload)?.len(),
                )?;
                anyhow::ensure!(
                    census_bytes <= via_btc_client::migration::MAX_CERTIFICATE_BYTES,
                    "decoded migration census exceeds its bound"
                );
                if update.decision == via_btc_client::migration::WalletDecision::NotReady {
                    groups[0]
                        .gaps
                        .push("rotation requires the complete historical proof/vote prefix".into());
                }
            } else if let FullInscriptionMessage::SystemContractUpgrade(approval) = message {
                let upgrade =
                    via_btc_client::migration::prepare_upgrade(&client, &approval).await?;
                groups[0].evidence.messages.last_mut().unwrap().payload = upgrade.payload.clone();
                writes.upgrades.push(upgrade);
                groups[0].reconstruction = Reconstruction::RebuiltReversible;
            } else if let FullInscriptionMessage::ValidatorAttestation(vote) = message {
                writes.votes.push((vote, payload));
                groups[2].reconstruction = Reconstruction::RebuiltReversible;
            } else if matches!(message, FullInscriptionMessage::ProofDAReference(_)) {
                groups[2].reconstruction = Reconstruction::RebuiltReversible;
            } else {
                groups[index].gaps.push("decoded source effect requires semantic replay".into());
            }
        }
        for update in &updates {
            if update.decision == via_btc_client::migration::WalletDecision::Accepted {
                for (role, info) in &update.details.0 {
                    use zksync_types::via_wallet::WalletRole;
                    match role {
                        WalletRole::Sequencer => {
                            current_wallets.sequencer = info.addresses[0].clone()
                        }
                        WalletRole::Gov => current_wallets.governance = info.addresses[0].clone(),
                        WalletRole::Bridge => current_wallets.bridge = info.addresses[0].clone(),
                        WalletRole::Verifier => current_wallets.verifiers = info.addresses.clone(),
                    }
                }
            }
        }
        indexer.update_system_wallets(
            Some(current_wallets.sequencer.clone()),
            Some(current_wallets.bridge.clone()),
            Some(current_wallets.verifiers.clone()),
            Some(current_wallets.governance.clone()),
        );
        writes.wallets.extend(updates);
        writes.proofs.extend(block_proofs);
    }
    for wallet in &input.wallets {
        let bootstrap_match = details.0.iter().any(|(role, info)| {
            role.to_string() == wallet.role
                && info.txid.to_string() == wallet.tx_hash
                && info.addresses.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")
                    == wallet.address
        });
        let source_match = writes.wallets.iter().any(|update| {
            update.details.0.iter().any(|(role, info)| {
                role.to_string() == wallet.role
                    && info.txid.to_string() == wallet.tx_hash
                    && info.addresses.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")
                        == wallet.address
            })
        });
        if !bootstrap_match && !source_match {
            conflicts.push(format!(
                "wallet {} has no matching complete checked source content",
                wallet.id
            ));
        }
    }
    if !writes.wallets.is_empty() {
        groups[0].reconstruction = Reconstruction::RebuiltReversible;
        groups[0].evidence.retained_records = u64::try_from(input.wallets.len())?;
        groups[0].evidence.rebuilt_records = u64::try_from(
            writes
                .wallets
                .iter()
                .flat_map(|update| update.details.0.iter())
                .filter(|(role, info)| {
                    !input.wallets.iter().any(|retained| {
                        retained.role == role.to_string()
                            && retained.tx_hash == info.txid.to_string()
                            && retained.address
                                == info
                                    .addresses
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect::<Vec<_>>()
                                    .join(",")
                    })
                })
                .count(),
        )?;
    }
    if matched.len() != input.deposits.len() {
        groups[1].gaps.push("retained deposits outside the proved source interval require explicit reconstruction or protected L2 reconciliation".into());
    }
    for retained in &input.votes {
        let matching = writes.votes.iter().find(|(vote, _)| {
            zksync_types::H256::from_str(&vote.input.reference_txid.to_string())
                .ok()
                .is_some_and(|reference| reference.as_bytes() == retained.proof)
                && vote
                    .common
                    .p2wpkh_address
                    .as_ref()
                    .is_some_and(|address| address.to_string() == retained.verifier)
                && matches!(vote.input.attestation, via_btc_client::types::Vote::Ok)
                    == retained.approved
                && writes.proofs.iter().any(|proof| {
                    proof.identity == vote.input.reference_txid
                        && i64::from(proof.batch.l1_batch_index.0) == retained.batch
                })
        });
        if matching.is_none() {
            groups[2]
                .gaps
                .push("retained vote has no matching authenticated source / exact proof".into());
        }
    }
    groups[2].evidence.retained_records = u64::try_from(input.votes.len())?;
    let bootstrap_timestamp = input
        .protocols
        .iter()
        .find(|protocol| {
            protocol.minor == bootstrap.protocol_version.minor as i32
                && protocol.patch == bootstrap.protocol_version.patch.0 as i32
        })
        .and_then(|protocol| protocol.timestamp)
        .unwrap_or(0);
    let protocols =
        source_protocols(&bootstrap, &writes.upgrades, u64::try_from(bootstrap_timestamp)?)?;
    for retained in &input.protocols {
        let expected = protocols.iter().find(|version| {
            version.version.minor as i32 == retained.minor
                && version.version.patch.0 as i32 == retained.patch
        });
        if !expected.is_some_and(|version| {
            retained.timestamp == i64::try_from(version.timestamp).ok()
                && retained.bootloader == version.base_system_contracts_hashes.bootloader.as_bytes()
                && retained.default_account
                    == version.base_system_contracts_hashes.default_aa.as_bytes()
                && retained.evm_emulator
                    == version
                        .base_system_contracts_hashes
                        .evm_emulator
                        .map(|hash| hash.as_bytes().to_vec())
                && retained.verifier_key
                    == version.l1_verifier_config.snark_wrapper_vk_hash.as_bytes()
                && retained.upgrade_tx_hash
                    == version.tx.as_ref().map(|tx| tx.common_data.hash().as_bytes().to_vec())
        }) {
            groups[0].gaps.push(format!(
                "protocol {}.{} differs from its complete checked source catalogue",
                retained.minor, retained.patch
            ));
        }
    }
    for retained in &input.upgrades {
        let expected = protocols
            .iter()
            .filter_map(|version| version.tx.as_ref())
            .find(|tx| tx.common_data.hash().as_bytes() == retained.hash);
        if !expected.is_some_and(|tx| retained.source_equivalent(tx)) {
            groups[0]
                .gaps
                .push("system upgrade transaction differs from its complete source payload".into());
        }
        if retained.protected_execution {
            groups[0].evidence.protected_records += 1;
            groups[0].gaps.push(
                "system upgrade transaction requires protected L2 execution reconciliation".into(),
            );
        }
    }
    groups[2].evidence.rebuilt_records = u64::try_from(writes.proofs.len() + writes.votes.len())?;
    for group in &mut groups {
        group.gaps.sort();
        group.gaps.dedup();
    }
    let certificate = Certificate {
        format_version: 1,
        role: "main".into(),
        bootstrap_txid,
        source,
        database_input: input,
        stores: vec![],
        groups,
        conflicts,
    };
    certificate.validate()?;
    anyhow::ensure!(
        u64::try_from(serde_json::to_vec(&certificate)?.len())?
            <= via_btc_client::migration::MAX_CERTIFICATE_BYTES,
        "complete semantic certificate exceeds its bound; choose an explicitly proved baseline"
    );
    Ok((certificate, writes))
}

fn source_protocols(
    bootstrap: &zksync_types::via_bootstrap::BootstrapState,
    upgrades: &[via_btc_client::migration::PreparedUpgrade],
    bootstrap_timestamp: u64,
) -> anyhow::Result<Vec<zksync_types::protocol_upgrade::ProtocolVersion>> {
    use zksync_types::{
        protocol_upgrade::ProtocolVersion, via_protocol_upgrade::ViaProtocolUpgrade,
        ProtocolUpgrade,
    };
    let mut catalogue = vec![(
        bootstrap.starting_block_number,
        ProtocolVersion {
            version: bootstrap.protocol_version,
            timestamp: bootstrap_timestamp,
            l1_verifier_config: zksync_types::protocol_version::L1VerifierConfig {
                snark_wrapper_vk_hash: bootstrap.snark_wrapper_vk_hash,
            },
            base_system_contracts_hashes: bootstrap.base_system_contracts_hashes(),
            tx: None,
        },
    )];
    for upgrade in upgrades {
        let height = upgrade.inclusion.block.height;
        let proposal = &upgrade.proposal;
        let governing = catalogue
            .iter()
            .filter(|(source, _)| *source < height)
            .max_by_key(|(_, version)| version.version)
            .context("upgrade has no historical code configuration")?
            .1
            .version;
        let base_minor =
            if proposal.version <= governing { proposal.version.minor } else { governing.minor };
        let base = &catalogue
            .iter()
            .filter(|(source, version)| *source < height && version.version.minor == base_minor)
            .max_by_key(|(_, version)| version.version)
            .context("upgrade base version is absent")?
            .1;
        let transaction = ViaProtocolUpgrade::default()
            .create_protocol_upgrade_tx(proposal.version, proposal.system_contracts.clone())?;
        let version = base.apply_upgrade(
            ProtocolUpgrade {
                version: proposal.version,
                bootloader_code_hash: Some(proposal.bootloader_code_hash),
                default_account_code_hash: Some(proposal.default_account_code_hash),
                evm_emulator_code_hash: proposal.evm_emulator_code_hash,
                tx: Some(transaction),
                timestamp: 0,
                verifier_address: None,
                verifier_params: None,
            },
            Some(proposal.recursion_scheduler_level_vk_hash),
        );
        catalogue.push((height, version));
    }
    Ok(catalogue.into_iter().map(|(_, version)| version).collect())
}

async fn apply(
    pool: &ConnectionPool<Core>,
    client: &Arc<BitcoinClient>,
    certificate: &Certificate,
    backup: &str,
) -> anyhow::Result<()> {
    certificate.require_complete()?;
    anyhow::ensure!(certificate.role == "main", "certificate belongs to another node role");
    anyhow::ensure!(
        client.get_network() == certificate.source.network,
        "certificate source network differs from configuration"
    );
    let mut connection = pool.connection().await?;
    let mut snapshot = connection.transaction_builder()?.set_readonly().build().await?;
    let already = applied_certificates(&mut snapshot, certificate, backup).await?;
    snapshot.rollback().await?;
    if let Some(applied) = already {
        for (group, id) in applied {
            println!("{group} {id}");
        }
        return Ok(());
    }
    let (prepared, writes) = prepare(
        certificate.database_input.clone(),
        certificate.source.clone(),
        certificate.bootstrap_txid,
        client.clone(),
    )
    .await?;
    anyhow::ensure!(
        prepared == *certificate,
        "certificate content differs from independent preparation"
    );
    let PreparedWrites { deposits, wallets, proofs, votes, upgrades } = writes;
    let bootstrap = certificate.source.bootstrap(certificate.bootstrap_txid, client).await?;
    certificate.source.revalidate(client).await?;
    let mut tx = connection.start_transaction().await?;
    tx.via_indexer_dal().lock_btc_watch().await?;
    if let Some(applied) = applied_certificates(&mut tx, certificate, backup).await? {
        tx.rollback().await?;
        for (group, id) in applied {
            println!("{group} {id}");
        }
        return Ok(());
    }
    let current = tx.via_indexer_dal().semantic_migration_input().await?;
    anyhow::ensure!(
        current == certificate.database_input,
        "stale semantic migration input; inspect again"
    );
    // SQLx owns migration transactions and checksum records; checked Rust hashes supply legacy conversion.
    let mut migrator = sqlx::migrate!("../../lib/dal/migrations");
    let later = migrator
        .migrations
        .iter()
        .filter(|migration| migration.version > 20261023030434)
        .cloned()
        .collect::<Vec<_>>();
    migrator.migrations = std::borrow::Cow::Owned(
        migrator
            .migrations
            .iter()
            .filter(|migration| migration.version <= 20261023030434)
            .cloned()
            .collect(),
    );
    migrator.set_ignore_missing(true);
    migrator.run_direct(tx.conn()).await?;
    sqlx::query("CREATE TEMP TABLE via_btc_legacy_hashes(display TEXT PRIMARY KEY,hash BYTEA NOT NULL) ON COMMIT DROP").execute(tx.conn()).await?;
    for block in &certificate.source.headers {
        let hash = block.header.block_hash();
        sqlx::query("INSERT INTO via_btc_headers(hash,height,parent_hash) VALUES($1,$2,$3) ON CONFLICT(hash) DO UPDATE SET hash=CASE WHEN via_btc_headers.height=EXCLUDED.height AND via_btc_headers.parent_hash=EXCLUDED.parent_hash THEN via_btc_headers.hash ELSE NULL END")
            .bind(hash.as_byte_array().as_slice()).bind(i64::from(block.height)).bind(block.header.prev_blockhash.as_byte_array().as_slice()).execute(tx.conn()).await?;
        sqlx::query("INSERT INTO pg_temp.via_btc_legacy_hashes(display,hash) VALUES($1,$2)")
            .bind(hash.to_string())
            .bind(hash.as_byte_array().as_slice())
            .execute(tx.conn())
            .await?;
    }
    // Run the complete known migration list so SQLx also validates every already applied checksum.
    migrator.migrations.to_mut().extend(later);
    migrator.set_ignore_missing(false);
    migrator.run_direct(tx.conn()).await?;
    for header in &certificate.source.headers {
        tx.via_l1_block_dal()
            .accept_header(via_reorg::chain::Header {
                height: header.height,
                block_hash: header.header.block_hash(),
                parent_hash: header.header.prev_blockhash,
            })
            .await?;
    }
    for wallet in &certificate.database_input.wallets {
        let bootstrap_origin =
            SystemWalletsDetails::try_from(&bootstrap)?.0.iter().any(|(role, info)| {
                role.to_string() == wallet.role
                    && info.txid.to_string() == wallet.tx_hash
                    && info.addresses.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")
                        == wallet.address
            });
        if !bootstrap_origin {
            let source = wallets
                .iter()
                .find(|update| {
                    update.details.0.iter().any(|(role, info)| {
                        role.to_string() == wallet.role
                            && info.txid.to_string() == wallet.tx_hash
                            && info
                                .addresses
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join(",")
                                == wallet.address
                    })
                })
                .context("legacy wallet no longer matches prepared source")?;
            let payload = serde_json::json!({"role":wallet.role,"addresses":wallet.address,"origin":"bitcoin","message":source.payload});
            let written = sqlx::query("UPDATE via_wallets SET origin_kind='bitcoin',l1_block_number=0,message_index=$2,immutable_payload=CASE WHEN origin_kind='legacy' THEN $3 ELSE via_btc_immutable_payload(immutable_payload,$3) END WHERE id=$1 AND origin_kind IN('legacy','bitcoin')")
                .bind(wallet.id).bind(i64::from(source.inclusion.message_index)).bind(payload).execute(tx.conn()).await?;
            anyhow::ensure!(
                written.rows_affected() == 1,
                "legacy wallet source classification changed"
            );
            continue;
        }
        let payload = serde_json::json!({"role":wallet.role,"addresses":wallet.address,"origin":"bootstrap","baseline":bootstrap.starting_block_number});
        let written=sqlx::query("UPDATE via_wallets SET origin_kind='bootstrap',l1_block_number=$2,immutable_payload=CASE WHEN origin_kind='legacy' THEN $3 ELSE via_btc_immutable_payload(immutable_payload,$3) END WHERE id=$1 AND origin_kind IN('legacy','bootstrap')")
            .bind(wallet.id).bind(i64::from(bootstrap.starting_block_number)).bind(payload).execute(tx.conn()).await?;
        anyhow::ensure!(
            written.rows_affected() == 1,
            "wallet classification changed during semantic migration"
        );
    }
    tx.via_wallet_dal()
        .insert_bootstrap_wallets(
            &SystemWalletsDetails::try_from(&bootstrap)?,
            i64::from(bootstrap.starting_block_number),
        )
        .await?;
    let timestamp = certificate
        .database_input
        .protocols
        .iter()
        .find(|protocol| {
            protocol.minor == bootstrap.protocol_version.minor as i32
                && protocol.patch == bootstrap.protocol_version.patch.0 as i32
        })
        .and_then(|protocol| protocol.timestamp)
        .unwrap_or(0);
    let protocols = source_protocols(&bootstrap, &upgrades, u64::try_from(timestamp)?)?;
    let payload = serde_json::json!({"timestamp":timestamp,"bootloader_code_hash":bootstrap.bootloader_hash,"default_account_code_hash":bootstrap.abstract_account_hash,
        "evm_emulator_code_hash":bootstrap.base_system_contracts_hashes().evm_emulator,"upgrade_tx_hash":Option::<zksync_types::H256>::None,"snark_wrapper_vk_hash":bootstrap.snark_wrapper_vk_hash});
    sqlx::query("UPDATE protocol_patches SET immutable_payload=CASE WHEN immutable_payload IS NULL THEN $3 ELSE via_btc_immutable_payload(immutable_payload,$3) END WHERE minor=$1 AND patch=$2")
        .bind(bootstrap.protocol_version.minor as i32).bind(bootstrap.protocol_version.patch.0 as i32).bind(payload).execute(tx.conn()).await?;
    tx.protocol_versions_dal().save_protocol_version_with_tx(&protocols[0]).await?;
    tx.protocol_versions_dal()
        .mark_via_bootstrap_version(
            bootstrap.protocol_version,
            bootstrap.base_system_contracts_hashes(),
            zksync_types::protocol_version::L1VerifierConfig {
                snark_wrapper_vk_hash: bootstrap.snark_wrapper_vk_hash,
            },
        )
        .await?;
    for (upgrade, version) in upgrades.iter().zip(protocols.iter().skip(1)) {
        tx.protocol_versions_dal()
            .record_via_upgrade_origin(
                upgrade.proposal.version,
                upgrade.identity,
                &upgrade.payload,
                upgrade.inclusion,
            )
            .await?;
        // Legacy patches are classified only after the complete read-only source comparison.
        let payload = serde_json::json!({"timestamp":version.timestamp,"bootloader_code_hash":version.base_system_contracts_hashes.bootloader,
            "default_account_code_hash":version.base_system_contracts_hashes.default_aa,"evm_emulator_code_hash":version.base_system_contracts_hashes.evm_emulator,
            "upgrade_tx_hash":version.tx.as_ref().map(|tx| tx.common_data.hash()),"snark_wrapper_vk_hash":version.l1_verifier_config.snark_wrapper_vk_hash});
        sqlx::query("UPDATE protocol_patches SET immutable_payload=CASE WHEN immutable_payload IS NULL THEN $3 ELSE via_btc_immutable_payload(immutable_payload,$3) END WHERE minor=$1 AND patch=$2")
            .bind(version.version.minor as i32).bind(version.version.patch.0 as i32).bind(payload).execute(tx.conn()).await?;
        tx.protocol_versions_dal().save_protocol_version_with_tx(version).await?;
    }
    for update in wallets {
        let decision =
            update.decision.persisted().context("not-ready configuration cannot be applied")?;
        if update.decision == via_btc_client::migration::WalletDecision::BadUpdate {
            tx.via_wallet_dal()
                .record_wallet_rejection(
                    update.identity,
                    &update.roles,
                    update.inclusion,
                    &update.payload,
                )
                .await?;
        } else {
            tx.via_wallet_dal()
                .record_wallet_update(&update.details, update.inclusion, &update.payload, decision)
                .await?;
        }
    }
    for deposit in deposits {
        tx.via_transactions_dal()
            .record_deposit_evidence(
                deposit.inclusion.carrier_txid,
                deposit.vout,
                &deposit.payload,
                deposit.inclusion,
                deposit.operation.hash(),
            )
            .await?;
        tx.via_transactions_dal()
            .insert_transaction_l1(
                &deposit.operation,
                deposit.operation.eth_block(),
                deposit.protocol_txid,
            )
            .await?;
    }
    for proof in proofs {
        tx.via_votes_dal()
            .record_reader_proof(
                proof.identity,
                proof.batch.l1_batch_index.0,
                &proof.payload,
                proof.inclusion,
            )
            .await?;
    }
    for (vote, payload) in votes {
        tx.via_votes_dal()
            .record_reader_vote(
                vote.common.tx_id,
                vote.input.reference_txid,
                &vote
                    .common
                    .p2wpkh_address
                    .as_ref()
                    .context("prepared vote has no authenticated verifier")?
                    .to_string(),
                matches!(vote.input.attestation, via_btc_client::types::Vote::Ok),
                &payload,
                vote.common.inclusion()?,
            )
            .await?;
    }
    tx.via_votes_dal()
        .recompute_reader_finalization(
            certificate.groups[2].completed.height,
            via_consensus::consensus::BATCH_FINALIZATION_THRESHOLD,
        )
        .await?;
    let mut applied = Vec::new();
    for group in &certificate.groups {
        let body = stored_group_body(certificate, group, backup);
        anyhow::ensure!(
            serde_json::to_vec(&body)?.len()
                <= via_btc_client::migration::MAX_CERTIFICATE_BYTES as usize,
            "persisted certificate exceeds its structural bound"
        );
        let id:String=sqlx::query_scalar("INSERT INTO via_btc_migration_certificates(format_version,network,genesis,scope,body,application_status,applied_at) VALUES(1,$1,$2,$3,$4,'applied',NOW()) RETURNING id::TEXT")
            .bind(certificate.source.network.to_string()).bind(certificate.source.genesis.as_byte_array().as_slice())
            .bind(&group.group).bind(body).fetch_one(tx.conn()).await?;
        applied.push((group.group.clone(), id));
    }
    tx.commit().await?;
    for (group, id) in applied {
        println!("{group} {id}");
    }
    Ok(())
}
async fn applied_certificates(
    storage: &mut zksync_dal::Connection<'_, Core>,
    certificate: &Certificate,
    backup: &str,
) -> anyhow::Result<Option<Vec<(String, String)>>> {
    let identity:(String,i64,String)=sqlx::query_as("SELECT current_database(),(SELECT oid::BIGINT FROM pg_database WHERE datname=current_database()),(SELECT system_identifier::TEXT FROM pg_control_system())")
        .fetch_one(storage.conn()).await?;
    let input = &certificate.database_input;
    anyhow::ensure!(
        (identity.0.as_str(), identity.1, identity.2.as_str())
            == (input.database.as_str(), input.database_oid, input.system_identifier.as_str()),
        "certificate database identity differs from target"
    );
    let reader_schema: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version=20261023041443 AND success)",
    )
    .fetch_one(storage.conn())
    .await?;
    if !reader_schema {
        return Ok(None);
    }
    let mut applied = Vec::new();
    for group in &certificate.groups {
        let ids:Vec<String>=sqlx::query_scalar("SELECT id::TEXT FROM via_btc_migration_certificates WHERE scope=$1 AND body=$2 AND application_status='applied' AND format_version=1 AND network=$3 AND genesis=$4 ORDER BY id")
            .bind(&group.group).bind(stored_group_body(certificate,group,backup))
            .bind(certificate.source.network.to_string()).bind(certificate.source.genesis.as_byte_array().as_slice())
            .fetch_all(storage.conn()).await?;
        anyhow::ensure!(ids.len() <= 1, "duplicate committed semantic certificate");
        if let Some(id) = ids.into_iter().next() {
            applied.push((group.group.clone(), id));
        }
    }
    if applied.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(applied.len() == 4, "partial committed semantic certificate set");
    Ok(Some(applied))
}
fn stored_group_body(
    certificate: &Certificate,
    group: &GroupCertificate,
    backup: &str,
) -> serde_json::Value {
    serde_json::json!({"database_input":certificate.database_input,"stores":certificate.stores,"group":group,
        "bootstrap_txid":certificate.bootstrap_txid,"source":certificate.source,"verified_backup":backup})
}
// jscpd:ignore-end

#[cfg(test)]
mod tests {
    use super::*;

    // jscpd:ignore-start Each stopped binary rehearses its actual SQLx schema, source boundary and certificate transaction.
    #[tokio::test]
    async fn proved_legacy_bootstrap_migrates_atomically_and_exact_retry_allocates_nothing(
    ) -> anyhow::Result<()> {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let mut connection = pool.connection().await?;
        sqlx::migrate!("../../lib/dal/migrations").undo(connection.conn(), 20261023030434).await?;
        let mut bootstrap = via_test_utils::utils::bootstrap_state_mock();
        bootstrap.starting_block_number = 2;
        bootstrap.protocol_version.minor = zksync_types::ProtocolVersionId::Version27;
        let key = bitcoin::secp256k1::Keypair::from_secret_key(
            &bitcoin::secp256k1::Secp256k1::new(),
            &bitcoin::secp256k1::SecretKey::from_slice(&[1; 32])?,
        );
        let sender = bitcoin::Address::p2wpkh(
            &bitcoin::CompressedPublicKey(key.public_key()),
            bitcoin::Network::Regtest,
        );
        bootstrap.wallets.sequencer = sender.clone();
        bootstrap.wallets.verifiers = vec![sender];
        let inscription = |kind: &[u8], fields: &[Vec<u8>], nonce: u32| {
            via_test_utils::inscription::parser_transaction(key, kind, fields, nonce)
        };
        let batch = inscription(
            via_btc_client::types::L1_BATCH_DA_REFERENCE_MSG.as_bytes(),
            &[
                vec![5; 32],
                1_u32.to_be_bytes().to_vec(),
                b"test-da".to_vec(),
                b"test-batch".to_vec(),
                vec![0; 32],
            ],
            1,
        );
        let proof = inscription(
            via_btc_client::types::PROOF_DA_REFERENCE_MSG.as_bytes(),
            &[
                batch.compute_txid().as_byte_array().to_vec(),
                b"test-da".to_vec(),
                b"test-proof".to_vec(),
            ],
            2,
        );
        let proof_identity: Txid = proof.compute_ntxid().into();
        let upgrade_version = zksync_types::protocol_version::ProtocolSemanticVersion::new(
            zksync_types::ProtocolVersionId::Version28,
            zksync_types::protocol_version::VersionPatch(0),
        );
        let mut packed_version = [0; 32];
        upgrade_version.pack().to_big_endian(&mut packed_version);
        let proposal = inscription(
            via_btc_client::types::SYSTEM_CONTRACT_UPGRADE_MSG.as_bytes(),
            &[packed_version.to_vec(), vec![2; 32], vec![3; 32], vec![4; 32]],
            4,
        );
        let vote = inscription(
            via_btc_client::types::VALIDATOR_ATTESTATION_MSG.as_bytes(),
            &[proof_identity.as_byte_array().to_vec(), b"OP_1".to_vec()],
            3,
        );
        let transaction = via_test_utils::utils::bootstrap_transaction(
            &bootstrap,
            via_btc_client::types::SYSTEM_BOOTSTRAPPING_MSG.as_bytes(),
        );
        let identity = transaction.compute_txid();
        bootstrap.bootstrap_tx_id = transaction.compute_ntxid().into();
        let mut chain = via_test_utils::chain::Chain::default();
        let mut block = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
        block.txdata = vec![transaction];
        chain.insert(1, block);
        let governance_parent = bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn::default()],
            output: vec![
                bitcoin::TxOut {
                    value: bitcoin::Amount::from_sat(10_000),
                    script_pubkey: bootstrap.wallets.governance.script_pubkey()
                };
                2
            ],
        };
        let governance_parent_id = governance_parent.compute_txid();
        let mut block = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
        block.txdata = vec![governance_parent, batch, proposal.clone()];
        chain.insert(2, block);
        let mut block = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
        block.txdata = vec![bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn::default()],
            output: vec![
                bitcoin::TxOut {
                    value: bitcoin::Amount::from_sat(50_000),
                    script_pubkey: bootstrap.wallets.bridge.script_pubkey(),
                },
                bitcoin::TxOut {
                    value: bitcoin::Amount::ZERO,
                    script_pubkey: bitcoin::ScriptBuf::new_op_return([7; 20]),
                },
            ],
        }];
        chain.insert(3, block);
        let sequencer = via_test_utils::utils::test_verifier_add_1();
        let mut payload = b"VIA_PROTOCOL:SEQ".to_vec();
        payload.push(1);
        payload.extend(sequencer.to_string().as_bytes());
        let rotation = bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: bitcoin::OutPoint { txid: governance_parent_id, vout: 0 },
                ..bitcoin::TxIn::default()
            }],
            output: vec![
                bitcoin::TxOut {
                    value: bitcoin::Amount::from_sat(9_000),
                    script_pubkey: bootstrap.wallets.governance.script_pubkey(),
                },
                bitcoin::TxOut {
                    value: bitcoin::Amount::ZERO,
                    script_pubkey: bitcoin::ScriptBuf::new_op_return(
                        bitcoin::script::PushBytesBuf::try_from(payload)?,
                    ),
                },
            ],
        };
        let rotation_identity: Txid = rotation.compute_ntxid().into();
        let mut approval_payload = b"VIA_PROTOCOL:UPGRADE".to_vec();
        approval_payload.push(0);
        approval_payload.extend(proposal.compute_txid().as_byte_array());
        let approval = bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: bitcoin::OutPoint { txid: governance_parent_id, vout: 1 },
                ..bitcoin::TxIn::default()
            }],
            output: vec![
                bitcoin::TxOut {
                    value: bitcoin::Amount::from_sat(9_000),
                    script_pubkey: bootstrap.wallets.governance.script_pubkey(),
                },
                bitcoin::TxOut {
                    value: bitcoin::Amount::ZERO,
                    script_pubkey: bitcoin::ScriptBuf::new_op_return(
                        bitcoin::script::PushBytesBuf::try_from(approval_payload)?,
                    ),
                },
            ],
        };
        let mut block = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
        block.txdata = vec![rotation, proof, vote, approval];
        chain.insert(4, block);
        let points = chain
            .blocks
            .iter()
            .map(|(&height, block)| (height, block.block_hash()))
            .collect::<Vec<_>>();
        let chain = Arc::new(std::sync::Mutex::new(chain));
        let served = chain.clone();
        let runtime = tokio::runtime::Handle::current();
        let probe_pool = pool.clone();
        let server = via_test_utils::bitcoin_rpc::BitcoinRpcServer::new_with_errors(
            None,
            false,
            move |_, request| {
                runtime.block_on(async {
                    let mut probe = probe_pool.connection().await.unwrap();
                    let owner: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=1464095563::oid AND objid=0 AND objsubid=2 AND granted AND database=(SELECT oid FROM pg_database WHERE datname=current_database()))")
                        .fetch_one(probe.conn()).await.unwrap();
                    assert!(!owner, "source RPC ran while the migration owner was held");
                });
                served.lock().unwrap().rpc(&request)
            },
        )?;
        let client = Arc::new(BitcoinClient::new(
            &server.url(""),
            NodeAuth::None,
            ViaBtcClientConfig::for_tests(),
        )?);
        let source = ChainArchive::prepare(&client, 0, 4, identity).await?;
        let mut snapshot = connection.transaction_builder()?.set_readonly().build().await?;
        let fresh_input = snapshot.via_indexer_dal().semantic_migration_input().await?;
        snapshot.rollback().await?;
        let fresh = inspect(fresh_input, source.clone(), identity, client.clone()).await?;
        fresh.require_complete()?;
        assert!(fresh.groups.iter().all(|group| group.reconstruction == Reconstruction::Fresh
            && group.baseline == group.completed
            && group.completed.height == 2
            && group.evidence.blocks == 0));
        {
            let fresh_pool = ConnectionPool::test_pool().await;
            let mut fresh_connection = fresh_pool.connection().await?;
            sqlx::migrate!("../../lib/dal/migrations")
                .undo(fresh_connection.conn(), 20261023030434)
                .await?;
            let fresh_input = fresh_connection.via_indexer_dal().semantic_migration_input().await?;
            let fresh = inspect(fresh_input, source.clone(), identity, client.clone()).await?;
            apply(&fresh_pool, &client, &fresh, "disposable fresh backup").await?;
            let certificates: i64 = sqlx::query_scalar("SELECT count(*) FROM via_btc_migration_certificates WHERE application_status='applied'").fetch_one(fresh_connection.conn()).await?;
            assert_eq!(certificates, 4);
        }
        let details = SystemWalletsDetails::try_from(&bootstrap)?;
        for (role, info) in &details.0 {
            sqlx::query(
                "INSERT INTO via_wallets(role,address,tx_hash,l1_block_number) VALUES($1,$2,$3,0)",
            )
            .bind(role.to_string())
            .bind(info.addresses.iter().map(ToString::to_string).collect::<Vec<_>>().join(","))
            .bind(info.txid.to_string())
            .execute(connection.conn())
            .await?;
        }
        sqlx::query("INSERT INTO protocol_versions(id,timestamp,bootloader_code_hash,default_account_code_hash,evm_emulator_code_hash,created_at) VALUES($1,0,$2,$3,$4,NOW())")
            .bind(bootstrap.protocol_version.minor as i32).bind(bootstrap.bootloader_hash.as_bytes())
            .bind(bootstrap.abstract_account_hash.as_bytes()).bind(bootstrap.base_system_contracts_hashes().evm_emulator.as_ref().map(zksync_types::H256::as_bytes)).execute(connection.conn()).await?;
        sqlx::query("INSERT INTO protocol_patches(minor,patch,snark_wrapper_vk_hash,created_at) VALUES($1,$2,$3,NOW())")
            .bind(bootstrap.protocol_version.minor as i32).bind(bootstrap.protocol_version.patch.0 as i32)
            .bind(bootstrap.snark_wrapper_vk_hash.as_bytes()).execute(connection.conn()).await?;
        for (height, hash) in points {
            sqlx::query("INSERT INTO via_l1_blocks(number,hash) VALUES($1,$2)")
                .bind(i64::from(height))
                .bind(hash.to_string())
                .execute(connection.conn())
                .await?;
        }
        sqlx::query("INSERT INTO via_indexer_metadata(module,last_indexer_l1_block,updated_at) VALUES('via_btc_watch',4,NOW())").execute(connection.conn()).await?;
        let input = connection.via_indexer_dal().semantic_migration_input().await?;
        let (rebuild, prepared) = prepare(input, source.clone(), identity, client.clone()).await?;
        rebuild.require_complete()?;
        assert_eq!(rebuild.groups[1].evidence.rebuilt_records, 1);
        assert_eq!(rebuild.groups[0].evidence.rebuilt_records, 1);
        sqlx::query("INSERT INTO via_wallets(role,address,tx_hash,l1_block_number) VALUES('Sequencer',$1,$2,0)")
            .bind(sequencer.to_string()).bind(rotation_identity.to_string()).execute(connection.conn()).await?;
        let upgrade_catalogue = source_protocols(&bootstrap, &prepared.upgrades, 0)?;
        let upgraded = &upgrade_catalogue[1];
        connection
            .transactions_dal()
            .insert_system_transaction(upgraded.tx.as_ref().unwrap())
            .await?;
        sqlx::query("INSERT INTO protocol_versions(id,timestamp,bootloader_code_hash,default_account_code_hash,upgrade_tx_hash,created_at) VALUES(28,0,$1,$2,$3,NOW())")
            .bind(upgraded.base_system_contracts_hashes.bootloader.as_bytes()).bind(upgraded.base_system_contracts_hashes.default_aa.as_bytes())
            .bind(upgraded.tx.as_ref().unwrap().common_data.hash().as_bytes()).execute(connection.conn()).await?;
        sqlx::query("INSERT INTO protocol_patches(minor,patch,snark_wrapper_vk_hash,created_at) VALUES(28,0,$1,NOW())")
            .bind(upgraded.l1_verifier_config.snark_wrapper_vk_hash.as_bytes()).execute(connection.conn()).await?;
        let deposit = &prepared.deposits[0];
        connection
            .transactions_dal()
            .insert_transaction_l1(&deposit.operation, deposit.operation.eth_block())
            .await?;
        sqlx::query("UPDATE transactions SET signature=$2 WHERE hash=$1")
            .bind(deposit.operation.hash().as_bytes())
            .bind(deposit.protocol_txid.as_bytes())
            .execute(connection.conn())
            .await?;
        sqlx::query("UPDATE transactions SET l1_tx_mint=l1_tx_mint+1")
            .execute(connection.conn())
            .await?;
        let changed = connection.via_indexer_dal().semantic_migration_input().await?;
        assert!(inspect(changed, source.clone(), identity, client.clone())
            .await?
            .require_complete()
            .is_err());
        sqlx::query(
            "UPDATE transactions SET l1_tx_mint=l1_tx_mint-1,error='unreconciled execution'",
        )
        .execute(connection.conn())
        .await?;
        let protected = connection.via_indexer_dal().semantic_migration_input().await?;
        let refused = inspect(protected, source.clone(), identity, client.clone()).await?;
        assert_eq!(refused.groups[1].evidence.protected_records, 1);
        assert_eq!(refused.groups[0].evidence.protected_records, 1);
        assert!(refused.require_complete().is_err());
        sqlx::query("UPDATE transactions SET error=NULL").execute(connection.conn()).await?;
        let mut snapshot = connection.transaction_builder()?.set_readonly().build().await?;
        let input = snapshot.via_indexer_dal().semantic_migration_input().await?;
        snapshot.rollback().await?;
        let certificate = inspect(input, source, identity, client.clone()).await?;
        certificate.require_complete()?;
        assert_eq!(certificate.groups[2].evidence.messages.len(), 2);
        assert_eq!(certificate.groups[1].evidence.retained_records, 1);
        assert_eq!(certificate.groups[1].reconstruction, Reconstruction::RebuiltReversible);
        sqlx::query(
            "UPDATE via_indexer_metadata SET last_indexer_l1_block=1 WHERE module='via_btc_watch'",
        )
        .execute(connection.conn())
        .await?;
        let error =
            apply(&pool, &client, &certificate, "disposable verified backup").await.unwrap_err();
        assert!(error.to_string().contains("stale semantic migration input"), "{error:#}");
        let upgraded: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version=20261023041443)",
        )
        .fetch_one(connection.conn())
        .await?;
        assert!(!upgraded);
        sqlx::query(
            "UPDATE via_indexer_metadata SET last_indexer_l1_block=4 WHERE module='via_btc_watch'",
        )
        .execute(connection.conn())
        .await?;
        apply(&pool, &client, &certificate, "disposable verified backup").await?;
        let canonical: i64 = sqlx::query_scalar("SELECT count(*) FROM via_deposit_inclusions inclusion JOIN via_l1_blocks canonical ON canonical.number=inclusion.block_height AND canonical.hash=inclusion.block_hash WHERE inclusion.revoked_at IS NULL").fetch_one(connection.conn()).await?;
        assert_eq!(canonical, 1);
        let retained = connection.via_indexer_dal().semantic_migration_input().await?;
        assert!(retained.deposits[0].source_equivalent(&deposit.operation, deposit.protocol_txid));
        let ids:Vec<String>=sqlx::query_scalar("SELECT id::TEXT FROM via_btc_migration_certificates WHERE application_status='applied' ORDER BY scope").fetch_all(connection.conn()).await?;
        assert_eq!(ids.len(), 4);
        assert_eq!(ids.iter().collect::<std::collections::BTreeSet<_>>().len(), 4);
        let active: i64 = sqlx::query_scalar("SELECT count(*) FROM via_btc_scan_positions")
            .fetch_one(connection.conn())
            .await?;
        assert_eq!(active, 0);
        let origins: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM via_upgrade_inclusions WHERE revoked_at IS NULL",
        )
        .fetch_one(connection.conn())
        .await?;
        assert_eq!(origins, 1);
        let proof_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM via_proof_inclusions")
            .fetch_one(connection.conn())
            .await?;
        assert_eq!(proof_rows, 1);
        let finalization: Option<bool> =
            sqlx::query_scalar("SELECT decision FROM via_proof_finalization")
                .fetch_one(connection.conn())
                .await?;
        assert_eq!(finalization, Some(true));
        let legacy: i64 =
            sqlx::query_scalar("SELECT count(*) FROM via_wallets WHERE origin_kind='legacy'")
                .fetch_one(connection.conn())
                .await?;
        assert_eq!(legacy, 0);
        assert!(connection.via_wallet_dal().get_system_wallets_raw(1).await?.is_none());
        assert!(connection.via_wallet_dal().get_system_wallets_raw(2).await?.is_some());
        assert_eq!(
            connection.via_wallet_dal().get_system_wallets_raw(3).await?.unwrap()["Sequencer"],
            bootstrap.wallets.sequencer.to_string()
        );
        assert_eq!(
            connection.via_wallet_dal().get_system_wallets_raw(4).await?.unwrap()["Sequencer"],
            sequencer.to_string()
        );
        assert!(connection.via_wallet_dal().verifier_set_before(2).await.is_err());
        assert!(!connection.via_wallet_dal().verifier_set_before(3).await?.1.is_empty());
        let sequence: (i64, bool) =
            sqlx::query_as("SELECT last_value,is_called FROM via_wallets_id_seq")
                .fetch_one(connection.conn())
                .await?;
        {
            let mut source = chain.lock().unwrap();
            source.fork_at_height(0);
            source.unavailable = true;
        }
        apply(&pool, &client, &certificate, "disposable verified backup").await?;
        let after: Vec<String> = sqlx::query_scalar(
            "SELECT id::TEXT FROM via_btc_migration_certificates ORDER BY scope",
        )
        .fetch_all(connection.conn())
        .await?;
        assert_eq!(after, ids);
        let after_sequence: (i64, bool) =
            sqlx::query_as("SELECT last_value,is_called FROM via_wallets_id_seq")
                .fetch_one(connection.conn())
                .await?;
        assert_eq!(after_sequence, sequence);
        Ok(())
    }
    // jscpd:ignore-end
}
