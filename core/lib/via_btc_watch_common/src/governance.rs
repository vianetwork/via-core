use std::sync::Arc;

use via_btc_client::{
    client::BitcoinClient,
    indexer::MessageParser,
    traits::BitcoinOps,
    types::{BitcoinTxid as Txid, FullInscriptionMessage, SystemContractUpgradeProposalInput},
};
use zksync_types::{protocol_version::ProtocolSemanticVersion, Address as EVMAddress, H256};

#[derive(Clone, Debug)]
pub struct GovernanceProposal {
    pub version: ProtocolSemanticVersion,
    pub bootloader_code_hash: H256,
    pub default_account_code_hash: H256,
    pub evm_emulator_code_hash: Option<H256>,
    pub recursion_scheduler_level_vk_hash: H256,
    pub system_contracts: Vec<(EVMAddress, H256)>,
}

pub async fn extract_governance_proposals(
    btc_client: &Arc<BitcoinClient>,
    message_parser: &mut MessageParser,
    proposal_tx_id: &Txid,
    block_height: u32,
) -> anyhow::Result<Vec<GovernanceProposal>> {
    let proposal_tx = btc_client
        .get_transaction(proposal_tx_id)
        .await
        .map_err(|err| {
            anyhow::anyhow!(
                "Failed to fetch protocol upgrade transaction: {}, error {}",
                proposal_tx_id,
                err
            )
        })?;

    let messages = message_parser.parse_system_transaction(&proposal_tx, block_height, None);

    let mut upgrades = Vec::new();
    for message in messages {
        if let FullInscriptionMessage::SystemContractUpgradeProposal(proposal) = message {
            let SystemContractUpgradeProposalInput {
                version,
                bootloader_code_hash,
                default_account_code_hash,
                evm_emulator_code_hash,
                recursion_scheduler_level_vk_hash,
                system_contracts,
            } = proposal.input;

            upgrades.push(GovernanceProposal {
                version,
                bootloader_code_hash,
                default_account_code_hash,
                evm_emulator_code_hash,
                recursion_scheduler_level_vk_hash,
                system_contracts,
            });
        }
    }

    Ok(upgrades)
}
