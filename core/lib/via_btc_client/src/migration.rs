//! Checked source material for stopped semantic migration. SQL ownership stays in each node's DAL.

use anyhow::Context;
use bitcoin::{blockdata::constants::genesis_block, Block, BlockHash, Network};
use serde::{Deserialize, Serialize};

use crate::{client::BitcoinClient, traits::BitcoinOps};

pub const MAX_CERTIFICATE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ARCHIVE_HEADERS: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WalletDecision {
    Accepted,
    DuplicateRole,
    EarlierUnfinalizedProof,
    BadUpdate,
    NotReady,
}

impl WalletDecision {
    pub fn persisted(self) -> Option<&'static str> {
        match self {
            Self::Accepted => Some("accepted"),
            Self::DuplicateRole => Some("duplicate_role"),
            Self::EarlierUnfinalizedProof => Some("earlier_unfinalized_proof"),
            Self::BadUpdate => Some("bad_update"),
            Self::NotReady => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PreparedWalletUpdate {
    pub identity: bitcoin::Txid,
    pub inclusion: via_reorg::chain::MessageRef<BlockHash, bitcoin::Txid>,
    pub roles: Vec<zksync_types::via_wallet::WalletRole>,
    pub details: zksync_types::via_wallet::SystemWalletsDetails,
    pub payload: serde_json::Value,
    pub decision: WalletDecision,
}

#[derive(Debug, Clone)]
pub struct PreparedProof {
    pub identity: bitcoin::Txid,
    pub inclusion: via_reorg::chain::MessageRef<BlockHash, bitcoin::Txid>,
    pub proof: crate::types::ProofDAReferenceInput,
    pub batch: crate::types::L1BatchDAReferenceInput,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct PreparedUpgrade {
    pub identity: bitcoin::Txid,
    pub inclusion: via_reorg::chain::MessageRef<BlockHash, bitcoin::Txid>,
    pub proposal: crate::types::SystemContractUpgradeProposalInput,
    pub payload: serde_json::Value,
}

/// Resolve immutable proposal content before either concrete SQL owner applies the approved version.
/// https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_watch/src/event_processors/decentralized_upgrades.rs
pub async fn prepare_upgrade(
    client: &BitcoinClient,
    approval: &crate::types::SystemContractUpgrade,
) -> anyhow::Result<PreparedUpgrade> {
    use crate::{
        indexer::{replay::semantic_payload, MessageParser},
        types::FullInscriptionMessage as M,
    };
    let transaction = client
        .get_transaction(&approval.input.proposal_tx_id)
        .await
        .with_context(|| format!("fetch upgrade proposal {}", approval.input.proposal_tx_id))?;
    anyhow::ensure!(
        transaction.compute_txid() == approval.input.proposal_tx_id,
        "upgrade proposal carrier differs from its requested identity"
    );
    let proposals = MessageParser::new(client.get_network())
        .parse_system_transaction(&transaction, approval.common.block_height, None)
        .into_iter()
        .filter(|message| matches!(message, M::SystemContractUpgradeProposal(_)))
        .collect::<Vec<_>>();
    let [proposal @ M::SystemContractUpgradeProposal(details)] = proposals.as_slice() else {
        anyhow::bail!("upgrade approval requires exactly one proposal");
    };
    Ok(PreparedUpgrade {
        identity: approval.common.tx_id,
        inclusion: approval.common.inclusion()?,
        proposal: details.input.clone(),
        payload: serde_json::json!({"approval":semantic_payload(&M::SystemContractUpgrade(approval.clone()))?,
            "proposal":semantic_payload(proposal)?}),
    })
}

/// This scoped source prefix is discarded on any preparation error; it never grants durable authority.
#[derive(Default)]
pub struct MigrationProofPrefix {
    proofs: Vec<(bitcoin::Txid, Vec<String>)>,
    proof_payloads: std::collections::BTreeMap<bitcoin::Txid, serde_json::Value>,
    vote_payloads: std::collections::BTreeMap<(bitcoin::Txid, u32), serde_json::Value>,
    votes: Vec<via_reorg::votes::Vote<bitcoin::Txid, String, (bitcoin::Txid, u32)>>,
}

impl MigrationProofPrefix {
    pub async fn prepare_block(
        &mut self,
        indexer: &mut crate::indexer::BitcoinInscriptionIndexer,
        messages: &[crate::types::FullInscriptionMessage],
    ) -> anyhow::Result<Vec<PreparedProof>> {
        use crate::{
            indexer::replay::{compare_payload, semantic_payload},
            types::FullInscriptionMessage as M,
        };
        let mut prepared = Vec::new();
        for message in messages {
            match message {
                M::ProofDAReference(proof) => {
                    let linked = indexer
                        .parse_transaction(&proof.input.l1_batch_reveal_txid)
                        .await
                        .with_context(|| {
                            format!("preparing linked batch for proof {}", proof.common.tx_id)
                        })?;
                    let [batch_message @ M::L1BatchDAReference(batch)] = linked.as_slice() else {
                        anyhow::bail!(
                            "proof {} must reference exactly one batch message",
                            proof.common.tx_id
                        );
                    };
                    anyhow::ensure!(
                        batch.common.carrier_tx_id == Some(proof.input.l1_batch_reveal_txid),
                        "linked batch carrier differs from its requested identity"
                    );
                    let payload = serde_json::json!({"proof":semantic_payload(message)?,"batch":semantic_payload(batch_message)?});
                    if let Some(previous) = self.proof_payloads.get(&proof.common.tx_id) {
                        compare_payload(previous, &payload).map_err(|field| {
                            anyhow::anyhow!(
                                "proof {} exists_with_different_{field}",
                                proof.common.tx_id
                            )
                        })?;
                    }
                    self.proof_payloads.insert(proof.common.tx_id, payload.clone());
                    self.proofs.push((
                        proof.common.tx_id,
                        indexer.get_state().verifiers.iter().map(ToString::to_string).collect(),
                    ));
                    prepared.push(PreparedProof {
                        identity: proof.common.tx_id,
                        inclusion: proof.common.inclusion()?,
                        proof: proof.input.clone(),
                        batch: batch.input.clone(),
                        payload,
                    });
                }
                M::ValidatorAttestation(vote) => {
                    let inclusion = vote.common.inclusion()?;
                    let identity = (vote.common.tx_id, inclusion.message_index);
                    let payload = semantic_payload(message)?;
                    if let Some(previous) = self.vote_payloads.get(&identity) {
                        compare_payload(previous, &payload).map_err(|field| {
                            anyhow::anyhow!(
                                "vote {} exists_with_different_{field}",
                                vote.common.tx_id
                            )
                        })?;
                    }
                    self.vote_payloads.insert(identity, payload);
                    self.votes.push(via_reorg::votes::Vote {
                        identity,
                        proof: vote.input.reference_txid,
                        verifier: vote
                            .common
                            .p2wpkh_address
                            .as_ref()
                            .context("authenticated vote has no verifier")?
                            .to_string(),
                        approved: matches!(vote.input.attestation, crate::types::Vote::Ok),
                        position: [
                            inclusion.block.height,
                            inclusion.tx_index,
                            inclusion.message_index,
                        ],
                    });
                }
                _ => {}
            }
        }
        Ok(prepared)
    }

    /// The captured set judges its exact proof, including votes later in the rotation's block.
    /// https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/types/validation.go
    pub fn rotation_admission(
        &self,
        threshold: f64,
    ) -> anyhow::Result<via_reorg::configuration::RotationAdmission> {
        let mut votes = std::collections::BTreeMap::<_, Vec<_>>::new();
        for vote in &self.votes {
            votes.entry(vote.proof).or_default().push(vote.clone());
        }
        for (proof, members) in &self.proofs {
            let tally = via_reorg::votes::tally(
                proof,
                Some(members.as_slice()),
                votes.get(proof).map(Vec::as_slice).unwrap_or_default(),
                threshold,
            )
            .map_err(|error| {
                anyhow::anyhow!("stopped proof prefix cannot be tallied: {error:?}")
            })?;
            if tally.finalized.is_none() {
                return Ok(
                    via_reorg::configuration::RotationAdmission::RejectedEarlierUnfinalizedProof,
                );
            }
        }
        Ok(via_reorg::configuration::RotationAdmission::Accepted)
    }
}

/// Source-only preparation leaves SQL ownership with each role. A missing prefix cannot admit a rotation.
/// https://github.com/etcd-io/raft/blob/1931ff5beacc1e2539003c0c70f096285f04c425/raft.go
pub async fn prepare_wallet_updates(
    client: &BitcoinClient,
    messages: &[crate::types::FullInscriptionMessage],
    prefix: via_reorg::configuration::RotationAdmission,
) -> anyhow::Result<Vec<PreparedWalletUpdate>> {
    use via_reorg::configuration::{duplicate_roles, RotationAdmission};
    use zksync_types::via_wallet::{SystemWalletsDetails, WalletInfo, WalletRole};

    use crate::{
        indexer::{replay::semantic_payload, MessageParser},
        types::FullInscriptionMessage as M,
    };

    let roles = |message: &M| match message {
        M::UpdateGovernance(_) => vec![WalletRole::Gov],
        M::UpdateSequencer(_) => vec![WalletRole::Sequencer],
        M::UpdateBridge(_) => vec![WalletRole::Bridge, WalletRole::Verifier],
        _ => vec![],
    };
    let duplicates = duplicate_roles(messages.iter().flat_map(|message| {
        let common = message.common();
        roles(message).into_iter().map(move |role| {
            ((common.block_height, role.to_string()), (common.tx_id, common.message_index))
        })
    }));
    let mut ordered =
        messages.iter().filter(|message| !roles(message).is_empty()).collect::<Vec<_>>();
    ordered.sort_by_key(|message| {
        let common = message.common();
        (common.block_height, common.tx_index, common.message_index)
    });
    let mut prepared = Vec::with_capacity(ordered.len());
    for message in ordered {
        let common = message.common();
        let affected_roles = roles(message);
        let mut update = PreparedWalletUpdate {
            identity: common.tx_id,
            inclusion: common.inclusion()?,
            roles: affected_roles.clone(),
            details: SystemWalletsDetails::default(),
            payload: semantic_payload(message)?,
            decision: if affected_roles
                .iter()
                .any(|role| duplicates.contains(&(common.block_height, role.to_string())))
            {
                WalletDecision::DuplicateRole
            } else {
                WalletDecision::Accepted
            },
        };
        let mut addresses = Vec::new();
        match message {
            M::UpdateGovernance(message) => {
                match message.input.address.clone().require_network(client.get_network()) {
                    Ok(address) => addresses.push((WalletRole::Gov, vec![address])),
                    Err(_) => update.decision = WalletDecision::BadUpdate,
                }
            }
            M::UpdateSequencer(message) => {
                match message.input.address.clone().require_network(client.get_network()) {
                    Ok(address) => addresses.push((WalletRole::Sequencer, vec![address])),
                    Err(_) => update.decision = WalletDecision::BadUpdate,
                }
            }
            M::UpdateBridge(message) => {
                let transaction = client
                    .get_transaction(&message.input.proposal_tx_id)
                    .await
                    .with_context(|| {
                        format!(
                            "configuration proposal {} is unavailable",
                            message.input.proposal_tx_id
                        )
                    })?;
                anyhow::ensure!(
                    transaction.compute_txid() == message.input.proposal_tx_id,
                    "configuration proposal carrier differs from its requested identity"
                );
                let proposals = MessageParser::new(client.get_network())
                    .parse_system_transaction(&transaction, common.block_height, None)
                    .into_iter()
                    .filter(|message| matches!(message, M::UpdateBridgeProposal(_)))
                    .collect::<Vec<_>>();
                if let [proposal @ M::UpdateBridgeProposal(details)] = proposals.as_slice() {
                    update.payload["proposal"] = semantic_payload(proposal)?;
                    let bridge = details
                        .input
                        .bridge_musig2_address
                        .clone()
                        .require_network(client.get_network());
                    let verifiers = details
                        .input
                        .verifier_p2wpkh_addresses
                        .iter()
                        .cloned()
                        .map(|address| address.require_network(client.get_network()))
                        .collect::<Result<Vec<_>, _>>();
                    match (bridge, verifiers) {
                        (Ok(bridge), Ok(verifiers)) if !verifiers.is_empty() => {
                            addresses.push((WalletRole::Bridge, vec![bridge]));
                            addresses.push((WalletRole::Verifier, verifiers));
                            if update.decision == WalletDecision::Accepted {
                                update.decision = match prefix {
                                    RotationAdmission::Accepted => WalletDecision::Accepted,
                                    RotationAdmission::RejectedEarlierUnfinalizedProof => {
                                        WalletDecision::EarlierUnfinalizedProof
                                    }
                                    RotationAdmission::NotReady => WalletDecision::NotReady,
                                };
                            }
                        }
                        _ => update.decision = WalletDecision::BadUpdate,
                    }
                } else {
                    update.payload["proposals"] = serde_json::to_value(
                        proposals.iter().map(semantic_payload).collect::<Result<Vec<_>, _>>()?,
                    )?;
                    update.decision = WalletDecision::BadUpdate;
                }
            }
            _ => unreachable!(),
        }
        for (role, addresses) in addresses {
            update.details.0.insert(role, WalletInfo { addresses, txid: common.tx_id });
        }
        prepared.push(update);
    }
    Ok(prepared)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePoint {
    pub height: u32,
    pub hash: BlockHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedBlock {
    pub height: u32,
    pub block: Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivedHeader {
    pub height: u32,
    pub header: bitcoin::block::Header,
}

impl ArchivedHeader {
    pub async fn checked_block(&self, client: &BitcoinClient) -> anyhow::Result<ArchivedBlock> {
        let header = client.canonical_header_at(self.height, self.header.block_hash()).await?;
        anyhow::ensure!(
            header.parent_hash == self.header.prev_blockhash,
            "archive parent differs from source"
        );
        let block = client.checked_block(&header).await?;
        anyhow::ensure!(block.header == self.header, "archive header differs from checked body");
        Ok(ArchivedBlock { height: self.height, block })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapLocation {
    pub block: SourcePoint,
    pub transaction_index: u32,
}

/// Headers commit to every empty/nonempty block. Replay holds only one checked body at a time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainArchive {
    pub network: Network,
    pub genesis: BlockHash,
    pub observed_tip: SourcePoint,
    pub headers: Vec<ArchivedHeader>,
    pub bootstrap: BootstrapLocation,
}

impl ChainArchive {
    /// Legacy display hashes and the migrated bytea rendering have different byte-order contracts.
    pub fn matches_persisted_source(&self, height: i64, encoded: &str) -> bool {
        use bitcoin::hashes::Hash;
        let hash = if let Some(native) = encoded.strip_prefix("\\x") {
            bitcoin::hex::FromHex::from_hex(native)
                .ok()
                .and_then(|bytes: Vec<u8>| BlockHash::from_slice(&bytes).ok())
        } else {
            encoded.parse::<BlockHash>().ok()
        };
        self.headers.iter().any(|header| {
            i64::from(header.height) == height && Some(header.header.block_hash()) == hash
        })
    }

    /// Like pg_rewind, preparation verifies source identity before proposing any mutation.
    /// Via additionally checks the complete hash-addressed interval, dropping each body after use.
    /// https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/bin/pg_rewind/pg_rewind.c
    pub async fn prepare(
        client: &BitcoinClient,
        first: u32,
        last: u32,
        bootstrap_txid: bitcoin::Txid,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(first <= last, "migration interval is reversed");
        anyhow::ensure!(
            u64::from(last) - u64::from(first) < MAX_ARCHIVE_HEADERS as u64,
            "migration header census exceeds its bound; choose an explicitly proved baseline"
        );
        let network = client.get_network();
        let genesis = client.block_hash_at_height(0).await?;
        anyhow::ensure!(
            genesis == genesis_block(network).block_hash(),
            "source genesis differs from configured network"
        );
        let tip_hash = client.best_block_hash().await?;
        let tip = client
            .header_by_hash(&tip_hash)
            .await?
            .ok_or_else(|| anyhow::anyhow!("source tip header is unavailable"))?;
        client.canonical_header_at(tip.height, tip_hash).await?;
        anyhow::ensure!(last <= tip.height, "migration interval exceeds the observed source tip");
        let mut headers = Vec::new();
        let mut bootstrap = None;
        for height in first..=last {
            let hash = client.block_hash_at_height(height).await?;
            let header = client.canonical_header_at(height, hash).await?;
            let block = client.checked_block(&header).await?;
            for index in block.txdata.iter().enumerate().filter_map(|(index, transaction)| {
                (transaction.compute_txid() == bootstrap_txid).then_some(index)
            }) {
                anyhow::ensure!(
                    bootstrap.is_none(),
                    "bootstrap transaction has multiple archived inclusions"
                );
                bootstrap = Some(BootstrapLocation {
                    block: SourcePoint { height, hash },
                    transaction_index: u32::try_from(index)?,
                });
            }
            headers.push(ArchivedHeader { height, header: block.header });
        }
        let archive = Self {
            network,
            genesis,
            observed_tip: SourcePoint { height: tip.height, hash: tip_hash },
            headers,
            bootstrap: bootstrap.ok_or_else(|| {
                anyhow::anyhow!("bootstrap transaction has no archived inclusion")
            })?,
        };
        archive.revalidate(client).await?;
        Ok(archive)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.genesis == genesis_block(self.network).block_hash(),
            "archive has a foreign genesis"
        );
        anyhow::ensure!(
            !self.headers.is_empty() && self.headers.len() <= MAX_ARCHIVE_HEADERS,
            "archive header census is empty or exceeds its bound"
        );
        for entry in &self.headers {
            anyhow::ensure!(
                entry.height <= self.observed_tip.height,
                "archive exceeds its observed tip"
            );
            if entry.height == 0 {
                anyhow::ensure!(
                    entry.header.block_hash() == self.genesis,
                    "archive genesis differs"
                );
            }
            if entry.height == self.observed_tip.height {
                anyhow::ensure!(
                    entry.header.block_hash() == self.observed_tip.hash,
                    "archive tip identity differs"
                );
            }
        }
        for pair in self.headers.windows(2) {
            anyhow::ensure!(
                pair[0].height.checked_add(1) == Some(pair[1].height),
                "archive omits or repeats a block height"
            );
            anyhow::ensure!(
                pair[1].header.prev_blockhash == pair[0].header.block_hash(),
                "archive parent linkage differs"
            );
        }
        anyhow::ensure!(
            self.headers.iter().any(|entry| entry.height == self.bootstrap.block.height
                && entry.header.block_hash() == self.bootstrap.block.hash),
            "bootstrap location has no archived header"
        );
        Ok(())
    }

    /// Decode the actual checked carrier, including its witness, rather than trusting file-supplied content.
    pub async fn bootstrap(
        &self,
        identity: bitcoin::Txid,
        client: &BitcoinClient,
    ) -> anyhow::Result<zksync_types::via_bootstrap::BootstrapState> {
        use crate::{indexer::MessageParser, types::FullInscriptionMessage};
        self.validate()?;
        let header =
            self.headers.iter().find(|entry| entry.height == self.bootstrap.block.height).unwrap();
        let block = header.checked_block(client).await?;
        let transaction =
            block.block.txdata.get(usize::try_from(self.bootstrap.transaction_index)?).ok_or_else(
                || anyhow::anyhow!("bootstrap transaction index is outside its checked body"),
            )?;
        anyhow::ensure!(
            transaction.compute_txid() == identity,
            "bootstrap carrier identity differs"
        );
        let parsed = MessageParser::new(self.network).parse_system_transaction(
            transaction,
            block.height,
            None,
        );
        anyhow::ensure!(
            parsed.len() == 1,
            "bootstrap transaction must contain one bootstrap message"
        );
        let FullInscriptionMessage::SystemBootstrapping(message) = &parsed[0] else {
            anyhow::bail!("configured transaction is not a bootstrap message");
        };
        let input = &message.input;
        let state = zksync_types::via_bootstrap::BootstrapState {
            wallets: zksync_types::via_wallet::SystemWallets {
                sequencer: input.sequencer_address.clone().require_network(self.network)?,
                bridge: input.bridge_musig2_address.clone().require_network(self.network)?,
                governance: input.governance_address.clone().require_network(self.network)?,
                verifiers: input
                    .verifier_p2wpkh_addresses
                    .iter()
                    .cloned()
                    .map(|address| address.require_network(self.network))
                    .collect::<Result<_, _>>()?,
            },
            bootstrap_tx_id: message.common.tx_id,
            starting_block_number: input.start_block_height,
            bootloader_hash: input.bootloader_hash,
            abstract_account_hash: input.abstract_account_hash,
            snark_wrapper_vk_hash: input.snark_wrapper_vk_hash,
            evm_emulator_hash: input.evm_emulator_hash,
            protocol_version: input.protocol_version,
        };
        state.validate()?;
        Ok(state)
    }

    /// A later extension preserves this exact inspected prefix; a fork never silently rebases it.
    pub async fn revalidate(&self, client: &BitcoinClient) -> anyhow::Result<()> {
        self.validate()?;
        anyhow::ensure!(client.get_network() == self.network, "migration source network changed");
        anyhow::ensure!(
            client.block_hash_at_height(0).await? == self.genesis,
            "migration source genesis changed"
        );
        let current_tip = client.best_block_hash().await?;
        let current = client
            .header_by_hash(&current_tip)
            .await?
            .ok_or_else(|| anyhow::anyhow!("current source tip is unavailable"))?;
        client.canonical_header_at(current.height, current_tip).await?;
        anyhow::ensure!(
            current.height >= self.observed_tip.height,
            "source tip shrank below the inspected observation"
        );
        client.canonical_header_at(self.observed_tip.height, self.observed_tip.hash).await?;
        for entry in [self.headers.first().unwrap(), self.headers.last().unwrap()] {
            client.canonical_header_at(entry.height, entry.header.block_hash()).await?;
        }
        anyhow::ensure!(
            client.best_block_hash().await? == current_tip,
            "source changed during freshness validation"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reconstruction {
    Retained,
    RebuiltReversible,
    Fresh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageIdentity {
    pub source: SourcePoint,
    pub carrier: bitcoin::Txid,
    pub transaction_index: u32,
    pub message_index: u32,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceCensus {
    pub blocks: u32,
    pub messages: Vec<MessageIdentity>,
    pub retained_records: u64,
    pub rebuilt_records: u64,
    pub protected_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreIdentity {
    pub kind: String,
    pub location: String,
    pub position: u64,
    pub root: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupCertificate {
    pub group: String,
    pub baseline: SourcePoint,
    pub completed: SourcePoint,
    /// Explicitly distinguishes reconstructed reversible output from proved old inclusion history.
    pub reconstruction: Reconstruction,
    pub evidence: EvidenceCensus,
    pub gaps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationCertificate<DatabaseInput> {
    pub format_version: u32,
    pub role: String,
    pub bootstrap_txid: bitcoin::Txid,
    pub source: ChainArchive,
    pub database_input: DatabaseInput,
    pub stores: Vec<StoreIdentity>,
    pub groups: Vec<GroupCertificate>,
    pub conflicts: Vec<String>,
}

impl<DatabaseInput> MigrationCertificate<DatabaseInput> {
    pub fn validate(&self) -> anyhow::Result<()> {
        self.source.validate()?;
        anyhow::ensure!(self.format_version == 1, "unsupported semantic certificate version");
        anyhow::ensure!(
            matches!(self.role.as_str(), "main" | "verifier"),
            "unknown certificate role"
        );
        anyhow::ensure!(self.stores.len() <= 16, "certificate has too many external stores");
        let names = self
            .groups
            .iter()
            .map(|group| group.group.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let expected: std::collections::BTreeSet<_> =
            via_reorg::reader::ScanGroup::ALL.map(|group| group.as_str()).into_iter().collect();
        anyhow::ensure!(
            self.groups.len() == 4 && names == expected,
            "certificate must name all four groups exactly once"
        );
        for group in &self.groups {
            anyhow::ensure!(
                group.baseline.height <= group.completed.height,
                "group interval is reversed"
            );
            anyhow::ensure!(
                group.evidence.messages.len() <= 100_000,
                "certificate evidence census exceeds its bound"
            );
            for point in [group.baseline, group.completed] {
                anyhow::ensure!(
                    self.source.headers.iter().any(|block| block.height == point.height
                        && block.header.block_hash() == point.hash),
                    "group position has no checked archived block"
                );
            }
        }
        Ok(())
    }

    pub fn require_complete(&self) -> anyhow::Result<()> {
        self.validate()?;
        anyhow::ensure!(self.conflicts.is_empty(), "semantic certificate has immutable conflicts");
        anyhow::ensure!(
            self.groups.iter().all(|group| group.gaps.is_empty()),
            "semantic certificate has unresolved group gaps"
        );
        Ok(())
    }
}
