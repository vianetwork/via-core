use std::collections::HashMap;

use async_trait::async_trait;
use bitcoin::{Address, BlockHash, KnownHrp, Network, Txid};
use bitcoincore_rpc::Auth;
use tracing::{debug, error, info, instrument, warn};

mod parser;
use parser::MessageParser;

use crate::{
    client::BitcoinClient,
    traits::{BitcoinIndexerOpt, BitcoinOps},
    types,
    types::{BitcoinIndexerResult, CommonFields, FullInscriptionMessage, L1ToL2Message, Vote},
};

/// Represents the state during the bootstrap process
#[derive(Debug)]
struct BootstrapState {
    verifier_addresses: Vec<Address>,
    proposed_sequencer: Option<Address>,
    proposed_sequencer_txid: Option<Txid>,
    sequencer_votes: HashMap<Address, Vote>,
    bridge_address: Option<Address>,
    starting_block_number: u32,
}

impl BootstrapState {
    fn new() -> Self {
        Self {
            verifier_addresses: Vec::new(),
            proposed_sequencer: None,
            proposed_sequencer_txid: None,
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
        positive_votes * 2 > total_votes && total_votes == self.verifier_addresses.len()
    }
}

/// The main indexer struct for processing Bitcoin inscriptions
pub struct BitcoinInscriptionIndexer {
    client: Box<dyn BitcoinOps>,
    parser: MessageParser,
    bridge_address: Address,
    sequencer_address: Address,
    verifier_addresses: Vec<Address>,
    starting_block_number: u32,
    network: Network,
}

#[async_trait]
impl BitcoinIndexerOpt for BitcoinInscriptionIndexer {
    #[instrument(skip(rpc_url, network, bootstrap_txids), target = "bitcoin_indexer")]
    async fn new(
        rpc_url: &str,
        network: Network,
        bootstrap_txids: Vec<Txid>,
    ) -> BitcoinIndexerResult<Self>
    where
        Self: Sized,
    {
        info!("Creating new BitcoinInscriptionIndexer");
        let client = Box::new(BitcoinClient::new(rpc_url, network, Auth::None)?);
        let parser = MessageParser::new(network);
        let mut bootstrap_state = BootstrapState::new();

        for txid in bootstrap_txids {
            debug!("Processing bootstrap transaction: {}", txid);
            let tx = client.get_transaction(&txid).await?;
            let messages = parser.parse_transaction(&tx);

            for message in messages {
                Self::process_bootstrap_message(&mut bootstrap_state, message, txid);
            }

            if bootstrap_state.is_complete() {
                info!("Bootstrap process completed");
                break;
            }
        }

        Self::create_indexer(bootstrap_state, client, parser, network)
    }

    #[instrument(skip(self), target = "bitcoin_indexer")]
    async fn process_blocks(
        &self,
        starting_block: u32,
        ending_block: u32,
    ) -> BitcoinIndexerResult<Vec<FullInscriptionMessage>> {
        info!(
            "Processing blocks from {} to {}",
            starting_block, ending_block
        );
        let mut res = Vec::with_capacity((ending_block - starting_block + 1) as usize);
        for block in starting_block..=ending_block {
            res.extend(self.process_block(block).await?);
        }
        debug!("Processed {} blocks", ending_block - starting_block + 1);
        Ok(res)
    }

    #[instrument(skip(self), target = "bitcoin_indexer")]
    async fn process_block(
        &self,
        block_height: u32,
    ) -> BitcoinIndexerResult<Vec<FullInscriptionMessage>> {
        debug!("Processing block at height {}", block_height);
        if block_height < self.starting_block_number {
            error!("Attempted to process block before starting block");
            return Err(types::IndexerError::InvalidBlockHeight(block_height));
        }

        let block = self.client.fetch_block(block_height as u128).await?;

        let messages: Vec<_> = block
            .txdata
            .iter()
            .flat_map(|tx| self.parser.parse_transaction(tx))
            .filter(|message| self.is_valid_message(message))
            .collect();

        debug!(
            "Processed {} valid messages in block {}",
            messages.len(),
            block_height
        );
        Ok(messages)
    }

