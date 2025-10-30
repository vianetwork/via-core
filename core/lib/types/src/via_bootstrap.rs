use bitcoin::Txid;
use zksync_basic_types::{protocol_version::ProtocolSemanticVersion, H256};

use crate::via_wallet::SystemWallets;

#[derive(Debug, Clone)]
pub struct BootstrapState {
    pub wallets: SystemWallets,
    pub bootstrap_tx_id: Txid,
    pub starting_block_number: u32,
    pub bootloader_hash: H256,
    pub abstract_account_hash: H256,
    pub snark_wrapper_vk_hash: H256,
    pub evm_emulator_hash: H256,
    pub protocol_version: ProtocolSemanticVersion,
}

impl BootstrapState {
    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.wallets.sequencer.script_pubkey().is_p2wpkh() {
            anyhow::bail!("Sequencer must be P2WPKH");
        }

        if !self.wallets.bridge.script_pubkey().is_p2tr() {
            anyhow::bail!("Bridge must be Taproot");
        }

        if !self.wallets.governance.script_pubkey().is_p2wsh() {
            anyhow::bail!("Governance must be P2WSH");
        }

        if !self
            .wallets
            .verifiers
            .iter()
            .all(|a| a.script_pubkey().is_p2wpkh())
        {
            anyhow::bail!("All verifiers must be P2WPKH");
        }

        if self.starting_block_number == 0 {
            anyhow::bail!("Starting block number must be > 0");
        }

        Ok(())
    }
}
