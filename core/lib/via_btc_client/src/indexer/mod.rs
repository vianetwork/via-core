use std::{collections::HashMap, sync::Arc};

use bitcoin::{Address, Amount, BlockHash, Network, Transaction as BitcoinTransaction, Txid};
use tracing::{debug, error, info, instrument, warn};

mod parser;
pub use parser::{get_eth_address, MessageParser};
use zksync_basic_types::L1BatchNumber;
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;
use zksync_types::H256;

use crate::{
    client::BitcoinClient,
    traits::BitcoinOps,
    types::{
        self, BitcoinIndexerResult, BridgeWithdrawal, FullInscriptionMessage, L1ToL2Message,
        SystemContractUpgrade, TransactionWithMetadata, Vote,
    },
};

/// Represents the state during the bootstrap process
#[derive(Debug, Clone)]
pub struct BootstrapState {
    pub verifier_addresses: Vec<Address>,
    pub proposed_sequencer: Option<Address>,
    pub proposed_sequencer_txid: Option<Txid>,
    pub sequencer_votes: HashMap<Address, Vote>,
    pub bridge_address: Option<Address>,
    pub starting_block_number: u32,
    pub bootloader_hash: Option<H256>,
    pub abstract_account_hash: Option<H256>,
    pub proposed_governance: Option<Address>,
}

impl BootstrapState {
    pub fn new() -> Self {
        Self {
            verifier_addresses: Vec::new(),
            proposed_sequencer: None,
            proposed_sequencer_txid: None,
            sequencer_votes: HashMap::new(),
            bridge_address: None,
            starting_block_number: 0,
            bootloader_hash: None,
            abstract_account_hash: None,
            proposed_governance: None,
        }
    }

