use std::{collections::HashMap, sync::Arc};

use anyhow::Context as _;
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
        BitcoinIndexerResult, FullInscriptionMessage, L1ToL2Message, SystemContractUpgrade,
        SystemContractUpgradeProposalInput, SystemTransactions, TransactionWithMetadata,
    },
};

pub mod withdrawal;

/// The main indexer struct for processing Bitcoin inscriptions
#[derive(Debug, Clone)]
pub struct BitcoinInscriptionIndexer {
    client: Arc<dyn BitcoinOps>,
    wallets: Arc<SystemWallets>,
    parser: MessageParser,
}

/// Fetches every proposal referenced by an activation; callers retain version filtering and effects.
pub async fn resolve_upgrade_proposals(
    client: &dyn BitcoinOps,
    activation: &SystemContractUpgrade,
) -> anyhow::Result<Vec<SystemContractUpgradeProposalInput>> {
    let proposal_tx_id = activation.input.proposal_tx_id;
    let proposal_tx = client
        .get_transaction(&proposal_tx_id)
        .await
        .with_context(|| {
            format!("failed to fetch protocol upgrade transaction {proposal_tx_id}")
        })?;
    let mut parser = MessageParser::new(client.get_network());

    Ok(parser
        .parse_system_transaction(&proposal_tx, activation.common.block_height, None)
        .into_iter()
        .filter_map(|message| match message {
            FullInscriptionMessage::SystemContractUpgradeProposal(proposal) => Some(proposal.input),
            _ => None,
        })
        .collect())
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
        debug!("Processing block at height {}", block_height);

        let block = self.client.fetch_block(block_height as u128).await?;
        // TODO: check block header is belong to a valid chain of blocks (reorg detection and management)
        // TODO: deal with malicious sequencer, verifiers from being able to make trouble by sending invalid messages / valid messages with invalid data

        let mut valid_messages = Vec::new();
        let mut parent_transactions = HashMap::new();

        let mut system_txs = self.extract_important_transactions(&block.txdata);

        for tx in &system_txs.governance_txs {
            for message in self
                .parser
                .parse_protocol_upgrade_transactions(tx, block_height)
            {
                let outpoint = match &message {
                    FullInscriptionMessage::SystemContractUpgrade(m) => m.input.inputs.first(),
                    FullInscriptionMessage::UpdateBridge(m) => m.input.inputs.first(),
                    FullInscriptionMessage::UpdateSequencer(m) => m.input.inputs.first(),
                    FullInscriptionMessage::UpdateGovernance(m) => m.input.inputs.first(),
                    _ => continue,
                };
                if self
                    .is_valid_prevout(outpoint, &self.wallets.governance, &mut parent_transactions)
                    .await?
                {
                    valid_messages.push(message);
                }
            }
        }

        for tx in &system_txs.system_txs {
            valid_messages.extend(
                self.parser
                    .parse_system_transaction(&tx.tx, block_height, Some(&self.wallets))
                    .into_iter()
                    .filter(|message| self.is_valid_scanned_system_message(message)),
            );
        }

        for tx in &mut system_txs.bridge_txs {
            for message in self
                .parser
                .parse_bridge_transaction(tx, block_height, &self.wallets)
            {
                if match &message {
                    FullInscriptionMessage::L1ToL2Message(m) => self.is_valid_l1_to_l2_transfer(m),
                    FullInscriptionMessage::BridgeWithdrawal(m) => {
                        self.is_valid_prevout(
                            m.input.inputs.first(),
                            &self.wallets.bridge,
                            &mut parent_transactions,
                        )
                        .await?
                    }
                    _ => false,
                } {
                    valid_messages.push(message);
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
        let bridge_script = self.wallets.bridge.script_pubkey();
        let governance_script = self.wallets.governance.script_pubkey();
        let mut system_txs = Vec::new();
        let mut bridge_txs = Vec::new();
        let mut governance_txs = Vec::new();

        for (tx_index, tx) in transactions.iter().enumerate() {
            if self
                .parser
                .unique_claimed_p2wpkh_address(tx)
                .is_some_and(|address| {
                    address == self.wallets.sequencer || self.wallets.verifiers.contains(&address)
                })
            {
                system_txs.push(TransactionWithMetadata::new(tx.clone(), tx_index));
            }
            // A parsed carrier-only withdrawal with a new first-input parent costs one fetch.
            // Fees and block space bound the remaining fetches.
            if tx
                .output
                .iter()
                .any(|output| output.script_pubkey == bridge_script)
                || parser::has_withdrawal_carrier(tx)
            {
                bridge_txs.push(TransactionWithMetadata::new(tx.clone(), tx_index));
            }
            if tx
                .output
                .iter()
                .any(|output| output.script_pubkey == governance_script)
            {
                governance_txs.push(TransactionWithMetadata::new(tx.clone(), tx_index));
            }
        }

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
    /// Applies wallet-role policy only to system messages discovered during ordinary block
    /// scanning; proposal payloads selected by a governance-authorized activation are outside this
    /// authorization domain.
    #[instrument(skip(self, message), target = "bitcoin_indexer")]
    fn is_valid_scanned_system_message(&self, message: &FullInscriptionMessage) -> bool {
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
            FullInscriptionMessage::SystemContractUpgradeProposal(m) => m
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

    #[instrument(skip_all, target = "bitcoin_indexer")]
    async fn is_valid_prevout(
        &self,
        outpoint_opt: Option<&OutPoint>,
        expected_address: &Address,
        parent_transactions: &mut HashMap<Txid, BitcoinTransaction>,
    ) -> anyhow::Result<bool> {
        use std::collections::hash_map::Entry;
        let Some(outpoint) = outpoint_opt.filter(|outpoint| !outpoint.is_null()) else {
            return Ok(false);
        };
        if let Entry::Vacant(entry) = parent_transactions.entry(outpoint.txid) {
            let fetched = self.client.get_transaction(&outpoint.txid).await;
            let tx = fetched
                .with_context(|| format!("failed to fetch prevout transaction {outpoint}"))?;
            entry.insert(tx);
        }
        Ok(parent_transactions
            .get(&outpoint.txid)
            .and_then(|tx| tx.output.get(outpoint.vout as usize))
            .is_some_and(|txout| txout.script_pubkey == expected_address.script_pubkey()))
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
    use async_trait::async_trait;
    use bitcoin::{
        absolute, block::Header, hashes::Hash, secp256k1, transaction, Address, Amount, Block,
        Network, OutPoint, ScriptBuf, Transaction, TxIn, TxMerkleNode, TxOut, Witness,
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

    fn p2wpkh_key(secret: u8) -> bitcoin::PublicKey {
        let secp = secp256k1::Secp256k1::new();
        let mut bytes = [0; 32];
        bytes[31] = secret;
        let secret = secp256k1::SecretKey::from_slice(&bytes).unwrap();
        bitcoin::PublicKey::new(secp256k1::PublicKey::from_secret_key(&secp, &secret))
    }

    fn get_test_addr(secret: u8) -> Address {
        let key = bitcoin::CompressedPublicKey::try_from(p2wpkh_key(secret)).unwrap();
        Address::p2wpkh(&key, Network::Testnet)
    }

    fn get_test_common_fields() -> CommonFields {
        CommonFields {
            schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
            encoded_public_key: bitcoin::script::PushBytesBuf::from([0u8; 32]),
            block_height: 0,
            tx_id: Txid::all_zeros(),
            p2wpkh_address: Some(get_test_addr(1)),
            tx_index: None,
            output_vout: None,
        }
    }

    fn get_indexer_with_mock(mock_client: MockBitcoinOps) -> BitcoinInscriptionIndexer {
        let wallets = Arc::new(SystemWallets {
            bridge: get_test_addr(2),
            sequencer: get_test_addr(1),
            governance: get_test_addr(3),
            verifiers: vec![get_test_addr(4)],
        });

        BitcoinInscriptionIndexer {
            client: Arc::new(mock_client),
            parser: MessageParser::new(Network::Testnet),
            wallets,
        }
    }

    fn p2wpkh_witness(secret: u8) -> Witness {
        Witness::from_slice(&[Vec::new(), p2wpkh_key(secret).to_bytes()])
    }

    fn test_block(txdata: Vec<Transaction>) -> Block {
        Block {
            header: Header {
                version: Default::default(),
                prev_blockhash: BlockHash::all_zeros(),
                merkle_root: TxMerkleNode::all_zeros(),
                time: 0,
                bits: Default::default(),
                nonce: 0,
            },
            txdata,
        }
    }

    fn test_output(script_pubkey: ScriptBuf) -> TxOut {
        TxOut {
            value: Amount::from_sat(1_000),
            script_pubkey,
        }
    }

    fn test_transaction(previous_outputs: Vec<OutPoint>, output: Vec<TxOut>) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: previous_outputs
                .into_iter()
                .map(|previous_output| TxIn {
                    previous_output,
                    ..Default::default()
                })
                .collect(),
            output,
        }
    }

    fn carrier_only_withdrawal(previous_output: OutPoint, marker: u8) -> Transaction {
        let mut payload = withdrawal::VIA_WI.to_vec();
        payload.push(0);
        payload.extend([marker; 10]);

        test_transaction(
            vec![previous_output],
            vec![
                test_output(get_test_addr(6).script_pubkey()),
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::new_op_return(
                        bitcoin::script::PushBytesBuf::try_from(payload).unwrap(),
                    ),
                },
            ],
        )
    }

    #[tokio::test]
    async fn resolves_upgrade_proposal_input() {
        let proposal_tx = parser::system_contract_upgrade_transaction(1);
        let proposal_tx_id = proposal_tx.compute_txid();
        let activation = types::SystemContractUpgrade {
            common: get_test_common_fields(),
            input: types::SystemContractUpgradeInput {
                inputs: vec![],
                proposal_tx_id,
            },
        };
        let mut client = MockBitcoinOps::new();
        client
            .expect_get_transaction()
            .with(eq(proposal_tx_id))
            .return_once(move |_| Ok(proposal_tx));
        client.expect_get_network().return_const(Network::Regtest);

        let proposals = resolve_upgrade_proposals(&client, &activation)
            .await
            .unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].system_contracts.len(), 1);
    }

    #[test]
    fn test_is_valid_scanned_system_and_bridge_messages() {
        let indexer = get_indexer_with_mock(MockBitcoinOps::new());

        let validator_attestation =
            FullInscriptionMessage::ValidatorAttestation(types::ValidatorAttestation {
                common: get_test_common_fields(),
                input: types::ValidatorAttestationInput {
                    reference_txid: Txid::all_zeros(),
                    attestation: Vote::Ok,
                },
            });
        assert!(!indexer.is_valid_scanned_system_message(&validator_attestation));

        let l1_batch_da_reference =
            FullInscriptionMessage::L1BatchDAReference(types::L1BatchDAReference {
                common: get_test_common_fields(),
                input: types::L1BatchDAReferenceInput {
                    l1_batch_hash: zksync_basic_types::H256::zero(),
                    l1_batch_index: zksync_types::L1BatchNumber(0),
                    da_identifier: "test".to_string(),
                    blob_id: "test".to_string(),
                    prev_l1_batch_hash: zksync_basic_types::H256::zero(),
                },
            });
        assert!(indexer.is_valid_scanned_system_message(&l1_batch_da_reference));

        let mut proposal_common = get_test_common_fields();
        proposal_common.p2wpkh_address = Some(indexer.wallets.sequencer.clone());
        let mut system_contract_upgrade_proposal =
            FullInscriptionMessage::SystemContractUpgradeProposal(
                types::SystemContractUpgradeProposal {
                    common: proposal_common,
                    input: types::SystemContractUpgradeProposalInput {
                        version: ProtocolSemanticVersion::new(
                            ProtocolVersionId::Version28,
                            VersionPatch(1),
                        ),
                        bootloader_code_hash: H256::zero(),
                        default_account_code_hash: H256::zero(),
                        evm_emulator_code_hash: None,
                        recursion_scheduler_level_vk_hash: H256::zero(),
                        system_contracts: vec![],
                    },
                },
            );
        assert!(indexer.is_valid_scanned_system_message(&system_contract_upgrade_proposal));

        for address in [
            &indexer.wallets.bridge,
            &indexer.wallets.governance,
            &indexer.wallets.verifiers[0],
        ] {
            let FullInscriptionMessage::SystemContractUpgradeProposal(proposal) =
                &mut system_contract_upgrade_proposal
            else {
                unreachable!();
            };
            proposal.common.p2wpkh_address = Some(address.clone());
            assert!(!indexer.is_valid_scanned_system_message(&system_contract_upgrade_proposal));
        }

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
        assert!(indexer.is_valid_scanned_system_message(&system_bootstrapping));
    }

    #[test]
    fn important_transaction_classification_covers_unique_senders_and_withdrawals() {
        let indexer = get_indexer_with_mock(MockBitcoinOps::new());
        let authorized = p2wpkh_witness(1);
        assert_eq!(
            indexer.parser.parse_p2wpkh(&authorized),
            Some(indexer.wallets.sequencer.clone())
        );
        let transaction = |witnesses: Vec<Witness>| Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: witnesses
                .into_iter()
                .map(|witness| TxIn {
                    witness,
                    ..Default::default()
                })
                .collect(),
            output: vec![],
        };
        assert_eq!(
            indexer
                .extract_important_transactions(&[transaction(vec![authorized.clone()])])
                .system_txs
                .len(),
            1
        );

        assert!(indexer
            .extract_important_transactions(&[transaction(vec![authorized, p2wpkh_witness(2)])])
            .system_txs
            .is_empty());

        let mut withdrawal = transaction(vec![]);
        let payload = [withdrawal::VIA_WI, &[0; 11]].concat();
        withdrawal.output.push(TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new_op_return(
                bitcoin::script::PushBytesBuf::try_from(payload).unwrap(),
            ),
        });
        let output_transaction = |script_pubkey| Transaction {
            output: vec![TxOut {
                value: Amount::ZERO,
                script_pubkey,
            }],
            ..transaction(vec![])
        };
        let important = indexer.extract_important_transactions(&[
            transaction(vec![]),
            output_transaction(indexer.wallets.bridge.script_pubkey()),
            output_transaction(indexer.wallets.governance.script_pubkey()),
            withdrawal,
        ]);
        assert_eq!(
            important
                .bridge_txs
                .iter()
                .map(|tx| tx.tx_index)
                .collect::<Vec<_>>(),
            [1, 3]
        );
        assert_eq!(important.governance_txs[0].tx_index, 2);
        assert_eq!(important.governance_txs.len(), 1);
    }

    #[tokio::test]
    async fn process_block_caches_shared_parent_without_changing_prevout_validation() {
        let bridge_script = get_test_addr(2).script_pubkey();
        let non_bridge_script = get_test_addr(5).script_pubkey();
        let deposit = test_transaction(
            vec![OutPoint::new(Txid::from_byte_array([0x66; 32]), 0)],
            vec![
                test_output(bridge_script.clone()),
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::new_op_return(
                        bitcoin::script::PushBytesBuf::try_from(vec![0x77; 20]).unwrap(),
                    ),
                },
            ],
        );
        let parent = test_transaction(
            vec![],
            vec![
                test_output(non_bridge_script.clone()),
                test_output(non_bridge_script),
                test_output(bridge_script.clone()),
            ],
        );
        let parent_txid = parent.compute_txid();
        let children = vec![
            carrier_only_withdrawal(OutPoint::new(parent_txid, 0), 0x11),
            carrier_only_withdrawal(OutPoint::new(parent_txid, 1), 0x22),
            carrier_only_withdrawal(OutPoint::new(parent_txid, 2), 0x33),
        ];

        let mut mock_client = MockBitcoinOps::new();
        mock_client
            .expect_fetch_block()
            .with(eq(42_u128))
            .times(1)
            .return_once(move |_| {
                Ok(test_block(
                    std::iter::once(deposit).chain(children).collect(),
                ))
            });
        mock_client
            .expect_get_transaction()
            .with(eq(parent_txid))
            .times(1)
            .return_once(move |_| Ok(parent));

        let messages = get_indexer_with_mock(mock_client)
            .process_block(42)
            .await
            .unwrap();

        assert!(matches!(
            messages.as_slice(),
            [
                FullInscriptionMessage::L1ToL2Message(deposit),
                FullInscriptionMessage::BridgeWithdrawal(withdrawal),
            ] if deposit.amount == Amount::from_sat(1_000)
                && withdrawal.input.inputs == vec![OutPoint::new(parent_txid, 2)]
                && withdrawal.input.withdrawals.len() == 1
        ));
    }

    #[tokio::test]
    async fn failed_parent_fetch_is_not_cached() {
        let parent = test_transaction(vec![], vec![test_output(get_test_addr(2).script_pubkey())]);
        let parent_txid = parent.compute_txid();
        let mut sequence = mockall::Sequence::new();
        let mut mock_client = MockBitcoinOps::new();
        mock_client
            .expect_get_transaction()
            .with(eq(parent_txid))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(types::BitcoinError::Other("transient parent fetch".into())));
        mock_client
            .expect_get_transaction()
            .with(eq(parent_txid))
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(move |_| Ok(parent));

        let indexer = get_indexer_with_mock(mock_client);
        let outpoint = OutPoint::new(parent_txid, 0);
        let mut parent_transactions = HashMap::new();

        assert!(indexer
            .is_valid_prevout(
                Some(&outpoint),
                &indexer.wallets.bridge,
                &mut parent_transactions,
            )
            .await
            .is_err());
        assert!(parent_transactions.is_empty());
        assert!(indexer
            .is_valid_prevout(
                Some(&outpoint),
                &indexer.wallets.bridge,
                &mut parent_transactions,
            )
            .await
            .unwrap());
        assert_eq!(parent_transactions.len(), 1);
    }

    #[tokio::test]
    async fn process_block_propagates_parent_fetch_error() {
        let parent_txid = Txid::from_byte_array([0x88; 32]);
        let child = carrier_only_withdrawal(OutPoint::new(parent_txid, 0), 0x11);
        let mut mock_client = MockBitcoinOps::new();
        mock_client
            .expect_fetch_block()
            .with(eq(42_u128))
            .return_once(move |_| Ok(test_block(vec![child])));
        mock_client
            .expect_get_transaction()
            .with(eq(parent_txid))
            .returning(|_| Err(types::BitcoinError::Other("transient parent fetch".into())));

        let error = get_indexer_with_mock(mock_client)
            .process_block(42)
            .await
            .unwrap_err();
        let types::IndexerError::Other(error) = error else {
            panic!("expected contextual indexer error");
        };
        let error = format!("{error:#}");
        assert!(error.contains("failed to fetch prevout transaction"));
        assert!(error.contains("transient parent fetch"));
    }

    #[tokio::test]
    async fn non_error_prevout_failures_remain_invalid() {
        let parent = test_transaction(vec![], vec![test_output(get_test_addr(5).script_pubkey())]);
        let parent_txid = parent.compute_txid();
        let mut mock_client = MockBitcoinOps::new();
        mock_client
            .expect_get_transaction()
            .with(eq(parent_txid))
            .times(1)
            .return_once(move |_| Ok(parent));
        let indexer = get_indexer_with_mock(mock_client);
        let mut parent_transactions = HashMap::new();

        assert!(!indexer
            .is_valid_prevout(None, &indexer.wallets.bridge, &mut parent_transactions)
            .await
            .unwrap());
        for outpoint in [OutPoint::new(parent_txid, 1), OutPoint::new(parent_txid, 0)] {
            assert!(!indexer
                .is_valid_prevout(
                    Some(&outpoint),
                    &indexer.wallets.bridge,
                    &mut parent_transactions,
                )
                .await
                .unwrap());
        }
    }

    #[tokio::test]
    async fn coinbase_withdrawal_carrier_is_rejected_without_parent_fetch() {
        let mut coinbase = carrier_only_withdrawal(OutPoint::null(), 0x11);
        coinbase.input[0].script_sig = ScriptBuf::from_bytes(vec![0x01, 0x00]);
        assert!(coinbase.is_coinbase());

        let mut mock_client = MockBitcoinOps::new();
        mock_client
            .expect_fetch_block()
            .with(eq(42_u128))
            .return_once(move |_| Ok(test_block(vec![coinbase])));
        mock_client.expect_get_transaction().times(0);

        assert!(get_indexer_with_mock(mock_client)
            .process_block(42)
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_is_valid_l1_to_l2_transfer() {
        let indexer = get_indexer_with_mock(MockBitcoinOps::new());

        let mut message = L1ToL2Message {
            common: get_test_common_fields(),
            amount: Amount::from_sat(2_000),
            input: types::L1ToL2MessageInput {
                receiver_l2_address: zksync_types::Address::zero(),
                l2_contract_address: zksync_types::Address::zero(),
                call_data: vec![],
            },
            tx_outputs: vec![
                test_output(indexer.wallets.bridge.script_pubkey()),
                test_output(indexer.wallets.bridge.script_pubkey()),
            ],
        };
        assert!(indexer.is_valid_l1_to_l2_transfer(&message));

        message.amount = Amount::ZERO;
        message.tx_outputs = vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new(),
        }];
        assert!(!indexer.is_valid_l1_to_l2_transfer(&message));
    }
}
