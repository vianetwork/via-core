use std::sync::Arc;

use async_trait::async_trait;
use bitcoin::{Address, Amount, BlockHash, OutPoint, Transaction as BitcoinTransaction, Txid};
use tracing::{debug, info, instrument, warn};

mod parser;
pub use parser::{get_eth_address, MessageParser};
use zksync_basic_types::L1BatchNumber;
use zksync_types::via_wallet::SystemWallets;

use crate::{
    client::BitcoinClient,
    traits::BitcoinOps,
    types::{
        BitcoinIndexerResult, BitcoinTxLocator, BridgeWithdrawal, FullInscriptionMessage,
        L1ToL2Message, SystemTransactions, TransactionWithMetadata,
    },
};

pub mod withdrawal;

#[async_trait]
pub trait BitcoinTxLocatorStore: Send {
    async fn insert_tx_locator(&mut self, locator: &BitcoinTxLocator) -> anyhow::Result<()>;
    async fn get_tx_locator(&mut self, txid: &Txid) -> anyhow::Result<Option<BitcoinTxLocator>>;
}

/// The main indexer struct for processing Bitcoin inscriptions
#[derive(Debug, Clone)]
pub struct BitcoinInscriptionIndexer {
    client: Arc<dyn BitcoinOps>,
    wallets: Arc<SystemWallets>,
    parser: MessageParser,
}

