use std::{borrow::Cow, collections::HashMap, sync::Arc};

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
        BitcoinIndexerResult, BridgeWithdrawal, FullInscriptionMessage, L1ToL2Message,
        SystemTransactions, TransactionWithMetadata,
    },
};

pub mod withdrawal;

/// Controls withdrawal provenance work independently of deposit and system-message ingestion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WithdrawalScanMode {
    /// Defer the whole scan on unavailable parents or credible malformed metadata.
    /// The RPC adapter needs historical transaction availability (normally txindex).
    /// Worst case: one RPC per distinct historical candidate parent per block scan,
    /// including third-party candidates. Same-block parents and repeats are cached.
    Required,
    /// Emit verified observations; skip unclassifiable withdrawals, not other messages.
    #[default]
    BestEffort,
    /// Do not recognize withdrawals or fetch their parents; still parse deposits.
    Ignore,
}

type WithdrawalParents<'a> = HashMap<Txid, Result<Cow<'a, BitcoinTransaction>, String>>;

/// The main indexer struct for processing Bitcoin inscriptions
#[derive(Debug, Clone)]
pub struct BitcoinInscriptionIndexer {
    client: Arc<dyn BitcoinOps>,
    wallets: Arc<SystemWallets>,
    parser: MessageParser,
    withdrawal_mode: WithdrawalScanMode,
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
            withdrawal_mode: WithdrawalScanMode::default(),
        }
    }

    pub fn with_withdrawal_mode(mut self, mode: WithdrawalScanMode) -> Self {
        self.withdrawal_mode = mode;
        self
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

    pub fn bridge_script_pubkey(&self) -> bitcoin::ScriptBuf {
        self.wallets.bridge.script_pubkey()
    }

    #[instrument(skip(self), target = "bitcoin_indexer")]
    pub async fn process_block(
        &mut self,
        block_height: u32,
    ) -> BitcoinIndexerResult<Vec<FullInscriptionMessage>> {
        debug!("Processing block at height {}", block_height);

        let block = self.client.fetch_block(block_height as u128).await?;
        let block_hash = block.block_hash();
        // TODO: check block header is belong to a valid chain of blocks (reorg detection and management)
        // TODO: deal with malicious sequencer, verifiers from being able to make trouble by sending invalid messages / valid messages with invalid data

        let mut valid_messages = Vec::new();

        let mut system_txs = self.extract_important_transactions(&block.txdata);

        // Parse protocol upgrade messages (Upgrade system contracts, bridge addresses, sequencer address)
        if !system_txs.governance_txs.is_empty() {
            let parsed_messages: Vec<_> = system_txs
                .governance_txs
                .iter()
                .flat_map(|tx| {
                    self.parser
                        .parse_protocol_upgrade_transactions(tx, block_height)
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

        if !system_txs.system_txs.is_empty() {
            let parsed_messages: Vec<_> = system_txs
                .system_txs
                .iter()
                .flat_map(|tx| {
                    self.parser
                        .parse_system_transaction(&tx.tx, block_height, Some(&self.wallets))
                })
                .collect();

            let messages: Vec<_> = parsed_messages
                .into_iter()
                .filter(|message| self.is_valid_system_message(message))
                .map(|mut message| {
                    match &mut message {
                        FullInscriptionMessage::ProofDAReference(proof) => {
                            proof.common.block_hash = Some(block_hash);
                        }
                        FullInscriptionMessage::ValidatorAttestation(vote) => {
                            vote.common.block_hash = Some(block_hash);
                        }
                        _ => {}
                    }
                    message
                })
                .collect();

            valid_messages.extend(messages);
        }

        if !system_txs.bridge_txs.is_empty() {
            let mut parents = WithdrawalParents::new();
            if self.withdrawal_mode != WithdrawalScanMode::Ignore {
                parents.extend(
                    block
                        .txdata
                        .iter()
                        .map(|tx| (tx.compute_txid(), Ok(Cow::Borrowed(tx)))),
                );
            }
            for tx in &mut system_txs.bridge_txs {
                let parsed = self
                    .parser
                    .parse_bridge_transaction(tx, block_height, &self.wallets);
                if self.withdrawal_mode == WithdrawalScanMode::Required
                    && MessageParser::is_withdrawal_candidate(&tx.tx)
                    && !parsed.iter().any(|message| {
                        matches!(message, FullInscriptionMessage::BridgeWithdrawal(_))
                    })
                {
                    let prevouts = self
                        .verified_withdrawal_prevouts(&tx.tx, &mut parents)
                        .await
                        .map_err(|err| {
                            crate::types::BitcoinError::InvalidTransaction(err.to_string())
                        })?;
                    if self.has_bridge_input(&prevouts) {
                        return Err(crate::types::BitcoinError::InvalidTransaction(
                            "Credible bridge withdrawal has incomplete metadata; defer scan".into(),
                        )
                        .into());
                    }
                }
                for mut message in parsed {
                    let valid = match self
                        .is_valid_bridge_message(&mut message, &mut parents)
                        .await
                    {
                        Ok(valid) => valid,
                        Err(err) if self.withdrawal_mode == WithdrawalScanMode::BestEffort => {
                            warn!("Skipping unclassifiable withdrawal: {err}");
                            false
                        }
                        Err(err) => {
                            return Err(crate::types::BitcoinError::InvalidTransaction(
                                err.to_string(),
                            )
                            .into())
                        }
                    };
                    if valid {
                        if let FullInscriptionMessage::BridgeWithdrawal(withdrawal) = &mut message {
                            withdrawal.input.block_hash = Some(block_hash);
                        }
                        valid_messages.push(message);
                    }
                }
            }
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

                let is_withdrawal = self.withdrawal_mode != WithdrawalScanMode::Ignore
                    && MessageParser::is_withdrawal_candidate(tx);
                if is_bridge_output || is_withdrawal {
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

    async fn is_valid_bridge_message(
        &self,
        message: &mut FullInscriptionMessage,
        parents: &mut WithdrawalParents<'_>,
    ) -> anyhow::Result<bool> {
        match message {
            FullInscriptionMessage::L1ToL2Message(m) => Ok(self.is_valid_l1_to_l2_transfer(m)),
            FullInscriptionMessage::BridgeWithdrawal(m) => {
                if self.withdrawal_mode == WithdrawalScanMode::Ignore {
                    return Ok(false);
                }
                self.is_valid_bridge_withdrawal(m, parents).await
            }
            _ => Ok(false),
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

    #[instrument(skip(self, message, parents), target = "bitcoin_indexer")]
    async fn is_valid_bridge_withdrawal(
        &self,
        message: &mut BridgeWithdrawal,
        parents: &mut WithdrawalParents<'_>,
    ) -> anyhow::Result<bool> {
        let prevouts = self
            .verified_withdrawal_prevouts(&message.input.transaction, parents)
            .await?;
        if !self.has_bridge_input(&prevouts) {
            return Ok(false);
        }
        message.input.prevouts = prevouts;
        message.input.bridge_script_pubkey = Some(self.wallets.bridge.script_pubkey());
        Ok(true)
    }

    fn has_bridge_input(&self, prevouts: &[(OutPoint, bitcoin::TxOut)]) -> bool {
        let script = self.wallets.bridge.script_pubkey();
        prevouts
            .iter()
            .any(|(_, output)| output.script_pubkey == script)
    }

    async fn verified_withdrawal_prevouts(
        &self,
        transaction: &BitcoinTransaction,
        parents: &mut WithdrawalParents<'_>,
    ) -> anyhow::Result<Vec<(OutPoint, bitcoin::TxOut)>> {
        let mut prevouts = Vec::with_capacity(transaction.input.len());
        for input in &transaction.input {
            let outpoint = input.previous_output;
            if let std::collections::hash_map::Entry::Vacant(entry) = parents.entry(outpoint.txid) {
                let parent = self
                    .client
                    .get_transaction(&outpoint.txid)
                    .await
                    .map_err(|err| err.to_string())
                    .and_then(|parent| {
                        if parent.compute_txid() == outpoint.txid {
                            Ok(Cow::Owned(parent))
                        } else {
                            Err("Parent transaction ID mismatch".into())
                        }
                    });
                entry.insert(parent);
            }
            let parent = parents[&outpoint.txid]
                .as_ref()
                .map_err(|err| anyhow::anyhow!("{err}"))?;
            let txout = parent
                .output
                .get(outpoint.vout as usize)
                .ok_or_else(|| anyhow::anyhow!("Withdrawal parent output unavailable"))?;
            prevouts.push((outpoint, txout.clone()));
        }
        Ok(prevouts)
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
        OutPoint, ScriptBuf, Transaction, TxIn, TxMerkleNode, TxOut,
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
            block_hash: None,
            tx_id: Txid::all_zeros(),
            p2wpkh_address: Some(get_test_addr()),
            tx_index: None,
            output_vout: None,
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
            withdrawal_mode: WithdrawalScanMode::Required,
        }
    }

    #[tokio::test]
    async fn source_inclusion_requires_block_scan_and_preserves_sender_authorization() {
        use bitcoin::{
            opcodes::{all, OP_FALSE},
            script::Builder,
            secp256k1::{Keypair, Secp256k1, SecretKey},
            taproot::LeafVersion,
            CompressedPublicKey, Witness,
        };

        let keypair =
            Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&[1; 32]).unwrap());
        let (internal_key, _) = keypair.x_only_public_key();
        let sender = Address::p2wpkh(&CompressedPublicKey(keypair.public_key()), Network::Testnet);
        let envelope = |message_type: &[u8]| {
            Builder::new()
                .push_slice(internal_key.serialize())
                .push_opcode(all::OP_CHECKSIG)
                .push_opcode(OP_FALSE)
                .push_opcode(all::OP_IF)
                .push_slice(b"via_inscription_protocol")
                .push_slice(bitcoin::script::PushBytesBuf::try_from(message_type.to_vec()).unwrap())
                .push_slice([7; 32])
        };
        let proof = envelope(types::PROOF_DA_REFERENCE_MSG.as_bytes())
            .push_slice(b"test-da")
            .push_slice(b"test-proof")
            .push_opcode(all::OP_ENDIF)
            .into_script();
        let vote = envelope(types::VALIDATOR_ATTESTATION_MSG.as_bytes())
            .push_int(1)
            .push_opcode(all::OP_ENDIF)
            .into_script();
        let mut control_block = vec![LeafVersion::TapScript.to_consensus()];
        control_block.extend(internal_key.serialize());
        let mut inputs: Vec<_> = [proof, vote]
            .into_iter()
            .map(|script| TxIn {
                witness: Witness::from_slice(&[
                    vec![0; 64],
                    script.into_bytes(),
                    control_block.clone(),
                ]),
                ..TxIn::default()
            })
            .collect();
        inputs.push(TxIn {
            witness: Witness::from_slice(&[vec![0; 64], keypair.public_key().serialize().to_vec()]),
            ..TxIn::default()
        });
        let transaction = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: inputs,
            output: vec![],
        };
        let block = Block {
            header: Header {
                version: Default::default(),
                prev_blockhash: BlockHash::all_zeros(),
                merkle_root: TxMerkleNode::all_zeros(),
                time: 42,
                bits: Default::default(),
                nonce: 7,
            },
            txdata: vec![transaction.clone()],
        };
        let scanned_hash = block.block_hash();
        let common = |message: FullInscriptionMessage| match message {
            FullInscriptionMessage::ProofDAReference(proof) => proof.common,
            FullInscriptionMessage::ValidatorAttestation(vote) => vote.common,
            other => panic!("unexpected source message: {other:?}"),
        };
        let parsed =
            MessageParser::new(Network::Testnet).parse_system_transaction(&transaction, 42, None);
        assert_eq!(parsed.len(), 2);
        for message in parsed {
            assert_eq!(common(message).block_hash, None);
        }
        for mode in [
            WithdrawalScanMode::Ignore,
            WithdrawalScanMode::BestEffort,
            WithdrawalScanMode::Required,
        ] {
            let mut client = MockBitcoinOps::new();
            let scanned_block = block.clone();
            client
                .expect_fetch_block()
                .with(eq(42u128))
                .times(2)
                .returning(move |_| Ok(scanned_block.clone()));
            let mut indexer = get_indexer_with_mock(client).with_withdrawal_mode(mode);
            let wallets = Arc::make_mut(&mut indexer.wallets);
            wallets.sequencer = sender.clone();
            wallets.verifiers = vec![sender.clone()];
            let messages = indexer.process_block(42).await.unwrap();
            assert_eq!(messages.len(), 2);
            for message in messages {
                let source = common(message);
                assert_eq!(
                    (source.block_height, source.block_hash),
                    (42, Some(scanned_hash))
                );
            }
            Arc::make_mut(&mut indexer.wallets).verifiers.clear();
            let messages = indexer.process_block(42).await.unwrap();
            assert!(matches!(
                messages.as_slice(),
                [FullInscriptionMessage::ProofDAReference(_)]
            ));
        }
    }

    #[tokio::test]
    async fn withdrawal_modes_scope_missing_parent_and_spam_to_withdrawals() {
        let bridge = Address::p2tr(
            &bitcoin::secp256k1::Secp256k1::new(),
            bitcoin::XOnlyPublicKey::from_str(
                "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
            )
            .unwrap(),
            None,
            Network::Testnet,
        );
        for mode in [
            WithdrawalScanMode::Ignore,
            WithdrawalScanMode::BestEffort,
            WithdrawalScanMode::Required,
        ] {
            for malformed in [false, true] {
                let metadata = if malformed {
                    b"VIA_WI".to_vec()
                } else {
                    [b"VIA_WI\0".as_slice(), &[1; 10]].concat()
                };
                let mut candidate = Transaction {
                    version: transaction::Version::TWO,
                    lock_time: absolute::LockTime::ZERO,
                    input: vec![TxIn::default()],
                    output: vec![
                        TxOut {
                            value: Amount::from_sat(100_000),
                            script_pubkey: bridge.script_pubkey(),
                        },
                        TxOut {
                            value: Amount::ZERO,
                            script_pubkey: ScriptBuf::new_op_return(
                                bitcoin::script::PushBytesBuf::try_from(metadata).unwrap(),
                            ),
                        },
                    ],
                };
                // An unrelated deposit on the same transaction must survive skipping
                // its withdrawal classification, as must a separate deposit transaction.
                candidate.input[0].witness =
                    parser::tests::inscription_witness(&[0x83; 20], &[0; 20]);
                let deposit = Transaction {
                    version: transaction::Version::TWO,
                    lock_time: absolute::LockTime::ZERO,
                    input: vec![TxIn::default()],
                    output: vec![
                        TxOut {
                            value: Amount::from_sat(100_000),
                            script_pubkey: bridge.script_pubkey(),
                        },
                        TxOut {
                            value: Amount::ZERO,
                            script_pubkey: ScriptBuf::new_op_return([0x81; 20]),
                        },
                    ],
                };
                let block = Block {
                    header: Header {
                        version: Default::default(),
                        prev_blockhash: BlockHash::all_zeros(),
                        merkle_root: TxMerkleNode::all_zeros(),
                        time: 0,
                        bits: Default::default(),
                        nonce: 0,
                    },
                    txdata: vec![candidate, deposit],
                };
                let mut client = MockBitcoinOps::new();
                client
                    .expect_fetch_block()
                    .returning(move |_| Ok(block.clone()));
                let lookups = usize::from(
                    mode != WithdrawalScanMode::Ignore
                        && (!malformed || mode == WithdrawalScanMode::Required),
                );
                client
                    .expect_get_transaction()
                    .times(lookups)
                    .returning(|_| {
                        Err(types::BitcoinError::InvalidTransaction(
                            "parent unavailable".into(),
                        ))
                    });
                let mut indexer = get_indexer_with_mock(client).with_withdrawal_mode(mode);
                Arc::make_mut(&mut indexer.wallets).bridge = bridge.clone();
                let result = indexer.process_blocks(42, 42).await;
                if mode == WithdrawalScanMode::Required {
                    assert!(result.is_err());
                } else {
                    let messages = result.unwrap();
                    let receivers: Vec<_> = messages
                        .iter()
                        .map(|message| {
                            let FullInscriptionMessage::L1ToL2Message(deposit) = message else {
                                panic!("expected only deposits, got {message:?}");
                            };
                            deposit.input.receiver_l2_address
                        })
                        .collect();
                    assert_eq!(
                        receivers,
                        vec![
                            zksync_types::Address::repeat_byte(0x83),
                            zksync_types::Address::repeat_byte(0x81)
                        ]
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn same_block_parents_preserve_verified_mixed_input_payments_without_rpc() {
        for mode in [WithdrawalScanMode::Required, WithdrawalScanMode::BestEffort] {
            let parent = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn::default()],
                output: vec![
                    TxOut {
                        value: Amount::from_sat(10_000),
                        script_pubkey: ScriptBuf::new(),
                    },
                    TxOut {
                        value: Amount::from_sat(10_000),
                        script_pubkey: get_test_addr().script_pubkey(),
                    },
                ],
            };
            let payment = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: [0, 1]
                    .into_iter()
                    .map(|vout| TxIn {
                        previous_output: OutPoint {
                            txid: parent.compute_txid(),
                            vout,
                        },
                        ..TxIn::default()
                    })
                    .collect(),
                output: vec![
                    TxOut {
                        value: Amount::from_sat(19_000),
                        script_pubkey: get_test_addr().script_pubkey(),
                    },
                    TxOut {
                        value: Amount::ZERO,
                        script_pubkey: ScriptBuf::new_op_return(
                            bitcoin::script::PushBytesBuf::try_from(
                                [b"VIA_WI\0".as_slice(), &[1; 10]].concat(),
                            )
                            .unwrap(),
                        ),
                    },
                ],
            };
            let block = Block {
                header: Header {
                    version: Default::default(),
                    prev_blockhash: BlockHash::all_zeros(),
                    merkle_root: TxMerkleNode::all_zeros(),
                    time: 0,
                    bits: Default::default(),
                    nonce: 0,
                },
                txdata: vec![parent.clone(), payment.clone()],
            };
            let mut client = MockBitcoinOps::new();
            client
                .expect_fetch_block()
                .returning(move |_| Ok(block.clone()));
            client.expect_get_transaction().times(0);
            let mut indexer = get_indexer_with_mock(client).with_withdrawal_mode(mode);
            let messages = indexer.process_blocks(42, 42).await.unwrap();
            let [FullInscriptionMessage::BridgeWithdrawal(observation)] = messages.as_slice()
            else {
                panic!("expected complete mixed-input observation, got {messages:?}");
            };
            assert_eq!(observation.input.transaction, payment);
            assert_eq!(observation.common.tx_id, payment.compute_txid());
            assert_eq!(
                observation.input.prevouts,
                payment
                    .input
                    .iter()
                    .zip(&parent.output)
                    .map(|(input, output)| (input.previous_output, output.clone()))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[tokio::test]
    async fn withdrawal_parent_unavailability_defers_and_retry_retains_whole_payment() {
        let parent = BitcoinTransaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey: get_test_addr().script_pubkey(),
            }],
        };
        let payment = BitcoinTransaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: parent.compute_txid(),
                    vout: 0,
                },
                ..TxIn::default()
            }],
            output: vec![
                TxOut {
                    value: Amount::from_sat(9_000),
                    script_pubkey: get_test_addr().script_pubkey(),
                },
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::new_op_return(
                        bitcoin::script::PushBytesBuf::try_from(
                            [b"VIA_WI\0".as_slice(), &[1; 10]].concat(),
                        )
                        .unwrap(),
                    ),
                },
            ],
        };
        let block = Block {
            header: Header {
                version: Default::default(),
                prev_blockhash: BlockHash::all_zeros(),
                merkle_root: TxMerkleNode::all_zeros(),
                time: 0,
                bits: Default::default(),
                nonce: 0,
            },
            txdata: vec![payment.clone()],
        };
        let block_hash = block.block_hash();
        let mut client = MockBitcoinOps::new();
        client
            .expect_fetch_block()
            .returning(move |_| Ok(block.clone()));
        let mut sequence = mockall::Sequence::new();
        client
            .expect_get_transaction()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| {
                Err(types::BitcoinError::InvalidTransaction(
                    "parent unavailable".into(),
                ))
            });
        let verified_parent = parent.clone();
        client
            .expect_get_transaction()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_| Ok(verified_parent.clone()));
        let mut indexer = get_indexer_with_mock(client);
        assert!(indexer.process_blocks(42, 42).await.is_err());
        let messages = indexer.process_blocks(42, 42).await.unwrap();
        let [FullInscriptionMessage::BridgeWithdrawal(observation)] = messages.as_slice() else {
            panic!("expected bridge payment");
        };
        assert_eq!(observation.input.transaction, payment);
        assert_eq!(
            observation.input.prevouts,
            vec![(payment.input[0].previous_output, parent.output[0].clone())]
        );
        assert_eq!(observation.input.block_hash, Some(block_hash));
    }

    #[tokio::test]
    async fn third_party_metadata_does_not_create_a_bridge_observation() {
        let parent = BitcoinTransaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![
                TxOut {
                    value: Amount::from_sat(10_000),
                    script_pubkey: ScriptBuf::new(),
                },
                TxOut {
                    value: Amount::from_sat(1_000),
                    script_pubkey: get_test_addr().script_pubkey(),
                },
            ],
        };
        let mut tx = BitcoinTransaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: parent.compute_txid(),
                    vout: 0,
                },
                ..TxIn::default()
            }],
            output: vec![
                TxOut {
                    value: Amount::from_sat(9_000),
                    script_pubkey: get_test_addr().script_pubkey(),
                },
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::new_op_return(
                        bitcoin::script::PushBytesBuf::try_from(
                            [b"VIA_WI\0".as_slice(), &[1; 10]].concat(),
                        )
                        .unwrap(),
                    ),
                },
            ],
        };
        let mut client = MockBitcoinOps::new();
        client
            .expect_get_transaction()
            .times(2)
            .returning(move |_| Ok(parent.clone()));
        let indexer = get_indexer_with_mock(client);
        let mut message = indexer
            .parser
            .parse_op_return_withdrawal(&tx, 42, &indexer.wallets)
            .unwrap();
        assert!(!indexer
            .is_valid_bridge_message(&mut message, &mut WithdrawalParents::new())
            .await
            .unwrap());
        tx.input.push(TxIn {
            previous_output: OutPoint {
                txid: tx.input[0].previous_output.txid,
                vout: 1,
            },
            ..TxIn::default()
        });
        let mut mixed = indexer
            .parser
            .parse_op_return_withdrawal(&tx, 42, &indexer.wallets)
            .unwrap();
        assert!(indexer
            .is_valid_bridge_message(&mut mixed, &mut WithdrawalParents::new())
            .await
            .unwrap());
        let FullInscriptionMessage::BridgeWithdrawal(mixed) = mixed else {
            unreachable!()
        };
        assert_eq!(mixed.input.prevouts.len(), 2);
        assert_eq!(
            mixed.input.bridge_script_pubkey,
            Some(get_test_addr().script_pubkey())
        );
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
    async fn malformed_bridge_metadata_defers_but_unrelated_metadata_does_not() {
        for bridge_spend in [true, false] {
            let parent = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn::default()],
                output: vec![TxOut {
                    value: Amount::from_sat(10_000),
                    script_pubkey: if bridge_spend {
                        get_test_addr().script_pubkey()
                    } else {
                        ScriptBuf::new()
                    },
                }],
            };
            let payment = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: OutPoint {
                        txid: parent.compute_txid(),
                        vout: 0,
                    },
                    ..TxIn::default()
                }],
                output: vec![TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::new_op_return(b"VIA_WI"),
                }],
            };
            let block = Block {
                header: Header {
                    version: Default::default(),
                    prev_blockhash: BlockHash::all_zeros(),
                    merkle_root: TxMerkleNode::all_zeros(),
                    time: 0,
                    bits: Default::default(),
                    nonce: 0,
                },
                txdata: vec![payment],
            };
            let mut client = MockBitcoinOps::new();
            client
                .expect_fetch_block()
                .returning(move |_| Ok(block.clone()));
            client
                .expect_get_transaction()
                .returning(move |_| Ok(parent.clone()));
            let mut indexer = get_indexer_with_mock(client);
            let result = indexer.process_blocks(42, 42).await;
            if bridge_spend {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_empty());
            }
        }
    }

    #[tokio::test]
    async fn test_process_blocks() {
        let start_block = 1;
        let end_block = 3;

        let mut op_return_script = vec![0x6a, 0x13];
        op_return_script.extend([0x55; 19]);
        let malformed_transaction = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![
                TxOut {
                    value: Amount::from_sat(100_000),
                    script_pubkey: get_test_addr().script_pubkey(),
                },
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::from_bytes(op_return_script),
                },
            ],
        };
        let mut valid_transaction = malformed_transaction.clone();
        valid_transaction.output[1].script_pubkey = ScriptBuf::new_op_return([0x81; 20]);
        let mut malformed_receiver = malformed_transaction.clone();
        malformed_receiver.input[0].witness =
            parser::tests::inscription_witness(&[0x55; 19], &[0; 20]);
        let mut malformed_withdrawal = malformed_transaction.clone();
        malformed_withdrawal.output[1].script_pubkey = ScriptBuf::new_op_return(b"VIA_WI");
        let unrelated_parent = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(100_000),
                script_pubkey: ScriptBuf::new(),
            }],
        };
        malformed_withdrawal.input[0].previous_output = OutPoint {
            txid: unrelated_parent.compute_txid(),
            vout: 0,
        };

        let mock_block = Block {
            header: Header {
                version: Default::default(),
                prev_blockhash: BlockHash::all_zeros(),
                merkle_root: TxMerkleNode::all_zeros(),
                time: 0,
                bits: Default::default(),
                nonce: 0,
            },
            txdata: vec![
                malformed_transaction,
                malformed_receiver,
                malformed_withdrawal,
                valid_transaction,
            ],
        };

        let mut mock_client = MockBitcoinOps::new();
        mock_client
            .expect_get_transaction()
            .returning(move |_| Ok(unrelated_parent.clone()));
        mock_client
            .expect_fetch_block()
            .returning(move |_| Ok(mock_block.clone()))
            .times(3);
        mock_client
            .expect_get_network()
            .returning(|| Network::Testnet);

        let mut indexer = get_indexer_with_mock(mock_client);
        let messages = indexer
            .process_blocks(start_block, end_block)
            .await
            .unwrap();
        assert_eq!(messages.len(), 3);
        for (message, block_height) in messages.iter().zip([1, 2, 3]) {
            let FullInscriptionMessage::L1ToL2Message(deposit) = message else {
                panic!("expected an L1-to-L2 deposit, got {message:?}");
            };
            assert_eq!(deposit.input.receiver_l2_address.as_bytes(), &[0x81; 20]);
            assert_eq!(deposit.amount, Amount::from_sat(100_000));
            assert_eq!(deposit.common.block_height, block_height);
            assert_eq!(deposit.common.tx_index, Some(3));
            assert_eq!(deposit.common.output_vout, Some(0));
        }
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

        let mut l1_to_l2_message = FullInscriptionMessage::L1ToL2Message(L1ToL2Message {
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
        assert!(indexer
            .is_valid_bridge_message(&mut l1_to_l2_message, &mut WithdrawalParents::new())
            .await
            .unwrap());

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
