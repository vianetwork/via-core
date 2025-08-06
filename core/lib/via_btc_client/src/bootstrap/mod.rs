use std::{str::FromStr, sync::Arc};

use bitcoin::{Address, Txid};
use zksync_config::configs::via_consensus::ViaGenesisConfig;
use zksync_types::{via_bootstrap::BootstrapState, via_wallet::SystemWallets};

use crate::{
    client::BitcoinClient,
    indexer::MessageParser,
    traits::BitcoinOps,
    types::{FullInscriptionMessage, Vote},
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
        let mut state = BootstrapState::default();

        let mut sequencer: Option<Address> = None;
        let mut bridge: Option<Address> = None;
        let mut governance: Option<Address> = None;
        let mut verifiers: Vec<Address> = vec![];

        for txid_str in self.config.bootstrap_txids.clone() {
            let txid = Txid::from_str(&txid_str)?;
            let tx = self.client.get_transaction(&txid).await?;
            let messages = parser.parse_system_transaction(&tx, 0);

            for message in messages {
                match message {
                    FullInscriptionMessage::SystemBootstrapping(sb) => {
                        tracing::debug!("Processing SystemBootstrapping message");

                        let verifier_addresses = sb
                            .input
                            .verifier_p2wpkh_addresses
                            .iter()
                            .map(|addr| addr.clone().require_network(network).unwrap())
                            .collect::<Vec<_>>();
                        verifiers.extend(verifier_addresses);

                        bridge = Some(
                            sb.input
                                .bridge_musig2_address
                                .require_network(network)
                                .unwrap(),
                        );
                        governance = Some(
                            sb.input
                                .governance_address
                                .require_network(network)
                                .unwrap(),
                        );

                        state.starting_block_number = sb.input.start_block_height;
                        state.bootloader_hash = Some(sb.input.bootloader_hash);
                        state.abstract_account_hash = Some(sb.input.abstract_account_hash);

                        state.bootstrap_tx_id = Some(txid);
                    }
                    FullInscriptionMessage::ProposeSequencer(ps) => {
                        tracing::debug!("Processing ProposeSequencer message");
                        let sequencer_address = ps
                            .input
                            .sequencer_new_p2wpkh_address
                            .require_network(network)
                            .unwrap();
                        sequencer = Some(sequencer_address);
                        state.sequencer_proposal_tx_id = Some(txid);
                    }
                    FullInscriptionMessage::ValidatorAttestation(va) => {
                        let p2wpkh_address = va
                            .common
                            .p2wpkh_address
                            .as_ref()
                            .expect("ValidatorAttestation must have a p2wpkh address");

                        if verifiers.contains(p2wpkh_address) {
                            if va.input.reference_txid == state.sequencer_proposal_tx_id.unwrap() {
                                state.sequencer_votes.insert(
                                    p2wpkh_address.clone(),
                                    va.input.attestation == Vote::Ok,
                                );
                            }
                        }
                    }
                    _ => {
                        tracing::debug!("Ignoring non-bootstrap message during bootstrap process");
                    }
                }
            }
        }

        // Construct SystemWallets and put them into state
        if let (Some(seq), Some(br), Some(gov)) = (sequencer, bridge, governance) {
            state.wallets = Some(SystemWallets {
                sequencer: seq,
                bridge: br,
                governance: gov,
                verifiers,
            });
        }

        // Validate the final state
        state.validate()?;

        Ok(state)
    }
}
