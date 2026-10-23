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
use via_verifier_dal::{
    via_indexer_dal::SemanticMigrationInput, ConnectionPool, Verifier, VerifierDal,
};
use zksync_config::{
    configs::{
        via_btc_client::ViaBtcClientConfig, via_consensus::ViaGenesisConfig,
        via_secrets::ViaL1Secrets, DatabaseSecrets,
    },
    PostgresConfig,
};
use zksync_env_config::FromEnv;
use zksync_types::via_wallet::SystemWalletsDetails;

type Certificate = MigrationCertificate<SemanticMigrationInput>;

struct PreparedDeposit {
    inclusion: via_reorg::chain::MessageRef<BlockHash, Txid>,
    vout: u32,
    payload: serde_json::Value,
    operation: via_verifier_dal::via_transactions_dal::L1ToL2Transaction,
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
        let secrets: zksync_config::configs::via_secrets::ViaSecrets =
            zksync_core_leftovers::temp_config_store::read_yaml_repr::<
                zksync_protobuf_config::proto::via_secrets::ViaSecrets,
            >(&path.to_path_buf())?;
        (
            secrets.base_secrets.database.context("database secrets are absent")?,
            secrets.via_l1.context("Bitcoin source secrets are absent")?,
        )
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
    let pool =
        ConnectionPool::<Verifier>::builder(database.master_url()?, postgres.max_connections()?)
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
        input.census.delivery_requests,
        input.census.delivery_history,
        input.census.vote_attachments,
        input.census.withdrawal_batches,
        input.census.withdrawal_observations,
        input.census.withdrawal_holds,
        input.census.withdrawal_effects,
        input.census.recovery_barriers,
    ]
    .into_iter()
    .any(|count| count != 0);
    let fresh = !nonempty_domain
        && input.proofs.is_empty()
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
    let migrations = sqlx::migrate!("../../lib/verifier_dal/migrations");
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
                let Some(l1_tx) = deposit.l1_tx() else { continue };
                let operation = via_verifier_dal::via_transactions_dal::L1ToL2Transaction {
                    priority_id: i64::try_from(deposit.priority_id().0)?,
                    l1_block_number: i64::from(block.height),
                    tx_id: zksync_types::H256::from_str(&message.common.tx_id.to_string())?,
                    receiver: deposit.l2_receiver_address.to_string(),
                    value: i64::try_from(deposit.amount)?,
                    calldata: deposit.calldata,
                    canonical_tx_hash: l1_tx.hash(),
                };
                if let Some(retained) = input.deposits.iter().find(|retained| {
                    retained.canonical_tx_hash == operation.canonical_tx_hash.as_bytes()
                }) {
                    matched.insert(retained.canonical_tx_hash.clone());
                    if !retained.source_equivalent(&operation) {
                        groups[1].gaps.push(format!(
                            "deposit {} differs from its complete checked source payload",
                            operation.priority_id
                        ));
                    }
                    if retained.status.is_some() || retained.l1_batch_number.is_some() {
                        groups[1].evidence.protected_records += 1;
                        groups[1].gaps.push(format!(
                            "deposit {} requires executed L2 input reconciliation",
                            operation.priority_id
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
        if let (Some(height), Some(hash)) =
            (retained.source_height, retained.source_hash.as_deref())
        {
            if !source.matches_persisted_source(height, hash)
                || !matching.is_some_and(|(vote, _)| i64::from(vote.common.block_height) == height)
            {
                groups[2].gaps.push("retained vote has conflicting source provenance".into());
            }
        }
    }
    groups[2].evidence.retained_records = u64::try_from(input.votes.len())?;
    for retained in &input.proofs {
        for (height, hash) in [
            (retained.source_height, retained.source_hash.as_deref()),
            (retained.proof_height, retained.proof_hash.as_deref()),
        ] {
            if let (Some(height), Some(hash)) = (height, hash) {
                if !source.matches_persisted_source(height, hash) {
                    groups[2].gaps.push(format!(
                        "retained proof {} has conflicting source provenance",
                        retained.id
                    ));
                }
            }
        }
        let matching = writes.proofs.iter().find(|proof| {
            zksync_types::H256::from_str(&proof.identity.to_string())
                .ok()
                .is_some_and(|identity| identity.as_bytes() == retained.proof)
        });
        if let Some(proof) = matching {
            if retained.batch != i64::from(proof.batch.l1_batch_index.0)
                || retained.batch_hash != proof.batch.l1_batch_hash.as_bytes()
                || retained.parent_hash != proof.batch.prev_l1_batch_hash.as_bytes()
                || retained.proof_blob != proof.proof.blob_id
                || retained.batch_blob != proof.batch.blob_id
                || retained.batch_txid != proof.proof.l1_batch_reveal_txid.to_string()
                || retained.da_identifier != proof.proof.da_identifier
            {
                groups[2].gaps.push(format!(
                    "retained proof {} differs from its full linked source content",
                    retained.id
                ));
            }
            if retained
                .proof_height
                .is_some_and(|height| height != i64::from(proof.inclusion.block.height))
            {
                groups[2].gaps.push(format!(
                    "retained proof {} has a different included source height",
                    retained.id
                ));
            }
        } else {
            groups[2].gaps.push(format!(
                "retained proof {} has no checked source in the proved interval",
                retained.id
            ));
        }
        if retained.status.is_some() || retained.unverified_dev {
            groups[2].evidence.protected_records += 1;
            groups[2].gaps.push(format!(
                "retained proof {} requires completed input / external effect reconciliation",
                retained.id
            ));
        }
    }
    groups[2].evidence.retained_records += u64::try_from(input.proofs.len())?;
    for retained in &input.protocols {
        let bootstrap_version = retained.minor == bootstrap.protocol_version.minor as i32
            && retained.patch == bootstrap.protocol_version.patch.0 as i32;
        let equivalent = if bootstrap_version {
            retained.bootloader == bootstrap.bootloader_hash.as_bytes()
                && retained.default_account == bootstrap.abstract_account_hash.as_bytes()
                && retained.verifier_key == bootstrap.snark_wrapper_vk_hash.as_bytes()
                && retained.upgrade_tx_hash.as_deref()
                    == Some(zksync_types::H256::zero().as_bytes())
        } else if let Some(upgrade) = writes.upgrades.iter().find(|upgrade| {
            upgrade.proposal.version.minor as i32 == retained.minor
                && upgrade.proposal.version.patch.0 as i32 == retained.patch
        }) {
            let proposal = &upgrade.proposal;
            let hash = zksync_types::via_protocol_upgrade::ViaProtocolUpgrade::default()
                .get_canonical_tx_hash(proposal.version, proposal.system_contracts.clone())?;
            retained.bootloader == proposal.bootloader_code_hash.as_bytes()
                && retained.default_account == proposal.default_account_code_hash.as_bytes()
                && retained.verifier_key == proposal.recursion_scheduler_level_vk_hash.as_bytes()
                && retained.upgrade_tx_hash.as_deref() == Some(hash.as_bytes())
        } else {
            false
        };
        if !equivalent || retained.evm_emulator.is_some() || retained.timestamp.is_some() {
            groups[0].gaps.push(format!(
                "protocol {}.{} differs from its complete checked source catalogue",
                retained.minor, retained.patch
            ));
        }
        if !bootstrap_version && retained.executed != Some(false) {
            groups[0].evidence.protected_records += 1;
            groups[0].gaps.push(
                "retained protocol upgrade requires protected execution reconciliation".into(),
            );
        }
        if !input
            .schema
            .iter()
            .any(|revision| revision.version == 20261023041443 && revision.success)
            && input.protocols.iter().filter(|version| version.minor == retained.minor).count() > 1
        {
            groups[0]
                .gaps
                .push("legacy minor execution flag cannot prove individual patch execution".into());
        }
    }
    groups[2].evidence.rebuilt_records = u64::try_from(writes.proofs.len() + writes.votes.len())?;
    for group in &mut groups {
        group.gaps.sort();
        group.gaps.dedup();
    }
    let certificate = Certificate {
        format_version: 1,
        role: "verifier".into(),
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

async fn apply(
    pool: &ConnectionPool<Verifier>,
    client: &Arc<BitcoinClient>,
    certificate: &Certificate,
    backup: &str,
) -> anyhow::Result<()> {
    certificate.require_complete()?;
    anyhow::ensure!(certificate.role == "verifier", "certificate belongs to another node role");
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
    tx.via_indexer_dal().lock_source_writer().await?;
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
    let mut migrator = sqlx::migrate!("../../lib/verifier_dal/migrations");
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
    let retained_bootstrap = certificate.database_input.protocols.iter().find(|protocol| {
        protocol.minor == bootstrap.protocol_version.minor as i32
            && protocol.patch == bootstrap.protocol_version.patch.0 as i32
    });
    let payload = serde_json::json!({"bootloader_code_hash":bootstrap.bootloader_hash.as_bytes(),"default_account_code_hash":bootstrap.abstract_account_hash.as_bytes(),
        "upgrade_tx_hash":zksync_types::H256::zero().as_bytes(),"recursion_scheduler_level_vk_hash":bootstrap.snark_wrapper_vk_hash.as_bytes()});
    sqlx::query("UPDATE protocol_patches SET immutable_payload=CASE WHEN immutable_payload IS NULL THEN $3 ELSE via_btc_immutable_payload(immutable_payload,$3) END,upgrade_tx_hash=$4,executed=$5 WHERE minor=$1 AND patch=$2")
        .bind(bootstrap.protocol_version.minor as i32).bind(bootstrap.protocol_version.patch.0 as i32).bind(payload)
        .bind(zksync_types::H256::zero().as_bytes()).bind(retained_bootstrap.and_then(|version| version.executed).unwrap_or(false)).execute(tx.conn()).await?;
    tx.via_protocol_versions_dal()
        .save_protocol_version(
            bootstrap.protocol_version,
            bootstrap.bootloader_hash.as_bytes(),
            bootstrap.abstract_account_hash.as_bytes(),
            zksync_types::H256::zero().as_bytes(),
            bootstrap.snark_wrapper_vk_hash.as_bytes(),
        )
        .await?;
    tx.via_protocol_versions_dal()
        .mark_via_bootstrap_version(
            bootstrap.protocol_version,
            bootstrap.bootloader_hash.as_bytes(),
            bootstrap.abstract_account_hash.as_bytes(),
            zksync_types::H256::zero().as_bytes(),
            bootstrap.snark_wrapper_vk_hash.as_bytes(),
        )
        .await?;
    if retained_bootstrap.is_none() {
        // Match the existing fresh verifier-genesis initializer; retained execution flags are preserved above.
        tx.via_protocol_versions_dal()
            .mark_upgrade_as_executed(zksync_types::H256::zero().as_bytes())
            .await?;
    }
    for upgrade in upgrades {
        let proposal = &upgrade.proposal;
        let hash = zksync_types::via_protocol_upgrade::ViaProtocolUpgrade::default()
            .get_canonical_tx_hash(proposal.version, proposal.system_contracts.clone())?;
        let payload = serde_json::json!({"bootloader_code_hash":proposal.bootloader_code_hash.as_bytes(),"default_account_code_hash":proposal.default_account_code_hash.as_bytes(),
            "upgrade_tx_hash":hash.as_bytes(),"recursion_scheduler_level_vk_hash":proposal.recursion_scheduler_level_vk_hash.as_bytes()});
        sqlx::query("UPDATE protocol_patches SET immutable_payload=CASE WHEN immutable_payload IS NULL THEN $3 ELSE via_btc_immutable_payload(immutable_payload,$3) END,upgrade_tx_hash=$4,executed=FALSE WHERE minor=$1 AND patch=$2")
            .bind(proposal.version.minor as i32).bind(proposal.version.patch.0 as i32).bind(payload).bind(hash.as_bytes()).execute(tx.conn()).await?;
        tx.via_protocol_versions_dal()
            .record_via_upgrade_origin(
                proposal.version,
                upgrade.identity,
                &upgrade.payload,
                upgrade.inclusion,
            )
            .await?;
        tx.via_protocol_versions_dal()
            .save_protocol_version(
                proposal.version,
                proposal.bootloader_code_hash.as_bytes(),
                proposal.default_account_code_hash.as_bytes(),
                hash.as_bytes(),
                proposal.recursion_scheduler_level_vk_hash.as_bytes(),
            )
            .await?;
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
                deposit.operation.canonical_tx_hash,
            )
            .await?;
        tx.via_transactions_dal().insert_transaction(deposit.operation).await?;
    }
    for proof in proofs {
        let inclusion_id = tx
            .via_votes_dal()
            .record_reader_proof(
                proof.identity,
                proof.batch.l1_batch_index.0,
                &proof.payload,
                proof.inclusion,
            )
            .await?;
        let protocol_txid = zksync_types::H256::from_str(&proof.identity.to_string())?;
        tx.via_votes_dal()
            .record_votable_transaction(
                proof.batch.l1_batch_index.0,
                proof.batch.l1_batch_hash,
                proof.batch.prev_l1_batch_hash,
                proof.proof.da_identifier,
                protocol_txid,
                proof.proof.blob_id,
                proof.proof.l1_batch_reveal_txid.to_string(),
                proof.batch.blob_id,
                inclusion_id,
                via_verifier_dal::via_votes_dal::ProofMembership::RetainedOnly,
            )
            .await?;
        let admission = tx
            .via_votes_dal()
            .admit_recorded_proof(
                inclusion_id,
                protocol_txid,
                proof.batch.l1_batch_index.0,
                proof.batch.prev_l1_batch_hash,
            )
            .await?;
        anyhow::ensure!(
            !matches!(
                admission,
                via_verifier_dal::via_votes_dal::ProofAdmission::ContinuityGap { .. }
            ),
            "stopped replay has a proof continuity gap: {admission:?}"
        );
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
    storage: &mut via_verifier_dal::Connection<'_, Verifier>,
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
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut connection = pool.connection().await?;
        sqlx::migrate!("../../lib/verifier_dal/migrations")
            .undo(connection.conn(), 20261023030434)
            .await?;
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
                    let owner: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid::BIGINT=($1::BIGINT >> 32) AND objid::BIGINT=($1::BIGINT & 4294967295) AND objsubid=1 AND granted AND database=(SELECT oid FROM pg_database WHERE datname=current_database()))")
                        .bind(via_verifier_dal::BTC_WATCH_OWNER_LOCK_KEY)
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
            sqlx::migrate!("../../lib/verifier_dal/migrations")
                .undo(fresh_connection.conn(), 20261023030434)
                .await?;
            let fresh_input = fresh_connection.via_indexer_dal().semantic_migration_input().await?;
            let fresh = inspect(fresh_input, source.clone(), identity, client.clone()).await?;
            apply(&fresh_pool, &client, &fresh, "disposable fresh backup").await?;
            let certificates: i64 = sqlx::query_scalar("SELECT count(*) FROM via_btc_migration_certificates WHERE application_status='applied'").fetch_one(fresh_connection.conn()).await?;
            assert_eq!(certificates, 4);
            assert_eq!(
                fresh_connection
                    .via_protocol_versions_dal()
                    .latest_executed_version_before(3)
                    .await?,
                Some(bootstrap.protocol_version)
            );
            assert_eq!(
                fresh_connection
                    .via_protocol_versions_dal()
                    .get_in_progress_upgrade_tx_hash_before(3)
                    .await?,
                None
            );
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
        sqlx::query("INSERT INTO protocol_versions(id,bootloader_code_hash,default_account_code_hash,upgrade_tx_hash,recursion_scheduler_level_vk_hash,executed,created_at) VALUES($1,$2,$3,$4,$5,TRUE,NOW())")
            .bind(bootstrap.protocol_version.minor as i32).bind(bootstrap.bootloader_hash.as_bytes())
            .bind(bootstrap.abstract_account_hash.as_bytes()).bind(zksync_types::H256::zero().as_bytes())
            .bind(bootstrap.snark_wrapper_vk_hash.as_bytes()).execute(connection.conn()).await?;
        sqlx::query("INSERT INTO protocol_patches(minor,patch,created_at) VALUES($1,$2,NOW())")
            .bind(bootstrap.protocol_version.minor as i32)
            .bind(bootstrap.protocol_version.patch.0 as i32)
            .execute(connection.conn())
            .await?;
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
        let retained_proof = &prepared.proofs[0];
        let retained_id: i64 = sqlx::query_scalar("INSERT INTO via_votable_transactions(l1_batch_number,l1_batch_hash,prev_l1_batch_hash,proof_blob_id,proof_reveal_tx_id,pubdata_blob_id,pubdata_reveal_tx_id,da_identifier) VALUES(1,$1,$2,$3,$4,$5,$6,$7) RETURNING id")
            .bind(retained_proof.batch.l1_batch_hash.as_bytes()).bind(retained_proof.batch.prev_l1_batch_hash.as_bytes())
            .bind(&retained_proof.proof.blob_id).bind(zksync_types::H256::from_str(&proof_identity.to_string())?.as_bytes())
            .bind(&retained_proof.batch.blob_id).bind(retained_proof.proof.l1_batch_reveal_txid.to_string())
            .bind(&retained_proof.proof.da_identifier).fetch_one(connection.conn()).await?;
        sqlx::query("INSERT INTO via_votes(votable_transaction_id,verifier_address,vote) VALUES($1,$2,TRUE)")
            .bind(retained_id).bind(bootstrap.wallets.verifiers[0].to_string()).execute(connection.conn()).await?;
        sqlx::query("UPDATE via_votable_transactions SET l1_batch_status=TRUE WHERE id=$1")
            .bind(retained_id)
            .execute(connection.conn())
            .await?;
        let completed = connection.via_indexer_dal().semantic_migration_input().await?;
        assert!(inspect(completed, source.clone(), identity, client.clone())
            .await?
            .require_complete()
            .is_err());
        sqlx::query("UPDATE via_votable_transactions SET l1_batch_status=NULL WHERE id=$1")
            .bind(retained_id)
            .execute(connection.conn())
            .await?;
        let upgrade = &prepared.upgrades[0].proposal;
        let upgrade_hash = zksync_types::via_protocol_upgrade::ViaProtocolUpgrade::default()
            .get_canonical_tx_hash(upgrade.version, upgrade.system_contracts.clone())?;
        sqlx::query("INSERT INTO protocol_versions(id,bootloader_code_hash,default_account_code_hash,upgrade_tx_hash,recursion_scheduler_level_vk_hash,executed,created_at) VALUES(28,$1,$2,$3,$4,FALSE,NOW())")
            .bind(upgrade.bootloader_code_hash.as_bytes()).bind(upgrade.default_account_code_hash.as_bytes())
            .bind(upgrade_hash.as_bytes()).bind(upgrade.recursion_scheduler_level_vk_hash.as_bytes()).execute(connection.conn()).await?;
        sqlx::query("INSERT INTO protocol_patches(minor,patch,created_at) VALUES(28,0,NOW())")
            .execute(connection.conn())
            .await?;
        let deposit = &prepared.deposits[0];
        let operation = &deposit.operation;
        sqlx::query("INSERT INTO via_transactions(priority_id,l1_block_number,tx_id,receiver,value,calldata,canonical_tx_hash) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(operation.priority_id).bind(operation.l1_block_number).bind(operation.tx_id.as_bytes()).bind(&operation.receiver)
            .bind(operation.value).bind(&operation.calldata).bind(operation.canonical_tx_hash.as_bytes()).execute(connection.conn()).await?;
        sqlx::query("UPDATE via_transactions SET value=value+1").execute(connection.conn()).await?;
        let changed = connection.via_indexer_dal().semantic_migration_input().await?;
        assert!(inspect(changed, source.clone(), identity, client.clone())
            .await?
            .require_complete()
            .is_err());
        sqlx::query("UPDATE via_transactions SET value=value-1,status=TRUE")
            .execute(connection.conn())
            .await?;
        let protected = connection.via_indexer_dal().semantic_migration_input().await?;
        let refused = inspect(protected, source.clone(), identity, client.clone()).await?;
        assert_eq!(refused.groups[1].evidence.protected_records, 1);
        assert!(refused.require_complete().is_err());
        sqlx::query("UPDATE via_transactions SET status=NULL").execute(connection.conn()).await?;
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
        assert!(retained.deposits[0].source_equivalent(operation));
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
        let result: (i64, Option<bool>, Option<bool>) =
            sqlx::query_as("SELECT id,l1_batch_status,is_finalized FROM via_votable_transactions")
                .fetch_one(connection.conn())
                .await?;
        assert_ne!(result.0, retained_id);
        assert_eq!((result.1, result.2), (None, None));
        let legacy_source: Option<i64> = sqlx::query_scalar(
            "SELECT reader_proof_inclusion_id FROM via_proof_verdicts WHERE id=$1",
        )
        .bind(retained_id)
        .fetch_one(connection.conn())
        .await?;
        assert_eq!(legacy_source, None);
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
        let executed: bool = sqlx::query_scalar("SELECT executed FROM protocol_versions")
            .fetch_one(connection.conn())
            .await?;
        assert!(executed);
        Ok(())
    }
    // jscpd:ignore-end
}