    #[instrument(skip(self), target = "bitcoin_indexer")]
    async fn are_blocks_connected(
        &self,
        parent_hash: &BlockHash,
        child_hash: &BlockHash,
    ) -> BitcoinIndexerResult<bool> {
        debug!(
            "Checking if blocks are connected: parent {}, child {}",
            parent_hash, child_hash
        );
        let child_block = self.client.fetch_block_by_hash(child_hash).await?;
        let are_connected = child_block.header.prev_blockhash == *parent_hash;
        debug!("Blocks connected: {}", are_connected);
        Ok(are_connected)
    }
}

impl BitcoinInscriptionIndexer {
    fn create_indexer(
        bootstrap_state: BootstrapState,
        client: Box<dyn BitcoinOps>,
        parser: MessageParser,
        network: Network,
    ) -> BitcoinIndexerResult<Self> {
        if bootstrap_state.is_complete() {
            if let (Some(bridge), Some(sequencer)) = (
                bootstrap_state.bridge_address,
                bootstrap_state.proposed_sequencer,
            ) {
                info!("BitcoinInscriptionIndexer successfully created");
                Ok(Self {
                    client,
                    parser,
                    bridge_address: bridge,
                    sequencer_address: sequencer,
                    verifier_addresses: bootstrap_state.verifier_addresses,
                    starting_block_number: bootstrap_state.starting_block_number,
                    network,
                })
            } else {
                error!("Incomplete bootstrap process despite state being marked as complete");
                Err(types::IndexerError::IncompleteBootstrap(
                    "Incomplete bootstrap process despite state being marked as complete"
                        .to_string(),
                ))
            }
        } else {
            error!("Bootstrap process did not complete with provided transactions");
            Err(types::IndexerError::IncompleteBootstrap(
                "Bootstrap process did not complete with provided transactions".to_string(),
            ))
        }
    }

    #[instrument(skip(self, message), target = "bitcoin_indexer")]
    fn is_valid_message(&self, message: &FullInscriptionMessage) -> bool {
        match message {
            FullInscriptionMessage::ProposeSequencer(m) => {
                let is_valid = Self::get_sender_address(&m.common, self.network)
                    .map_or(false, |addr| self.verifier_addresses.contains(&addr));
                debug!("ProposeSequencer message validity: {}", is_valid);
                is_valid
            }
            FullInscriptionMessage::ValidatorAttestation(m) => {
                let is_valid = Self::get_sender_address(&m.common, self.network)
                    .map_or(false, |addr| self.verifier_addresses.contains(&addr));
                debug!("ValidatorAttestation message validity: {}", is_valid);
                is_valid
            }
            FullInscriptionMessage::L1BatchDAReference(m) => {
                let is_valid = Self::get_sender_address(&m.common, self.network)
                    .map_or(false, |addr| addr == self.sequencer_address);
                debug!("L1BatchDAReference message validity: {}", is_valid);
                is_valid
            }
            FullInscriptionMessage::ProofDAReference(m) => {
                let is_valid = Self::get_sender_address(&m.common, self.network)
                    .map_or(false, |addr| addr == self.sequencer_address);
                debug!("ProofDAReference message validity: {}", is_valid);
                is_valid
            }
            FullInscriptionMessage::L1ToL2Message(m) => {
                let is_valid =
                    m.amount > bitcoin::Amount::ZERO && self.is_valid_l1_to_l2_transfer(m);
                debug!("L1ToL2Message validity: {}", is_valid);
                is_valid
            }
            FullInscriptionMessage::SystemBootstrapping(_) => {
                debug!("SystemBootstrapping message is always valid");
                true
            }
        }
    }

    #[instrument(skip(state, message), target = "bitcoin_indexer")]
    fn process_bootstrap_message(
        state: &mut BootstrapState,
        message: FullInscriptionMessage,
        txid: Txid,
    ) {
        match message {
            FullInscriptionMessage::SystemBootstrapping(sb) => {
                debug!("Processing SystemBootstrapping message");
                state.verifier_addresses = sb.input.verifier_p2wpkh_addresses;
                state.bridge_address = Some(sb.input.bridge_p2wpkh_mpc_address);
                state.starting_block_number = sb.input.start_block_height;
            }
            FullInscriptionMessage::ProposeSequencer(ps) => {
                debug!("Processing ProposeSequencer message");
                if let Some(sender_address) = Self::get_sender_address(&ps.common, Network::Testnet)
                {
                    // TODO: use actual network
                    if state.verifier_addresses.contains(&sender_address) {
                        state.proposed_sequencer = Some(ps.input.sequencer_new_p2wpkh_address);
                        state.proposed_sequencer_txid = Some(txid);
                    }
                }
            }
            FullInscriptionMessage::ValidatorAttestation(va) => {
                debug!("Processing ValidatorAttestation message");
                if state.proposed_sequencer.is_some() {
                    if let Some(sender_address) =
                        Self::get_sender_address(&va.common, Network::Testnet)
                    {
                        // TODO: use actual network
                        if state.verifier_addresses.contains(&sender_address) {
                            if let Some(proposed_txid) = state.proposed_sequencer_txid {
                                if va.input.reference_txid == proposed_txid {
                                    state
                                        .sequencer_votes
                                        .insert(sender_address, va.input.attestation);
                                }
                            }
                        }
                    }
                }
            }
            _ => {
                debug!("Ignoring non-bootstrap message during bootstrap process");
            }
        }
    }

