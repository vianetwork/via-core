use std::collections::HashMap;

use async_trait::async_trait;
use bitcoin::{Address, BlockHash, KnownHrp, Network, Transaction, Txid};
use bitcoincore_rpc::Auth;

mod parser;
use parser::MessageParser;

use crate::{
    client::BitcoinClient,
    traits::BitcoinIndexerOpt,
    types::{BitcoinError, BitcoinIndexerResult, CommonFields, Message, Vote},
    BitcoinOps,
};

struct BootstrapState {
    verifier_addresses: Vec<Address>,
    proposed_sequencer: Option<Address>,
    sequencer_votes: HashMap<Address, Vote>,
    bridge_address: Option<Address>,
    starting_block_number: u32,
}

impl BootstrapState {
    fn new() -> Self {
        Self {
            verifier_addresses: Vec::new(),
            proposed_sequencer: None,
            sequencer_votes: HashMap::new(),
            bridge_address: None,
            starting_block_number: 0,
        }
    }

    fn is_complete(&self) -> bool {
        !self.verifier_addresses.is_empty()
            && self.proposed_sequencer.is_some()
            && self.bridge_address.is_some()
            && self.starting_block_number > 0
            && self.has_majority_votes()
    }

    fn has_majority_votes(&self) -> bool {
        let total_votes = self.sequencer_votes.len();
        let positive_votes = self
            .sequencer_votes
            .values()
            .filter(|&v| *v == Vote::Ok)
            .count();
        positive_votes * 2 > total_votes
    }
}

pub struct BitcoinInscriptionIndexer {
    client: Box<dyn BitcoinOps>,
    parser: MessageParser,
    bridge_address: Address,
    sequencer_address: Address,
    verifier_addresses: Vec<Address>,
    starting_block_number: u32,
}

#[async_trait]
impl BitcoinIndexerOpt for BitcoinInscriptionIndexer {
    async fn new(
        rpc_url: &str,
        network: Network,
        bootstrap_txids: Vec<Txid>,
    ) -> BitcoinIndexerResult<Self>
    where
        Self: Sized,
    {
        let client = Box::new(BitcoinClient::new(rpc_url, network, Auth::None).await?);
        let parser = MessageParser::new(network);
        let mut bootstrap_state = BootstrapState::new();

        for txid in bootstrap_txids {
            let tx = client.get_rpc_client().get_transaction(&txid).await?;
            let messages = parser.parse_transaction(&tx);

            for message in messages {
                Self::process_bootstrap_message(&mut bootstrap_state, message)?;
            }

            if bootstrap_state.is_complete() {
                break;
            }
        }

        if bootstrap_state.is_complete() {
            if let (Some(bridge), Some(sequencer)) = (
                bootstrap_state.bridge_address,
                bootstrap_state.proposed_sequencer,
            ) {
                Ok(Self {
                    client,
                    parser,
                    bridge_address: bridge,
                    sequencer_address: sequencer,
                    verifier_addresses: bootstrap_state.verifier_addresses,
                    starting_block_number: bootstrap_state.starting_block_number,
                })
            } else {
                Err(BitcoinError::Other(
                    "Incomplete bootstrap process despite state being marked as complete"
                        .to_string(),
                ))
            }
        } else {
            Err(BitcoinError::Other(
                "Bootstrap process did not complete with provided transactions".to_string(),
            ))
        }
    }

    async fn process_blocks(
        &self,
        starting_block: u32,
        ending_block: u32,
    ) -> BitcoinIndexerResult<Vec<Message>> {
        let mut res = Vec::with_capacity((ending_block - starting_block + 1) as usize);
        for block in starting_block..=ending_block {
            res.extend(self.process_block(block).await?);
        }
        Ok(res)
    }

    async fn process_block(&self, block: u32) -> BitcoinIndexerResult<Vec<Message>> {
        if block < self.starting_block_number {
            return Err(BitcoinError::Other(
                "Indexer error: can't get block before starting block".to_string(),
            ));
        }

        let block = self
            .client
            .get_rpc_client()
            .get_block_by_height(block as u128)
            .await?;

        let messages: Vec<Message> = block
            .txdata
            .iter()
            .flat_map(|tx| self.process_tx(tx))
            .filter(|message| self.is_valid_message(message))
            .collect();

        Ok(messages)
    }

    async fn are_blocks_connected(
        &self,
        parent_hash: &BlockHash,
        child_hash: &BlockHash,
    ) -> BitcoinIndexerResult<bool> {
        let child_block = self
            .client
            .get_rpc_client()
            .get_block_by_hash(child_hash)
            .await?;
        Ok(child_block.header.prev_blockhash == *parent_hash)
    }
}

impl BitcoinInscriptionIndexer {
    fn process_tx(&self, tx: &Transaction) -> Vec<Message> {
        self.parser.parse_transaction(tx)
    }

    fn is_valid_message(&self, message: &Message) -> bool {
        match message {
            Message::ProposeSequencer(m) => {
                if let Ok(sender) = Self::get_sender_address(&m.common) {
                    self.verifier_addresses.contains(&sender)
                } else {
                    false
                }
            }
            Message::ValidatorAttestation(m) => {
                if let Ok(sender) = Self::get_sender_address(&m.common) {
                    self.verifier_addresses.contains(&sender)
                } else {
                    false
                }
            }
            Message::L1BatchDAReference(m) => {
                if let Ok(sender) = Self::get_sender_address(&m.common) {
                    sender == self.sequencer_address
                } else {
                    false
                }
            }
            Message::ProofDAReference(m) => {
                if let Ok(sender) = Self::get_sender_address(&m.common) {
                    sender == self.sequencer_address
                } else {
                    false
                }
            }
            Message::L1ToL2Message(m) => m.amount > bitcoin::Amount::ZERO,
            Message::SystemBootstrapping(_) => true,
        }
    }

    fn process_bootstrap_message(
        state: &mut BootstrapState,
        message: Message,
    ) -> BitcoinIndexerResult<()> {
        match message {
            Message::SystemBootstrapping(sb) => {
                state.verifier_addresses = sb.input.verifier_addresses;
                state.bridge_address = Some(sb.input.bridge_p2wpkh_mpc_address);
                state.starting_block_number = sb.input.start_block_height;
            }
            Message::ProposeSequencer(ps) => {
                if let Ok(sender) = Self::get_sender_address(&ps.common) {
                    if state.verifier_addresses.contains(&sender) {
                        state.proposed_sequencer = Some(ps.input.sequencer_p2wpkh_address);
                    }
                }
            }
            Message::ValidatorAttestation(va) => {
                if state.proposed_sequencer.is_some() {
                    if let Ok(sender) = Self::get_sender_address(&va.common) {
                        if state.verifier_addresses.contains(&sender) {
                            state.sequencer_votes.insert(sender, va.input.attestation);
                        }
                    }
                }
            }
            _ => {
                return Err(BitcoinError::Other(
                    "Unexpected message during bootstrap".to_string(),
                ))
            }
        }
        Ok(())
    }

    fn get_sender_address(common_fields: &CommonFields) -> BitcoinIndexerResult<Address> {
        let public_key =
            secp256k1::XOnlyPublicKey::from_slice(&common_fields.encoded_public_key.as_bytes())
                .map_err(|_| BitcoinError::Other("Invalid public key".to_string()))?;
        Ok(Address::p2tr(
            &bitcoin::secp256k1::Secp256k1::new(),
            public_key,
            None,
            KnownHrp::from(Network::Testnet), // TODO: make it configurable
        ))
    }
}
