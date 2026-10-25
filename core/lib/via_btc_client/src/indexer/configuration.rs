//! Prepare related Bitcoin inputs before the role opens its short write transaction.

use std::collections::HashMap;

use anyhow::{ensure, Context};
use bitcoin::{Network, Txid};
use serde::{Deserialize, Serialize};
use zksync_types::via_wallet::{SystemWalletsDetails, WalletInfo, WalletRole};

use super::replay::Included;
use crate::{
    client::BitcoinClient,
    types::{
        FullInscriptionMessage, SystemContractUpgradeInput, SystemContractUpgradeProposalInput,
        UpdateBridgeInput, UpdateBridgeProposalInput, UpdateGovernanceInput, UpdateSequencerInput,
    },
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConfigurationInput {
    Governance(UpdateGovernanceInput),
    Sequencer(UpdateSequencerInput),
    Bridge(UpdateBridgeInput, Option<Box<Included<UpdateBridgeProposalInput>>>),
    Protocol(SystemContractUpgradeInput, Vec<Included<SystemContractUpgradeProposalInput>>),
}

impl ConfigurationInput {
    pub fn role(&self) -> &'static str {
        match self {
            Self::Governance(_) => "Gov",
            Self::Sequencer(_) => "Sequencer",
            Self::Bridge(..) => "Bridge",
            Self::Protocol(..) => "Protocol",
        }
    }

    pub fn valid_proposal(&self) -> bool {
        match self {
            Self::Bridge(_, proposal) => proposal.is_some(),
            Self::Protocol(_, proposals) => proposals.len() == 1,
            Self::Governance(_) | Self::Sequencer(_) => true,
        }
    }
}

/// prepare_configurations fetches each governance message's proposal from Bitcoin before any SQL runs.
/// Proposal must sit at or before approval height, and write transaction stays short and never waits on RPC.
/// Idea only from Hyperlane near-head pipeline, which gathers chain data first and appends in one store call: https://github.com/hyperlane-xyz/hyperlane-monorepo/blob/693d2d45df75135504e424c5a4d16761d0016f99/rust/main/agents/scraper/src/near_head/pipeline.rs
pub async fn prepare_configurations(
    client: &BitcoinClient,
    messages: &[FullInscriptionMessage],
) -> anyhow::Result<Vec<Included<ConfigurationInput>>> {
    let mut prepared = Vec::new();
    for message in messages {
        let input = match message {
            FullInscriptionMessage::UpdateGovernance(message) => {
                ConfigurationInput::Governance(message.input.clone())
            }
            FullInscriptionMessage::UpdateSequencer(message) => {
                ConfigurationInput::Sequencer(message.input.clone())
            }
            FullInscriptionMessage::UpdateBridge(message) => {
                let related = client
                    .canonical_messages(&message.input.proposal_tx_id)
                    .await?
                    .context("bridge proposal has no canonical inclusion")?;
                let proposal = match related.into_iter().next() {
                    Some(FullInscriptionMessage::UpdateBridgeProposal(proposal)) => {
                        Some(Box::new(Included::new(&proposal.common, proposal.input)?))
                    }
                    _ => None,
                };
                if let Some(proposal) = &proposal {
                    ensure!(
                        proposal.origin.height <= message.common.block_height,
                        "bridge proposal is outside approval prefix"
                    );
                }
                ConfigurationInput::Bridge(message.input.clone(), proposal)
            }
            FullInscriptionMessage::SystemContractUpgrade(message) => {
                let related = client
                    .canonical_messages(&message.input.proposal_tx_id)
                    .await?
                    .context("protocol proposal has no canonical inclusion")?;
                let mut proposals = Vec::new();
                for related in related {
                    if let FullInscriptionMessage::SystemContractUpgradeProposal(proposal) = related
                    {
                        ensure!(
                            proposal.common.block_height <= message.common.block_height,
                            "protocol proposal is outside approval prefix"
                        );
                        proposals.push(Included::new(&proposal.common, proposal.input)?);
                    }
                }
                ConfigurationInput::Protocol(message.input.clone(), proposals)
            }
            _ => continue,
        };
        prepared.push(Included::new(message.common(), input)?);
    }
    Ok(prepared)
}

/// Existing Via wallet projections, shared by both authority roles and the independent indexer.
pub fn project_wallets(
    input: &ConfigurationInput,
    txid: Txid,
    network: Network,
) -> anyhow::Result<SystemWalletsDetails> {
    let mut roles = HashMap::new();
    let mut insert = |role, addresses| {
        roles.insert(role, WalletInfo { addresses, txid });
    };
    match input {
        ConfigurationInput::Governance(input) => {
            insert(WalletRole::Gov, vec![input.address.clone().assume_checked()])
        }
        ConfigurationInput::Sequencer(input) => {
            insert(WalletRole::Sequencer, vec![input.address.clone().assume_checked()])
        }
        ConfigurationInput::Bridge(_, Some(proposal)) => {
            return project_bridge_wallets(&proposal.payload.input, txid, network);
        }
        ConfigurationInput::Bridge(_, None) | ConfigurationInput::Protocol(..) => {}
    }
    Ok(SystemWalletsDetails(roles))
}

pub fn project_bridge_wallets(
    input: &crate::types::UpdateBridgeProposalInput,
    txid: Txid,
    network: Network,
) -> anyhow::Result<SystemWalletsDetails> {
    let mut roles = HashMap::new();
    roles.insert(
        WalletRole::Bridge,
        WalletInfo {
            addresses: vec![input.bridge_musig2_address.clone().require_network(network)?],
            txid,
        },
    );
    roles.insert(
        WalletRole::Verifier,
        WalletInfo {
            addresses: input
                .verifier_p2wpkh_addresses
                .iter()
                .map(|address| address.clone().assume_checked())
                .collect(),
            txid,
        },
    );
    Ok(SystemWalletsDetails(roles))
}