    #[instrument(skip(self, message), target = "bitcoin_indexer")]
    fn is_valid_l1_to_l2_transfer(&self, message: &L1ToL2Message) -> bool {
        let is_valid = message
            .tx_outputs
            .iter()
            .any(|output| output.script_pubkey == self.bridge_address.script_pubkey());
        debug!("L1ToL2Message transfer validity: {}", is_valid);
        is_valid
    }

    #[instrument(skip(common_fields), target = "bitcoin_indexer")]
    fn get_sender_address(common_fields: &CommonFields, network: Network) -> Option<Address> {
        secp256k1::XOnlyPublicKey::from_slice(common_fields.encoded_public_key.as_bytes())
            .ok()
            .map(|public_key| {
                Address::p2tr(
                    &bitcoin::secp256k1::Secp256k1::new(),
                    public_key,
                    None,
                    KnownHrp::from(network),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bitcoin::{
        block::Header, hashes::Hash, Amount, Block, OutPoint, ScriptBuf, Transaction, TxMerkleNode,
        TxOut,
    };
    use mockall::{mock, predicate::*};
    use secp256k1::Secp256k1;

    use super::*;
    use crate::types::BitcoinClientResult;

    mock! {
        BitcoinOps {}
        #[async_trait]
        impl BitcoinOps for BitcoinOps {
            async fn get_transaction(&self, txid: &Txid) -> BitcoinClientResult<Transaction>;
            async fn fetch_block(&self, block_height: u128) -> BitcoinClientResult<Block>;
            async fn fetch_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinClientResult<Block>;
            async fn get_balance(&self, address: &Address) -> BitcoinClientResult<u128>;
            async fn broadcast_signed_transaction(&self, signed_transaction: &str) -> BitcoinClientResult<Txid>;
            async fn fetch_utxos(&self, address: &Address) -> BitcoinClientResult<Vec<(OutPoint, TxOut)>>;
            async fn check_tx_confirmation(&self, txid: &Txid, conf_num: u32) -> BitcoinClientResult<bool>;
            async fn fetch_block_height(&self) -> BitcoinClientResult<u128>;
            async fn get_fee_rate(&self, conf_target: u16) -> BitcoinClientResult<u64>;
            fn get_network(&self) -> Network;
        }
    }

    fn get_indexer_with_mock(mock_client: MockBitcoinOps) -> BitcoinInscriptionIndexer {
        let parser = MessageParser::new(Network::Testnet);
        let bridge_address = Address::from_str("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx")
            .unwrap()
            .require_network(Network::Testnet)
            .unwrap();
        let sequencer_address =
            Address::from_str("tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3q0sl5k7")
                .unwrap()
                .require_network(Network::Testnet)
                .unwrap();

        BitcoinInscriptionIndexer {
            client: Box::new(mock_client),
            parser,
            bridge_address,
            sequencer_address,
            verifier_addresses: vec![],
            starting_block_number: 0,
            network: Network::Testnet,
        }
    }

    #[tokio::test]
    async fn test_are_blocks_connected() {
        let parent_hash = BlockHash::all_zeros();
        let child_hash = BlockHash::all_zeros();
        let mock_block = Block {
            header: Header {
                version: Default::default(),
                prev_blockhash: parent_hash,
                merkle_root: TxMerkleNode::all_zeros(),
                time: 0,
                bits: Default::default(),
                nonce: 0,
            },
            txdata: vec![],
        };

        let mut mock_client = MockBitcoinOps::new();
        mock_client
            .expect_fetch_block_by_hash()
            .with(eq(child_hash))
            .returning(move |_| Ok(mock_block.clone()));
        mock_client
            .expect_get_network()
            .returning(|| Network::Testnet);

        let indexer = get_indexer_with_mock(mock_client);

        let result = indexer
            .are_blocks_connected(&parent_hash, &child_hash)
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn test_process_blocks() {
        let start_block = 1;
        let end_block = 3;

        let mock_block = Block {
            header: Header {
                version: Default::default(),
                prev_blockhash: BlockHash::all_zeros(),
                merkle_root: TxMerkleNode::all_zeros(),
                time: 0,
                bits: Default::default(),
                nonce: 0,
            },
            txdata: vec![],
        };

        let mut mock_client = MockBitcoinOps::new();
        mock_client
            .expect_fetch_block()
            .returning(move |_| Ok(mock_block.clone()))
            .times(3);
        mock_client
            .expect_get_network()
            .returning(|| Network::Testnet);

        let indexer = get_indexer_with_mock(mock_client);
        let result = indexer.process_blocks(start_block, end_block).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_is_valid_message() {
        let indexer = get_indexer_with_mock(MockBitcoinOps::new());

        let propose_sequencer = FullInscriptionMessage::ProposeSequencer(types::ProposeSequencer {
            common: CommonFields {
                schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
                encoded_public_key: bitcoin::script::PushBytesBuf::from([0u8; 32]),
            },
            input: types::ProposeSequencerInput {
                sequencer_new_p2wpkh_address: indexer.sequencer_address.clone(),
            },
        });
        assert!(!indexer.is_valid_message(&propose_sequencer));

        let validator_attestation =
            FullInscriptionMessage::ValidatorAttestation(types::ValidatorAttestation {
                common: CommonFields {
                    schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
                    encoded_public_key: bitcoin::script::PushBytesBuf::from([0u8; 32]),
                },
                input: types::ValidatorAttestationInput {
                    reference_txid: Txid::all_zeros(),
                    attestation: Vote::Ok,
                },
            });
        assert!(!indexer.is_valid_message(&validator_attestation));

        let l1_batch_da_reference =
            FullInscriptionMessage::L1BatchDAReference(types::L1BatchDAReference {
                common: CommonFields {
                    schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
                    encoded_public_key: bitcoin::script::PushBytesBuf::from([0u8; 32]),
                },
                input: types::L1BatchDAReferenceInput {
                    l1_batch_hash: zksync_basic_types::H256::zero(),
                    l1_batch_index: zksync_types::L1BatchNumber(0),
                    da_identifier: "test".to_string(),
                    blob_id: "test".to_string(),
                },
            });
        assert!(!indexer.is_valid_message(&l1_batch_da_reference));

        let l1_to_l2_message = FullInscriptionMessage::L1ToL2Message(L1ToL2Message {
            common: CommonFields {
                schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
                encoded_public_key: bitcoin::script::PushBytesBuf::from([0u8; 32]),
            },
            amount: Amount::from_sat(1000),
            input: types::L1ToL2MessageInput {
                receiver_l2_address: zksync_types::Address::zero(),
                l2_contract_address: zksync_types::Address::zero(),
                call_data: vec![],
            },
            tx_outputs: vec![TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: indexer.bridge_address.script_pubkey(),
            }],
        });
        assert!(indexer.is_valid_message(&l1_to_l2_message));

        let system_bootstrapping =
            FullInscriptionMessage::SystemBootstrapping(types::SystemBootstrapping {
                common: CommonFields {
                    schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
                    encoded_public_key: bitcoin::script::PushBytesBuf::from([0u8; 32]),
                },
                input: types::SystemBootstrappingInput {
                    start_block_height: 0,
                    bridge_p2wpkh_mpc_address: indexer.bridge_address.clone(),
                    verifier_p2wpkh_addresses: vec![],
                },
            });
        assert!(indexer.is_valid_message(&system_bootstrapping));
    }

    #[test]
    fn test_get_sender_address() {
        let network = Network::Testnet;
        let secp = Secp256k1::new();
        let (_secret_key, p) = secp.generate_keypair(&mut rand::thread_rng());
        let x_only_public_key = p.x_only_public_key();

        let common_fields = CommonFields {
            schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
            encoded_public_key: bitcoin::script::PushBytesBuf::from(
                x_only_public_key.0.serialize(),
            ),
        };

        let sender_address = BitcoinInscriptionIndexer::get_sender_address(&common_fields, network);
        assert!(sender_address.is_some());

        let expected_address =
            Address::p2tr(&secp, x_only_public_key.0, None, KnownHrp::from(network));
        assert_eq!(sender_address.unwrap(), expected_address);
    }

    #[tokio::test]
    async fn test_is_valid_l1_to_l2_transfer() {
        let indexer = get_indexer_with_mock(MockBitcoinOps::new());

        let valid_message = L1ToL2Message {
            common: CommonFields {
                schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
                encoded_public_key: bitcoin::script::PushBytesBuf::from([0u8; 32]),
            },
            amount: Amount::from_sat(1000),
            input: types::L1ToL2MessageInput {
                receiver_l2_address: zksync_types::Address::zero(),
                l2_contract_address: zksync_types::Address::zero(),
                call_data: vec![],
            },
            tx_outputs: vec![TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: indexer.bridge_address.script_pubkey(),
            }],
        };
        assert!(indexer.is_valid_l1_to_l2_transfer(&valid_message));

        let invalid_message = L1ToL2Message {
            common: CommonFields {
                schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
                encoded_public_key: bitcoin::script::PushBytesBuf::from([0u8; 32]),
            },
            amount: Amount::from_sat(1000),
            input: types::L1ToL2MessageInput {
                receiver_l2_address: zksync_types::Address::zero(),
                l2_contract_address: zksync_types::Address::zero(),
                call_data: vec![],
            },
            tx_outputs: vec![TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: ScriptBuf::new(),
            }],
        };
        assert!(!indexer.is_valid_l1_to_l2_transfer(&invalid_message));
    }
}