impl BitcoinInscriptionIndexer {
    #[instrument(
        skip(client, wallets)
        target = "bitcoin_indexer"
    )]
    pub fn new(client: Arc<BitcoinClient>, wallets: Arc<SystemWallets>) -> Self {
        Self {
            client: client.clone(),
            parser: MessageParser::new(client.get_network()),
            wallets,
        }
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

    #[instrument(skip(self, locator_store), target = "bitcoin_indexer")]
    pub async fn process_blocks_with_locator_store(
        &mut self,
        starting_block: u32,
        ending_block: u32,
        locator_store: &mut dyn BitcoinTxLocatorStore,
    ) -> BitcoinIndexerResult<Vec<FullInscriptionMessage>> {
        info!(
            "Processing blocks from {} to {}",
            starting_block, ending_block
        );
        let mut res = Vec::with_capacity((ending_block - starting_block + 1) as usize);
        for block in starting_block..=ending_block {
            res.extend(
                self.process_block_inner(block, Some(&mut *locator_store))
                    .await?,
            );
        }
        debug!("Processed {} blocks", ending_block - starting_block + 1);
        Ok(res)
    }

    #[instrument(skip(self, locator_store), target = "bitcoin_indexer")]
    pub async fn backfill_transaction_locators(
        &mut self,
        starting_block: u32,
        ending_block: u32,
        locator_store: &mut dyn BitcoinTxLocatorStore,
    ) -> BitcoinIndexerResult<()> {
        if starting_block > ending_block {
            return Ok(());
        }

        info!(
            "Backfilling Bitcoin transaction locators from {} to {}",
            starting_block, ending_block
        );
        for block_height in starting_block..=ending_block {
            let block = self.client.fetch_block(block_height as u128).await?;
            let block_hash = block.block_hash();
            for (tx_index, tx) in block.txdata.iter().enumerate() {
                locator_store
                    .insert_tx_locator(&BitcoinTxLocator {
                        txid: tx.compute_txid(),
                        block_height,
                        block_hash,
                        tx_index: Some(tx_index),
                    })
                    .await
                    .map_err(|err| crate::types::IndexerError::Internal(err.to_string()))?;
            }
        }
        Ok(())
    }

    #[instrument(skip(self), target = "bitcoin_indexer")]
    pub fn update_system_wallets(
        &mut self,
        sequencer_opt: Option<Address>,
        bridge_opt: Option<Address>,
        verifiers_opt: Option<Vec<Address>>,
        governance_opt: Option<Address>,
    ) {
        let mut new_wallets = SystemWallets {
            ..(*self.wallets).clone()
        };

        if let Some(sequencer) = sequencer_opt {
            new_wallets.sequencer = sequencer;
        }

        if let Some(bridge) = bridge_opt {
            new_wallets.bridge = bridge;
        }

        if let Some(verifiers) = verifiers_opt {
            new_wallets.verifiers = verifiers;
        }

        if let Some(governance) = governance_opt {
            new_wallets.governance = governance;
        }

        self.wallets = Arc::new(new_wallets);
    }

    #[instrument(skip(self), target = "bitcoin_indexer")]
    pub async fn process_block(
        &mut self,
        block_height: u32,
    ) -> BitcoinIndexerResult<Vec<FullInscriptionMessage>> {
        self.process_block_inner(block_height, None).await
    }

    async fn process_block_inner(
        &mut self,
        block_height: u32,
        mut locator_store: Option<&mut dyn BitcoinTxLocatorStore>,
    ) -> BitcoinIndexerResult<Vec<FullInscriptionMessage>> {
        debug!("Processing block at height {}", block_height);

        let block = self.client.fetch_block(block_height as u128).await?;
        let block_hash = block.block_hash();
        // TODO: check block header is belong to a valid chain of blocks (reorg detection and management)
        // TODO: deal with malicious sequencer, verifiers from being able to make trouble by sending invalid messages / valid messages with invalid data

        let mut valid_messages = Vec::new();

        let mut system_txs = self.extract_important_transactions(&block.txdata);
        if let Some(locator_store) = locator_store.as_deref_mut() {
            Self::store_transaction_locators(locator_store, block_height, block_hash, &system_txs)
                .await
                .map_err(|err| crate::types::IndexerError::Internal(err.to_string()))?;
        }

        // Parse protocol upgrade messages (Upgrade system contracts, bridge addresses, sequencer address)
        if !system_txs.governance_txs.is_empty() {
            let parsed_messages: Vec<_> = system_txs
                .governance_txs
                .iter()
                .flat_map(|tx| {
                    self.parser
                        .parse_protocol_upgrade_transactions_with_block_hash(
                            tx,
                            block_height,
                            Some(block_hash),
                        )
                })
                .collect();

            let mut messages = vec![];
            for message in parsed_messages {
                let is_valid = if let Some(locator_store) = locator_store.as_deref_mut() {
                    self.is_valid_gov_message_with_locator_store(&message, locator_store)
                        .await
                } else {
                    self.is_valid_gov_message(&message).await
                };

                if is_valid {
                    messages.push(message);
                }
            }

            valid_messages.extend(messages);
        }

        if !system_txs.system_txs.is_empty() {
            let parsed_messages: Vec<_> = system_txs
                .system_txs
                .iter()
                .flat_map(|tx| {
                    self.parser.parse_system_transaction_with_block_hash(
                        &tx.tx,
                        block_height,
                        Some(block_hash),
                        Some(&self.wallets),
                    )
                })
                .collect();

            let messages: Vec<_> = parsed_messages
                .into_iter()
                .filter(|message| self.is_valid_system_message(message))
                .collect();

            valid_messages.extend(messages);
        }

        if !system_txs.bridge_txs.is_empty() {
            let parsed_messages: Vec<_> = system_txs
                .bridge_txs
                .iter_mut()
                .flat_map(|tx| {
                    self.parser.parse_bridge_transaction_with_block_hash(
                        tx,
                        block_height,
                        Some(block_hash),
                        &self.wallets,
                    )
                })
                .collect();

            let mut messages = vec![];
            for message in parsed_messages {
                let is_valid = if let Some(locator_store) = locator_store.as_deref_mut() {
                    self.is_valid_bridge_message_with_locator_store(&message, locator_store)
                        .await
                } else {
                    self.is_valid_bridge_message(&message).await
                };

                if is_valid {
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

    async fn store_transaction_locators(
        locator_store: &mut dyn BitcoinTxLocatorStore,
        block_height: u32,
        block_hash: BlockHash,
        system_txs: &SystemTransactions,
    ) -> anyhow::Result<()> {
        for tx in system_txs
            .system_txs
            .iter()
            .chain(system_txs.bridge_txs.iter())
            .chain(system_txs.governance_txs.iter())
        {
            locator_store
                .insert_tx_locator(&BitcoinTxLocator {
                    txid: tx.tx.compute_txid(),
                    block_height,
                    block_hash,
                    tx_index: Some(tx.tx_index),
                })
                .await?;
        }

        Ok(())
    }

    fn extract_important_transactions(
        &self,
        transactions: &[BitcoinTransaction],
    ) -> SystemTransactions {
        // We only care about the transactions that sequencer, verifiers are sending and the bridge is receiving
        let system_txs: Vec<TransactionWithMetadata> = transactions
            .iter()
            .enumerate()
            .filter_map(|(tx_index, tx)| {
                let is_valid = tx.input.iter().any(|input| {
                    if let Some(btc_address) = self.parser.parse_p2wpkh(&input.witness) {
                        btc_address == self.wallets.sequencer
                            || self.wallets.verifiers.contains(&btc_address)
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
                    .any(|output| output.script_pubkey == self.wallets.bridge.script_pubkey());

                if is_bridge_output {
                    Some(TransactionWithMetadata::new(tx.clone(), tx_index))
                } else {
                    None
                }
            })
            .collect();

        let governance_txs: Vec<TransactionWithMetadata> = transactions
            .iter()
            .enumerate()
            .filter_map(|(tx_index, tx)| {
                let is_bridge_output = tx
                    .output
                    .iter()
                    .any(|output| output.script_pubkey == self.wallets.governance.script_pubkey());

                if is_bridge_output {
                    Some(TransactionWithMetadata::new(tx.clone(), tx_index))
                } else {
                    None
                }
            })
            .collect();

        SystemTransactions {
            system_txs,
            bridge_txs,
            governance_txs,
        }
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

    pub fn get_state(&self) -> Arc<SystemWallets> {
        self.wallets.clone()
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
        self.wallets.verifiers.len()
    }

    pub async fn parse_transaction(
        &mut self,
        tx: &Txid,
    ) -> BitcoinIndexerResult<Vec<FullInscriptionMessage>> {
        let tx = self.client.get_transaction(tx).await?;
        Ok(self
            .parser
            .parse_system_transaction(&tx, 0, Some(&self.wallets)))
    }

    pub async fn parse_transaction_with_locator(
        &mut self,
        locator: &BitcoinTxLocator,
    ) -> BitcoinIndexerResult<Vec<FullInscriptionMessage>> {
        let tx = self
            .client
            .get_transaction_in_block(&locator.txid, &locator.block_hash)
            .await?;
        Ok(self.parser.parse_system_transaction_with_block_hash(
            &tx,
            locator.block_height,
            Some(locator.block_hash),
            Some(&self.wallets),
        ))
    }
}

impl BitcoinInscriptionIndexer {
    #[instrument(skip(self, message), target = "bitcoin_indexer")]
    fn is_valid_system_message(&self, message: &FullInscriptionMessage) -> bool {
        match message {
            FullInscriptionMessage::ValidatorAttestation(m) => m
                .common
                .p2wpkh_address
                .as_ref()
                .map_or(false, |addr| self.wallets.verifiers.contains(addr)),
            FullInscriptionMessage::L1BatchDAReference(m) => m
                .common
                .p2wpkh_address
                .as_ref()
                .map_or(false, |addr| addr == &self.wallets.sequencer),
            FullInscriptionMessage::ProofDAReference(m) => m
                .common
                .p2wpkh_address
                .as_ref()
                .map_or(false, |addr| addr == &self.wallets.sequencer),
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

    async fn is_valid_bridge_message_with_locator_store(
        &self,
        message: &FullInscriptionMessage,
        locator_store: &mut dyn BitcoinTxLocatorStore,
    ) -> bool {
        match message {
            FullInscriptionMessage::L1ToL2Message(m) => self.is_valid_l1_to_l2_transfer(m),
            FullInscriptionMessage::BridgeWithdrawal(m) => self
                .is_valid_bridge_withdrawal_with_locator_store(m, locator_store)
                .await
                .unwrap_or(false),
            _ => false,
        }
    }

    async fn is_valid_gov_message(&self, message: &FullInscriptionMessage) -> bool {
        let maybe_input = match message {
            FullInscriptionMessage::SystemContractUpgrade(m) => m.input.inputs.first(),
            FullInscriptionMessage::UpdateBridge(m) => m.input.inputs.first(),
            FullInscriptionMessage::UpdateSequencer(m) => m.input.inputs.first(),
            FullInscriptionMessage::UpdateGovernance(m) => m.input.inputs.first(),
            _ => return false,
        };

        self.is_valid_gov_upgrade(maybe_input)
            .await
            .unwrap_or(false)
    }

    async fn is_valid_gov_message_with_locator_store(
        &self,
        message: &FullInscriptionMessage,
        locator_store: &mut dyn BitcoinTxLocatorStore,
    ) -> bool {
        let maybe_input = match message {
            FullInscriptionMessage::SystemContractUpgrade(m) => m.input.inputs.first(),
            FullInscriptionMessage::UpdateBridge(m) => m.input.inputs.first(),
            FullInscriptionMessage::UpdateSequencer(m) => m.input.inputs.first(),
            FullInscriptionMessage::UpdateGovernance(m) => m.input.inputs.first(),
            _ => return false,
        };

        self.is_valid_gov_upgrade_with_locator_store(maybe_input, locator_store)
            .await
            .unwrap_or(false)
    }

    #[instrument(skip(self, message), target = "bitcoin_indexer")]
    fn is_valid_l1_to_l2_transfer(&self, message: &L1ToL2Message) -> bool {
        let is_valid_receiver = message
            .tx_outputs
            .iter()
            .any(|output| output.script_pubkey == self.wallets.bridge.script_pubkey());
        debug!("L1ToL2Message transfer validity: {}", is_valid_receiver);

        let total_bridge_amount = message
            .tx_outputs
            .iter()
            .filter(|output| output.script_pubkey == self.wallets.bridge.script_pubkey())
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
                return Ok(txout.script_pubkey == self.wallets.bridge.script_pubkey());
            }
        }
        Ok(false)
    }

    async fn is_valid_bridge_withdrawal_with_locator_store(
        &self,
        message: &BridgeWithdrawal,
        locator_store: &mut dyn BitcoinTxLocatorStore,
    ) -> anyhow::Result<bool> {
        if let Some(outpoint) = message.input.inputs.first() {
            let tx = self
                .get_transaction_for_validation_with_locator_store(&outpoint.txid, locator_store)
                .await?;
            if let Some(txout) = tx.output.get(outpoint.vout as usize) {
                return Ok(txout.script_pubkey == self.wallets.bridge.script_pubkey());
            }
        }
        Ok(false)
    }

    #[instrument(skip(self, outpoint_opt), target = "bitcoin_indexer")]
    async fn is_valid_gov_upgrade(&self, outpoint_opt: Option<&OutPoint>) -> anyhow::Result<bool> {
        if let Some(outpoint) = outpoint_opt {
            let tx = self.client.get_transaction(&outpoint.txid).await?;
            if let Some(txout) = tx.output.get(outpoint.vout as usize) {
                return Ok(txout.script_pubkey == self.wallets.governance.script_pubkey());
            }
        }
        Ok(false)
    }

    async fn is_valid_gov_upgrade_with_locator_store(
        &self,
        outpoint_opt: Option<&OutPoint>,
        locator_store: &mut dyn BitcoinTxLocatorStore,
    ) -> anyhow::Result<bool> {
        if let Some(outpoint) = outpoint_opt {
            let tx = self
                .get_transaction_for_validation_with_locator_store(&outpoint.txid, locator_store)
                .await?;
            if let Some(txout) = tx.output.get(outpoint.vout as usize) {
                return Ok(txout.script_pubkey == self.wallets.governance.script_pubkey());
            }
        }
        Ok(false)
    }

    async fn get_transaction_for_validation_with_locator_store(
        &self,
        txid: &Txid,
        locator_store: &mut dyn BitcoinTxLocatorStore,
    ) -> anyhow::Result<BitcoinTransaction> {
        let locator = locator_store
            .get_tx_locator(txid)
            .await?
            .ok_or_else(|| anyhow::anyhow!("missing Bitcoin block locator for txid {}", txid))?;

        Ok(self
            .client
            .get_transaction_in_block(txid, &locator.block_hash)
            .await?)
    }

    async fn get_l1_batch_number_from_proof_tx_id(
        &mut self,
        txid: &Txid,
    ) -> anyhow::Result<L1BatchNumber> {
        let a = self.client.get_transaction(txid).await?;
        let b = self
            .parser
            .parse_system_transaction(&a, 0, Some(&self.wallets));
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
        let b = self
            .parser
            .parse_system_transaction(&a, 0, Some(&self.wallets));
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
        absolute, block::Header, hashes::Hash, transaction, Address, Amount, Block, Network,
        OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxMerkleNode, TxOut, Witness,
    };
    use bitcoincore_rpc::json::GetBlockStatsResult;
    use mockall::{mock, predicate::*};
    use zksync_types::{
        protocol_version::{ProtocolSemanticVersion, VersionPatch},
        ProtocolVersionId, H256,
    };

    use super::*;
    use crate::types::{self, BitcoinClientResult, CommonFields, Vote};

    mock! {
        BitcoinOps {}
        #[async_trait]
        impl BitcoinOps for BitcoinOps {
            async fn get_transaction(&self, txid: &Txid) -> BitcoinClientResult<Transaction>;
            async fn get_transaction_in_block(&self, txid: &Txid, block_hash: &BlockHash) -> BitcoinClientResult<Transaction>;
            async fn fetch_block(&self, block_height: u128) -> BitcoinClientResult<Block>;
            async fn fetch_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinClientResult<Block>;
            async fn find_transaction_locator(&self, txid: &Txid, from_block_height: u64, to_block_height: u64) -> BitcoinClientResult<Option<BitcoinTxLocator>>;
            async fn get_balance(&self, address: &Address) -> BitcoinClientResult<u128>;
            async fn broadcast_signed_transaction(&self, signed_transaction: &str) -> BitcoinClientResult<Txid>;
            async fn fetch_utxos(&self, address: &Address) -> BitcoinClientResult<Vec<(OutPoint, TxOut)>>;
            async fn check_tx_confirmation(&self, txid: &Txid, conf_num: u32) -> BitcoinClientResult<bool>;
            async fn check_tx_confirmation_in_block(&self, txid: &Txid, block_hash: &BlockHash, conf_num: u32) -> BitcoinClientResult<bool>;
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

    fn get_other_test_addr() -> Address {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let private_key = bitcoin::PrivateKey {
            compressed: true,
            network: Network::Testnet.into(),
            inner: bitcoin::secp256k1::SecretKey::from_slice(&[2; 32]).unwrap(),
        };
        let compressed_public_key =
            bitcoin::CompressedPublicKey::from_private_key(&secp, &private_key).unwrap();

        Address::p2wpkh(&compressed_public_key, Network::Testnet)
    }

    fn get_test_common_fields() -> CommonFields {
        CommonFields {
            schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
            encoded_public_key: bitcoin::script::PushBytesBuf::from([0u8; 32]),
            block_height: 0,
            block_hash: None,
            tx_id: Txid::all_zeros(),
            p2wpkh_address: Some(get_test_addr()),
            tx_index: None,
            output_vout: None,
        }
    }

    fn get_test_transaction(output_script: ScriptBuf) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::from_bytes(vec![1, 2, 3]),
                sequence: Sequence::MAX,
                witness: Witness::default(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(1_000),
                script_pubkey: output_script,
            }],
        }
    }

    fn get_indexer_with_mock(mock_client: MockBitcoinOps) -> BitcoinInscriptionIndexer {
        let wallets = Arc::new(SystemWallets {
            bridge: get_test_addr(),
            sequencer: get_test_addr(),
            governance: get_test_addr(),
            verifiers: vec![],
        });

        BitcoinInscriptionIndexer {
            client: Arc::new(mock_client),
            parser: MessageParser::new(Network::Testnet),
            wallets,
        }
    }

    #[derive(Default)]
    struct InMemoryLocatorStore {
        locators: Vec<BitcoinTxLocator>,
    }

    #[async_trait]
    impl BitcoinTxLocatorStore for InMemoryLocatorStore {
        async fn insert_tx_locator(&mut self, locator: &BitcoinTxLocator) -> anyhow::Result<()> {
            self.locators.push(locator.clone());
            Ok(())
        }

        async fn get_tx_locator(
            &mut self,
            txid: &Txid,
        ) -> anyhow::Result<Option<BitcoinTxLocator>> {
            Ok(self
                .locators
                .iter()
                .find(|locator| locator.txid == *txid)
                .cloned())
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
    async fn test_parse_transaction_with_locator_uses_block_hash() {
        let tx = get_test_transaction(ScriptBuf::new());
        let txid = tx.compute_txid();
        let block_hash = BlockHash::all_zeros();

        let mut mock_client = MockBitcoinOps::new();
        mock_client
            .expect_get_transaction_in_block()
            .with(eq(txid), eq(block_hash))
            .returning(move |_, _| Ok(tx.clone()))
            .once();

        let mut indexer = get_indexer_with_mock(mock_client);
        let messages = indexer
            .parse_transaction_with_locator(&BitcoinTxLocator {
                txid,
                block_height: 10,
                block_hash,
                tx_index: Some(0),
            })
            .await
            .unwrap();

        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn test_process_blocks_with_locator_store_persists_real_txid() {
        let tx = get_test_transaction(get_test_addr().script_pubkey());
        assert_ne!(
            tx.compute_txid().to_string(),
            tx.compute_ntxid().to_string()
        );

        let block = Block {
            header: Header {
                version: Default::default(),
                prev_blockhash: BlockHash::all_zeros(),
                merkle_root: TxMerkleNode::all_zeros(),
                time: 0,
                bits: Default::default(),
                nonce: 0,
            },
            txdata: vec![tx.clone()],
        };
        let block_hash = block.block_hash();

        let mut mock_client = MockBitcoinOps::new();
        mock_client
            .expect_fetch_block()
            .with(eq(42_u128))
            .returning(move |_| Ok(block.clone()))
            .once();

        let mut indexer = get_indexer_with_mock(mock_client);
        indexer.update_system_wallets(None, None, None, Some(get_other_test_addr()));
        let mut locator_store = InMemoryLocatorStore::default();
        let messages = indexer
            .process_blocks_with_locator_store(42, 42, &mut locator_store)
            .await
            .unwrap();

        assert!(messages.is_empty());
        assert_eq!(
            locator_store.locators,
            vec![BitcoinTxLocator {
                txid: tx.compute_txid(),
                block_height: 42,
                block_hash,
                tx_index: Some(0),
            }]
        );
    }

    #[tokio::test]
    async fn test_is_valid_message() {
        let indexer = get_indexer_with_mock(MockBitcoinOps::new());

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
                    block_hash: None,
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
                script_pubkey: indexer.wallets.bridge.script_pubkey(),
            }],
        });
        assert!(indexer.is_valid_bridge_message(&l1_to_l2_message).await);

        let system_bootstrapping =
            FullInscriptionMessage::SystemBootstrapping(types::SystemBootstrapping {
                common: get_test_common_fields(),
                input: types::SystemBootstrappingInput {
                    start_block_height: 0,
                    bridge_musig2_address: indexer.wallets.bridge.clone().as_unchecked().to_owned(),
                    verifier_p2wpkh_addresses: vec![],
                    bootloader_hash: H256::zero(),
                    abstract_account_hash: H256::zero(),
                    governance_address: indexer
                        .wallets
                        .governance
                        .clone()
                        .as_unchecked()
                        .to_owned(),
                    protocol_version: ProtocolSemanticVersion::new(
                        ProtocolVersionId::Version28,
                        VersionPatch(0),
                    ),
                    snark_wrapper_vk_hash: H256::zero(),
                    sequencer_address: indexer.wallets.sequencer.clone().as_unchecked().to_owned(),
                    evm_emulator_hash: H256::zero(),
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
                script_pubkey: indexer.wallets.bridge.script_pubkey(),
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
