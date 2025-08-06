use std::collections::HashMap;

use bitcoin::{Address, Txid};
use zksync_basic_types::H256;

use crate::via_wallet::SystemWallets;

#[derive(Debug, Clone, Default)]
pub struct BootstrapState {
    pub wallets: Option<SystemWallets>,
    pub sequencer_proposal_tx_id: Option<Txid>,
    pub bootstrap_tx_id: Option<Txid>,
    pub sequencer_votes: HashMap<Address, bool>,
    pub starting_block_number: u32,
    pub bootloader_hash: Option<H256>,
    pub abstract_account_hash: Option<H256>,
}

impl BootstrapState {
    pub fn validate(&self) -> anyhow::Result<()> {
        let wallets = self
            .wallets
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Wallets missing"))?;

        if !wallets.sequencer.script_pubkey().is_p2wpkh() {
            anyhow::bail!("Sequencer must be P2WPKH");
        }
        if !wallets.bridge.script_pubkey().is_p2tr() {
            anyhow::bail!("Bridge must be Taproot");
        }
        if !wallets.governance.script_pubkey().is_p2wsh() {
            anyhow::bail!("Governance must be P2WSH");
        }
        if !wallets
            .verifiers
            .iter()
            .all(|a| a.script_pubkey().is_p2wpkh())
        {
            anyhow::bail!("All verifiers must be P2WPKH");
        }
        if self.starting_block_number == 0 {
            anyhow::bail!("Starting block number must be > 0");
        }
        if !self.has_majority_votes(wallets.verifiers.len()) {
            anyhow::bail!("Majority votes missing");
        }
        if self.sequencer_proposal_tx_id.is_none() {
            anyhow::bail!("sequencer_proposal_tx_id missing");
        }
        if self.bootstrap_tx_id.is_none() {
            anyhow::bail!("bootstrap_tx_id missing");
        }
        if self.bootloader_hash.is_none() {
            anyhow::bail!("Bootloader hash missing");
        }
        if self.abstract_account_hash.is_none() {
            anyhow::bail!("Abstract account hash missing");
        }

        Ok(())
    }

    fn has_majority_votes(&self, verifier_count: usize) -> bool {
        let total_votes = self.sequencer_votes.len();
        let positive_votes = self.sequencer_votes.values().filter(|&&v| v).count();

        positive_votes * 2 > total_votes && total_votes == verifier_count
    }
}