    pub fn is_complete(&self) -> bool {
        !self.verifier_addresses.is_empty()
            && self.proposed_sequencer.is_some()
            && self.bridge_address.is_some()
            && self.starting_block_number > 0
            && self.has_majority_votes()
            && self.bootloader_hash.is_some()
            && self.abstract_account_hash.is_some()
            && self.proposed_governance.is_some()
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
#[derive(Debug, Clone)]
pub struct BitcoinInscriptionIndexer {
    client: Arc<dyn BitcoinOps>,
    parser: MessageParser,
    bridge_address: Address,
    sequencer_address: Address,
    governance_address: Address,
    verifier_addresses: Vec<Address>,
    starting_block_number: u32,
}

impl BitcoinInscriptionIndexer {
    #[instrument(skip(client, config, bootstrap_txids), target = "bitcoin_indexer")]
    pub async fn new(
        client: Arc<BitcoinClient>,
        config: ViaBtcClientConfig,
        bootstrap_txids: Vec<Txid>,
    ) -> BitcoinIndexerResult<Self>
    where
        Self: Sized,
    {
        info!("Creating new BitcoinInscriptionIndexer");
        let mut parser = MessageParser::new(config.network());
        let mut bootstrap_state = BootstrapState::new();

        for txid in bootstrap_txids {
            debug!("Processing bootstrap transaction: {}", txid);
            let tx = client.get_transaction(&txid).await?;
            let messages = parser.parse_system_transaction(&tx, 0);

            for message in messages {
                Self::process_bootstrap_message(
                    &mut bootstrap_state,
                    message,
                    txid,
                    config.network(),
                );
            }

            if bootstrap_state.is_complete() {
                info!("Bootstrap process completed");
                break;
            }
        }

        Self::create_indexer(bootstrap_state, client, parser)
    }

    #[instrument(skip(self), target = "bitcoin_indexer")]
    pub async fn process_blocks(
        &mut self,
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
    pub async fn process_block(
        &mut self,
        block_height: u32,
    ) -> BitcoinIndexerResult<Vec<FullInscriptionMessage>> {
        debug!("Processing block at height {}", block_height);
        if block_height < self.starting_block_number {
            error!("Attempted to process block before starting block");
            return Err(types::IndexerError::InvalidBlockHeight(block_height));
        }

        let block = self.client.fetch_block(block_height as u128).await?;
        // TODO: check block header is belong to a valid chain of blocks (reorg detection and management)
        // TODO: deal with malicious sequencer, verifiers from being able to make trouble by sending invalid messages / valid messages with invalid data

        let mut valid_messages = Vec::new();

        let (gov_txs, system_tx, bridge_tx) = self.extract_important_transactions(&block.txdata);

        // Parse protocol upgrade messages
        if let Some(gov_txs) = gov_txs {
            let parsed_messages: Vec<_> = gov_txs
                .iter()
                .flat_map(|tx| {
                    self.parser
                        .parse_protocol_upgrade_transaction(tx, block_height)
                })
                .collect();

            let mut messages = vec![];
            for message in parsed_messages {
                if self.is_valid_gov_message(&message).await {
                    messages.push(message);
                }
            }

            valid_messages.extend(messages);
        }

        if let Some(system_tx) = system_tx {
            let parsed_messages: Vec<_> = system_tx
                .iter()
                .flat_map(|tx| self.parser.parse_system_transaction(&tx.tx, block_height))
                .collect();

            let messages: Vec<_> = parsed_messages
                .into_iter()
                .filter(|message| self.is_valid_system_message(message))
                .collect();

            valid_messages.extend(messages);
        }

        if let Some(mut bridge_tx) = bridge_tx {
            let parsed_messages: Vec<_> = bridge_tx
                .iter_mut()
                .flat_map(|tx| self.parser.parse_bridge_transaction(tx, block_height))
                .collect();

            let mut messages = vec![];
            for message in parsed_messages {
                if self.is_valid_bridge_message(&message).await {
                    messages.push(message);
                }
            }

            valid_messages.extend(messages);
        }

        debug!(
            "Processed {} valid messages in block {}",
            valid_messages.len(),
            block_height
        );
        Ok(valid_messages)
    }

    fn extract_important_transactions(
        &self,
        transactions: &[BitcoinTransaction],
    ) -> (
        Option<Vec<TransactionWithMetadata>>,
        Option<Vec<TransactionWithMetadata>>,
        Option<Vec<TransactionWithMetadata>>,
    ) {
        // We only care about the transactions that sequencer, verifiers are sending and the bridge is receiving
        let system_txs: Vec<TransactionWithMetadata> = transactions
            .iter()
            .enumerate()
            .filter_map(|(tx_index, tx)| {
                let is_valid = tx.input.iter().any(|input| {
                    if let Some(btc_address) = self.parser.parse_p2wpkh(&input.witness) {
                        btc_address == self.sequencer_address
                            || self.verifier_addresses.contains(&btc_address)
                    } else {
                        false
                    }
                });

                if is_valid {
                    Some(TransactionWithMetadata::new(tx.clone(), tx_index))
                } else {
                    None
                }
            })
            .collect();

        let bridge_txs: Vec<TransactionWithMetadata> = transactions
            .iter()
            .enumerate()
            .filter_map(|(tx_index, tx)| {
                let is_bridge_output = tx
                    .output
                    .iter()
                    .any(|output| output.script_pubkey == self.bridge_address.script_pubkey());

                if is_bridge_output {
                    Some(TransactionWithMetadata::new(tx.clone(), tx_index))
                } else {
                    None
                }
            })
            .collect();

        let gov_txs: Vec<TransactionWithMetadata> = transactions
            .iter()
            .enumerate()
            .filter_map(|(tx_index, tx)| {
                let is_bridge_output = tx
                    .output
                    .iter()
                    .any(|output| output.script_pubkey == self.governance_address.script_pubkey());

                if is_bridge_output {
                    Some(TransactionWithMetadata::new(tx.clone(), tx_index))
                } else {
                    None
                }
            })
            .collect();

        let gov_txs = if !gov_txs.is_empty() {
            Some(gov_txs)
        } else {
            None
        };

        let system_txs = if !system_txs.is_empty() {
            Some(system_txs)
        } else {
            None
        };

        let bridge_txs = if !bridge_txs.is_empty() {
            Some(bridge_txs)
        } else {
            None
        };

        (gov_txs, system_txs, bridge_txs)
    }

    #[instrument(skip(self), target = "bitcoin_indexer")]
    pub async fn are_blocks_connected(
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

    pub async fn fetch_block_height(&self) -> BitcoinIndexerResult<u64> {
        self.client.fetch_block_height().await.map_err(|e| e.into())
    }

    pub fn get_state(&self) -> (Address, Address, Vec<Address>, u32) {
        (
            self.bridge_address.clone(),
            self.sequencer_address.clone(),
            self.verifier_addresses.clone(),
            self.starting_block_number,
        )
    }

    pub async fn get_l1_batch_number(
        &mut self,
        msg: &FullInscriptionMessage,
    ) -> Option<L1BatchNumber> {
        match msg {
            FullInscriptionMessage::ProofDAReference(proof_msg) => self
                .get_l1_batch_number_from_proof_tx_id(&proof_msg.input.l1_batch_reveal_txid)
                .await
                .ok(),
            FullInscriptionMessage::ValidatorAttestation(va_msg) => self
                .get_l1_batch_number_from_validation_tx_id(&va_msg.input.reference_txid)
                .await
                .ok(),
            _ => None,
        }
    }

    pub fn get_number_of_verifiers(&self) -> usize {
        self.verifier_addresses.len()
    }

    pub async fn parse_transaction(
        &mut self,
        tx: &Txid,
    ) -> BitcoinIndexerResult<Vec<FullInscriptionMessage>> {
        let tx = self.client.get_transaction(tx).await?;
        Ok(self.parser.parse_system_transaction(&tx, 0))
    }
}

impl BitcoinInscriptionIndexer {
    pub fn create_indexer(
        bootstrap_state: BootstrapState,
        client: Arc<dyn BitcoinOps>,
        parser: MessageParser,
    ) -> BitcoinIndexerResult<Self> {
        if bootstrap_state.is_complete() {
            if let (Some(bridge), Some(sequencer), Some(governance)) = (
                bootstrap_state.bridge_address.clone(),
                bootstrap_state.proposed_sequencer.clone(),
                bootstrap_state.proposed_governance.clone(),
            ) {
                info!("BitcoinInscriptionIndexer successfully created");
                Ok(Self {
                    client,
                    parser,
                    bridge_address: bridge,
                    sequencer_address: sequencer,
                    governance_address: governance,
                    verifier_addresses: bootstrap_state.verifier_addresses,
                    starting_block_number: bootstrap_state.starting_block_number,
                })
            } else {
                error!("Incomplete bootstrap process despite state being marked as complete");
                error!("state: {:?}", bootstrap_state);
                Err(types::IndexerError::IncompleteBootstrap(
                    "Incomplete bootstrap process despite state being marked as complete"
                        .to_string(),
                ))
            }
        } else {
            error!("Bootstrap process did not complete with provided transactions");
            error!("state: {:?}", bootstrap_state);
            Err(types::IndexerError::IncompleteBootstrap(
                "Bootstrap process did not complete with provided transactions".to_string(),
            ))
        }
    }

    #[instrument(skip(self, message), target = "bitcoin_indexer")]
    fn is_valid_system_message(&self, message: &FullInscriptionMessage) -> bool {
        match message {
            FullInscriptionMessage::ProposeSequencer(m) => m
                .common
                .p2wpkh_address
                .as_ref()
                .map_or(false, |addr| self.verifier_addresses.contains(addr)),
            FullInscriptionMessage::ValidatorAttestation(m) => m
                .common
                .p2wpkh_address
                .as_ref()
                .map_or(false, |addr| self.verifier_addresses.contains(addr)),
            FullInscriptionMessage::L1BatchDAReference(m) => m
                .common
                .p2wpkh_address
                .as_ref()
                .map_or(false, |addr| addr == &self.sequencer_address),
            FullInscriptionMessage::ProofDAReference(m) => m
                .common
                .p2wpkh_address
                .as_ref()
                .map_or(false, |addr| addr == &self.sequencer_address),
            FullInscriptionMessage::SystemBootstrapping(_) => {
                debug!("SystemBootstrapping message is always valid");
                true
            }
            _ => false,
        }
    }

    async fn is_valid_bridge_message(&self, message: &FullInscriptionMessage) -> bool {
        match message {
            FullInscriptionMessage::L1ToL2Message(m) => self.is_valid_l1_to_l2_transfer(m),
            FullInscriptionMessage::BridgeWithdrawal(m) => {
                self.is_valid_bridge_withdrawal(m).await.unwrap_or(false)
            }
            _ => false,
        }
    }

    async fn is_valid_gov_message(&self, message: &FullInscriptionMessage) -> bool {
        match message {
            FullInscriptionMessage::SystemContractUpgrade(m) => {
                self.is_valid_gov_upgrade(m).await.unwrap_or(false)
            }
            _ => false,
        }
    }

    #[instrument(skip(state, message), target = "bitcoin_indexer")]
    fn process_bootstrap_message(
        state: &mut BootstrapState,
        message: FullInscriptionMessage,
        txid: Txid,
        network: Network,
    ) {
        match message {
            FullInscriptionMessage::SystemBootstrapping(sb) => {
                debug!("Processing SystemBootstrapping message");
                // convert the verifier addresses to the correct network
                // since the bootstrap message should run on the bootstrapping phase of sequencer
                // i consume it's ok to using unwrap
                let verifier_addresses = sb
                    .input
                    .verifier_p2wpkh_addresses
                    .iter()
                    .map(|addr| addr.clone().require_network(network).unwrap())
                    .collect();

                state.verifier_addresses = verifier_addresses;

                let bridge_address = sb
                    .input
                    .bridge_musig2_address
                    .require_network(network)
                    .unwrap();

                let governance_address = sb
                    .input
                    .governance_address
                    .require_network(network)
                    .unwrap();

                state.bridge_address = Some(bridge_address);
                state.starting_block_number = sb.input.start_block_height;
                state.bootloader_hash = Some(sb.input.bootloader_hash);
                state.abstract_account_hash = Some(sb.input.abstract_account_hash);
                state.proposed_governance = Some(governance_address);
            }
            FullInscriptionMessage::ProposeSequencer(ps) => {
                debug!("Processing ProposeSequencer message");
                let sequencer_address = ps
                    .input
                    .sequencer_new_p2wpkh_address
                    .require_network(network)
                    .unwrap();
                state.proposed_sequencer = Some(sequencer_address);
                state.proposed_sequencer_txid = Some(txid);
            }
            FullInscriptionMessage::ValidatorAttestation(va) => {
                let p2wpkh_address = va
                    .common
                    .p2wpkh_address
                    .as_ref()
                    .expect("ValidatorAttestation message must have a p2wpkh address");
                if state.verifier_addresses.contains(p2wpkh_address)
                    && state.proposed_sequencer.is_some()
                {
                    if let Some(proposed_txid) = state.proposed_sequencer_txid {
                        if va.input.reference_txid == proposed_txid {
                            state
                                .sequencer_votes
                                .insert(p2wpkh_address.clone(), va.input.attestation);
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
        let is_valid_receiver = message
            .tx_outputs
            .iter()
            .any(|output| output.script_pubkey == self.bridge_address.script_pubkey());
        debug!("L1ToL2Message transfer validity: {}", is_valid_receiver);

        let total_bridge_amount = message
            .tx_outputs
            .iter()
            .filter(|output| output.script_pubkey == self.bridge_address.script_pubkey())
            .map(|output| output.value)
            .sum::<Amount>();

        let is_valid_amount = message.amount == total_bridge_amount;
        debug!(
            "Amount validation: message amount = {}, total bridge outputs = {}",
            message.amount, total_bridge_amount
        );

        is_valid_receiver && is_valid_amount
    }

    #[instrument(skip(self, message), target = "bitcoin_indexer")]
    async fn is_valid_bridge_withdrawal(&self, message: &BridgeWithdrawal) -> anyhow::Result<bool> {
        if let Some(outpoint) = message.input.inputs.first() {
            let tx = self.client.get_transaction(&outpoint.txid).await?;
            if let Some(txout) = tx.output.get(outpoint.vout as usize) {
                return Ok(txout.script_pubkey == self.bridge_address.script_pubkey());
            }
        }
        Ok(false)
    }

    #[instrument(skip(self, message), target = "bitcoin_indexer")]
    async fn is_valid_gov_upgrade(&self, message: &SystemContractUpgrade) -> anyhow::Result<bool> {
        if let Some(outpoint) = message.input.inputs.first() {
            let tx = self.client.get_transaction(&outpoint.txid).await?;
            if let Some(txout) = tx.output.get(outpoint.vout as usize) {
                return Ok(txout.script_pubkey == self.governance_address.script_pubkey());
            }
        }
        Ok(false)
    }

    async fn get_l1_batch_number_from_proof_tx_id(
        &mut self,
        txid: &Txid,
    ) -> anyhow::Result<L1BatchNumber> {
        let a = self.client.get_transaction(txid).await?;
        let b = self.parser.parse_system_transaction(&a, 0);
        let msg = b
            .first()
            .ok_or_else(|| anyhow::anyhow!("No message found"))?;

        match msg {
            FullInscriptionMessage::L1BatchDAReference(da_msg) => Ok(da_msg.input.l1_batch_index),
            _ => Err(anyhow::anyhow!("Invalid message type")),
        }
    }

    async fn get_l1_batch_number_from_validation_tx_id(
        &mut self,
        txid: &Txid,
    ) -> anyhow::Result<L1BatchNumber> {
        let a = self.client.get_transaction(txid).await?;
        let b = self.parser.parse_system_transaction(&a, 0);
        let msg = b
            .first()
            .ok_or_else(|| anyhow::anyhow!("No message found"))?;

        match msg {
            FullInscriptionMessage::ProofDAReference(da_msg) => Ok(self
                .get_l1_batch_number_from_proof_tx_id(&da_msg.input.l1_batch_reveal_txid)
                .await?),
            _ => Err(anyhow::anyhow!("Invalid message type")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use async_trait::async_trait;
    use bitcoin::{
        block::Header, hashes::Hash, Amount, Block, OutPoint, ScriptBuf, Transaction, TxMerkleNode,
        TxOut,
    };
    use bitcoincore_rpc::json::GetBlockStatsResult;
    use mockall::{mock, predicate::*};

    use super::*;
    use crate::types::{BitcoinClientResult, CommonFields};

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
            async fn fetch_block_height(&self) -> BitcoinClientResult<u64>;
            async fn get_fee_rate(&self, conf_target: u16) -> BitcoinClientResult<u64>;
            fn get_network(&self) -> Network;
            async fn get_block_stats(&self, height: u64) -> BitcoinClientResult<GetBlockStatsResult>;
            async fn get_fee_history(
                &self,
                from_block_height: usize,
                to_block_height: usize,
            ) -> BitcoinClientResult<Vec<u64>>;
        }
    }

    fn get_test_addr() -> Address {
        Address::from_str("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx")
            .unwrap()
            .require_network(Network::Testnet)
            .unwrap()
    }

    fn get_test_common_fields() -> CommonFields {
        CommonFields {
            schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
            encoded_public_key: bitcoin::script::PushBytesBuf::from([0u8; 32]),
            block_height: 0,
            tx_id: Txid::all_zeros(),
            p2wpkh_address: Some(get_test_addr()),
            tx_index: None,
            output_vout: None,
        }
    }

    fn get_indexer_with_mock(mock_client: MockBitcoinOps) -> BitcoinInscriptionIndexer {
        let parser = MessageParser::new(Network::Testnet);
        let bridge_address = get_test_addr();
        let sequencer_address = get_test_addr();
        let governance_address = get_test_addr();

        BitcoinInscriptionIndexer {
            client: Arc::new(mock_client),
            parser,
            bridge_address,
            sequencer_address,
            governance_address,
            verifier_addresses: vec![],
            starting_block_number: 0,
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

        let mut indexer = get_indexer_with_mock(mock_client);
        let result = indexer.process_blocks(start_block, end_block).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_is_valid_message() {
        let indexer = get_indexer_with_mock(MockBitcoinOps::new());

        let propose_sequencer = FullInscriptionMessage::ProposeSequencer(types::ProposeSequencer {
            common: get_test_common_fields(),
            input: types::ProposeSequencerInput {
                sequencer_new_p2wpkh_address: indexer
                    .sequencer_address
                    .clone()
                    .as_unchecked()
                    .to_owned(),
            },
        });
        assert!(!indexer.is_valid_system_message(&propose_sequencer));

        let validator_attestation =
            FullInscriptionMessage::ValidatorAttestation(types::ValidatorAttestation {
                common: get_test_common_fields(),
                input: types::ValidatorAttestationInput {
                    reference_txid: Txid::all_zeros(),
                    attestation: Vote::Ok,
                },
            });
        assert!(!indexer.is_valid_system_message(&validator_attestation));

        let l1_batch_da_reference =
            FullInscriptionMessage::L1BatchDAReference(types::L1BatchDAReference {
                common: CommonFields {
                    schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
                    encoded_public_key: bitcoin::script::PushBytesBuf::from([0u8; 32]),
                    block_height: 0,
                    tx_id: Txid::all_zeros(),
                    p2wpkh_address: Some(get_test_addr()),
                    tx_index: None,
                    output_vout: None,
                },
                input: types::L1BatchDAReferenceInput {
                    l1_batch_hash: zksync_basic_types::H256::zero(),
                    l1_batch_index: zksync_types::L1BatchNumber(0),
                    da_identifier: "test".to_string(),
                    blob_id: "test".to_string(),
                    prev_l1_batch_hash: zksync_basic_types::H256::zero(),
                },
            });
        // We didn't vote for the sequencer yet, so this message is invalid
        assert!(indexer.is_valid_system_message(&l1_batch_da_reference));

        let l1_to_l2_message = FullInscriptionMessage::L1ToL2Message(L1ToL2Message {
            common: get_test_common_fields(),
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
        assert!(indexer.is_valid_bridge_message(&l1_to_l2_message).await);

        let system_bootstrapping =
            FullInscriptionMessage::SystemBootstrapping(types::SystemBootstrapping {
                common: get_test_common_fields(),
                input: types::SystemBootstrappingInput {
                    start_block_height: 0,
                    bridge_musig2_address: indexer.bridge_address.clone().as_unchecked().to_owned(),
                    verifier_p2wpkh_addresses: vec![],
                    bootloader_hash: H256::zero(),
                    abstract_account_hash: H256::zero(),
                    governance_address: indexer
                        .governance_address
                        .clone()
                        .as_unchecked()
                        .to_owned(),
                },
            });
        assert!(indexer.is_valid_system_message(&system_bootstrapping));
    }

    #[tokio::test]
    async fn test_is_valid_l1_to_l2_transfer() {
        let indexer = get_indexer_with_mock(MockBitcoinOps::new());

        let valid_message = L1ToL2Message {
            common: get_test_common_fields(),
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
            common: get_test_common_fields(),
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
