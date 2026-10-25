use std::{str::FromStr, sync::Arc};

use bitcoin::Txid;
use zksync_config::configs::via_consensus::ViaGenesisConfig;
use zksync_types::{via_bootstrap::BootstrapState, via_wallet::SystemWallets};

use crate::{client::BitcoinClient, traits::BitcoinOps, types::FullInscriptionMessage};

#[derive(Debug, Clone)]
pub struct ViaBootstrap {
    pub config: ViaGenesisConfig,
    pub client: Arc<BitcoinClient>,
}

impl ViaBootstrap {
    pub fn new(client: Arc<BitcoinClient>, config: ViaGenesisConfig) -> Self {
        Self { client, config }
    }

    pub async fn process_bootstrap_messages(&self) -> anyhow::Result<BootstrapState> {
        let network = self.client.get_network();

        let bootstrap_txid = self
            .config
            .bootstrap_txids
            .first()
            .ok_or_else(|| anyhow::anyhow!("Bootstrap transaction not found"))?;

        let txid = Txid::from_str(bootstrap_txid)?;
        let messages =
            self.client.canonical_messages(&txid).await?.ok_or_else(|| {
                anyhow::anyhow!("Bootstrap transaction has no canonical inclusion")
            })?;

        let message = messages
            .first()
            .ok_or_else(|| anyhow::anyhow!("Bootstrap message not found"))?;

        let state = match message {
            FullInscriptionMessage::SystemBootstrapping(sb) => sb.clone(),
            _ => anyhow::bail!("Invalid Bootstrap message"),
        };

        let verifiers = state
            .input
            .verifier_p2wpkh_addresses
            .iter()
            .map(|addr| addr.clone().require_network(network))
            .collect::<Result<Vec<_>, _>>()?;

        let bootstrap_state = BootstrapState {
            wallets: SystemWallets {
                sequencer: state
                    .input
                    .sequencer_address
                    .require_network(network)?,
                bridge: state
                    .input
                    .bridge_musig2_address
                    .require_network(network)?,
                governance: state
                    .input
                    .governance_address
                    .require_network(network)?,
                verifiers,
            },
            bootstrap_tx_id: state.common.tx_id,
            starting_block_number: state.input.start_block_height,
            bootloader_hash: state.input.bootloader_hash,
            abstract_account_hash: state.input.abstract_account_hash,
            snark_wrapper_vk_hash: state.input.snark_wrapper_vk_hash,
            evm_emulator_hash: state.input.evm_emulator_hash,
            protocol_version: state.input.protocol_version,
        };

        // Validate the final state
        bootstrap_state.validate()?;

        Ok(bootstrap_state)
    }

    /// The declared scan start is checked independently of the bootstrap transaction's inclusion.
    pub async fn checked_reader_bootstrap(&self) -> anyhow::Result<CheckedBootstrap> {
        let state = self.process_bootstrap_messages().await?;
        let genesis = self.client.canonical_header_at(0).await?;
        anyhow::ensure!(
            genesis.hash
                == bitcoin::blockdata::constants::genesis_block(self.client.get_network())
                    .block_hash(),
            "Bitcoin genesis does not match configured network"
        );
        let starting = self.client.canonical_header_at(state.starting_block_number).await?;
        let selected_hash = self.client.best_block_hash().await?;
        let selected = self
            .client
            .header_by_hash(&selected_hash)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Selected Bitcoin tip is unavailable"))?;
        anyhow::ensure!(
            matches!(selected.membership, crate::types::HeaderMembership::Canonical(_)),
            "Selected tip changed"
        );
        Ok(CheckedBootstrap { state, genesis, starting, selected })
    }
}

pub struct CheckedBootstrap {
    pub state: BootstrapState,
    pub genesis: crate::types::BlockHeaderInfo,
    pub starting: crate::types::BlockHeaderInfo,
    pub selected: crate::types::BlockHeaderInfo,
}

impl CheckedBootstrap {
    /// revalidate rereads Bitcoin source right before first write.
    /// Chain change between preparation and commit cannot anchor reader on replaced block.
    /// Idea only from Hyperlane near-head store, which rechecks expected state inside write: https://github.com/hyperlane-xyz/hyperlane-monorepo/blob/693d2d45df75135504e424c5a4d16761d0016f99/rust/main/agents/scraper/src/near_head/store.rs
    pub async fn revalidate(&self, client: &BitcoinClient) -> anyhow::Result<()> {
        anyhow::ensure!(
            client.canonical_header_at(self.starting.height).await?.hash == self.starting.hash
                && client.best_block_hash().await? == self.selected.hash,
            "Bitcoin source changed during startup"
        );
        Ok(())
    }
}
