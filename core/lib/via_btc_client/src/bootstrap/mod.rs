use std::{str::FromStr, sync::Arc};

use bitcoin::Txid;
use zksync_config::configs::via_consensus::ViaGenesisConfig;
use zksync_types::{via_bootstrap::BootstrapState, via_wallet::SystemWallets};

use crate::{
    client::BitcoinClient, indexer::MessageParser, traits::BitcoinOps,
    types::FullInscriptionMessage,
};

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
        let mut parser = MessageParser::new(network);

        let bootstrap_txid = self
            .config
            .bootstrap_txids
            .first()
            .ok_or_else(|| anyhow::anyhow!("Bootstrap transaction not found"))?;

        let txid = Txid::from_str(&bootstrap_txid)?;
        let tx = self.client.get_transaction(&txid).await?;
        let block_height = self.client.fetch_block_height().await? as u32;
        let messages = parser.parse_system_transaction(&tx, block_height, None);

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
            .map(|addr| addr.clone().require_network(network).unwrap())
            .collect::<Vec<_>>();

        let bootstrap_state = BootstrapState {
            wallets: SystemWallets {
                sequencer: state
                    .input
                    .sequencer_address
                    .require_network(network)
                    .unwrap(),
                bridge: state
                    .input
                    .bridge_musig2_address
                    .require_network(network)
                    .unwrap(),
                governance: state
                    .input
                    .governance_address
                    .require_network(network)
                    .unwrap(),
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
}
