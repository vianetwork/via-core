use std::str::FromStr;

use bitcoin::{
    hashes::Hash,
    key::rand::{self},
    Txid,
};
use chrono::Utc;
use via_btc_client::{
    inscriber::test_utils::{get_mock_inscriber_and_conditions, MockBitcoinOps},
    traits::Serializable,
    types::InscriptionMessage,
};
use zksync_config::ViaBtcSenderConfig;
use zksync_contracts::BaseSystemContractsHashes;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use via_test_utils::utils::test_wallets;
use zksync_types::{
    block::{L1BatchHeader, L1BatchTreeData, L2BlockHasher, L2BlockHeader},
    btc_block::ViaBtcL1BlockDetails,
    btc_inscription_operations::ViaBtcInscriptionRequestType,
    commitment::{L1BatchCommitmentArtifacts, L1BatchMetaParameters, L1BatchMetadata},
    fee_model::BatchFeeInput,
    l2_to_l1_log::{L2ToL1Log, UserL2ToL1Log},
    protocol_version::{L1VerifierConfig, ProtocolSemanticVersion},
    Bloom, L1BatchNumber, L2BlockNumber, ProtocolVersion, ProtocolVersionId, H160, H256,
    via_wallet::SystemWalletsDetails,
};

use crate::{
    aggregated_operations::ViaAggregatedOperation, aggregator::ViaAggregator,
    btc_inscription_manager::ViaBtcInscriptionManager,
};

pub const BOOTLOADER_CODE_HASH_TEST: &str = "010008e79c154523aa30981e598b73c4a33c304bef9c82bae7d2ca4d21daedc7";
pub const DEFAULT_AA_CODE_HASH_TEST: &str = "010005630848b5537f934eea6bd8c61c50648a162b90c82f316454f4109462b1";

pub fn generate_random_bytes(length: usize) -> Vec<u8> {
    let mut bytes: Vec<u8> = vec![];
    for _ in 0..length {
        let number = rand::random::<u8>();
        bytes.push(number);
    }
    bytes
}

/// Creates an L1 batch header with the specified number and deterministic contents.
pub fn create_l1_batch(number: u32) -> L1BatchHeader {
    let mut header = L1BatchHeader::new(
        L1BatchNumber(number),
        number.into(),
        BaseSystemContractsHashes {
            bootloader: H256::from_str(BOOTLOADER_CODE_HASH_TEST).unwrap(),
            default_aa: H256::from_str(DEFAULT_AA_CODE_HASH_TEST).unwrap(),
            evm_emulator: None,
        },
        ProtocolVersionId::latest(),
    );
    header.l1_tx_count = 3;
    header.l2_tx_count = 5;
    header.l2_to_l1_logs.push(UserL2ToL1Log(L2ToL1Log {
        shard_id: 0,
        is_service: false,
        tx_number_in_block: 2,
        sender: H160::random(),
        key: H256::repeat_byte(3),
        value: H256::zero(),
    }));
    header.l2_to_l1_messages.push(vec![22; 22]);
    header.l2_to_l1_messages.push(vec![33; 33]);

    header
}

pub fn default_l1_batch_metadata() -> L1BatchMetadata {
    L1BatchMetadata {
        root_hash: H256::default(),
        rollup_last_leaf_index: 0,
        initial_writes_compressed: Some(vec![]),
        repeated_writes_compressed: Some(vec![]),
        commitment: H256::default(),
        l2_l1_merkle_root: H256::default(),
        block_meta_params: L1BatchMetaParameters {
            zkporter_is_available: false,
            bootloader_code_hash: H256::from_str(BOOTLOADER_CODE_HASH_TEST).unwrap(),
            default_aa_code_hash: H256::from_str(DEFAULT_AA_CODE_HASH_TEST).unwrap(),
            protocol_version: Some(ProtocolVersionId::latest()),
            evm_emulator_code_hash: None,
        },
        aux_data_hash: H256::default(),
        meta_parameters_hash: H256::default(),
        pass_through_data_hash: H256::default(),
        events_queue_commitment: Some(H256::zero()),
        bootloader_initial_content_commitment: Some(H256::zero()),
        state_diffs_compressed: vec![],
        state_diff_hash: Some(H256::default()),
        aggregation_root: Some(H256::zero()),
        da_inclusion_data: None,
        local_root: Some(H256::zero()),
    }
}

pub fn create_btc_l1_batch_details(number: L1BatchNumber, timestamp: i64) -> ViaBtcL1BlockDetails {
    ViaBtcL1BlockDetails {
        number,
        timestamp,
        hash: None,
        blob_id: "blob_id".to_string(),
        commit_tx_id: Txid::all_zeros(),
        reveal_tx_id: Txid::all_zeros(),
        prev_l1_batch_hash: None,
    }
}

pub fn get_btc_sender_config(
    max_aggregated_blocks_to_commit: i32,
    max_aggregated_proofs_to_commit: i32,
) -> ViaBtcSenderConfig {
    ViaBtcSenderConfig {
        max_aggregated_blocks_to_commit,
        max_aggregated_proofs_to_commit,
        ..ViaBtcSenderConfig::for_tests()
    }
}

pub async fn get_inscription_manager_mock(
    pool: ConnectionPool<Core>,
    config: ViaBtcSenderConfig,
    mut mock_btc_ops_config: MockBitcoinOps,
) -> ViaBtcInscriptionManager {
    let chain = via_test_utils::chain::Chain::with_height(
        u32::try_from(mock_btc_ops_config.block_height.max(1)).unwrap(),
    );
    mock_btc_ops_config.headers = chain
        .blocks
        .iter()
        .map(|(&height, block)| via_btc_client::types::BlockHeaderInfo {
            height,
            hash: block.block_hash(),
            parent_hash: block.header.prev_blockhash,
            membership: via_btc_client::types::HeaderMembership::Canonical(
                std::num::NonZeroU32::new(
                    u32::try_from(mock_btc_ops_config.block_height.max(1)).unwrap() - height + 1,
                )
                .unwrap(),
            ),
        })
        .collect();
    mock_btc_ops_config.block = Some(chain.blocks.values().last().unwrap().clone());
    let inscriber = get_mock_inscriber_and_conditions(mock_btc_ops_config);
    let mut wallets = test_wallets();
    wallets.sequencer = inscriber.inscriber_address().unwrap();
    let mut db = pool.connection().await.unwrap();
    let mut tx = db.start_transaction().await.unwrap();
    let tip = *chain.blocks.keys().last().unwrap();
    sqlx::query("UPDATE protocol_patches SET bitcoin_height=0,bitcoin_hash=$1 WHERE bitcoin_payload IS NULL")
        .bind(chain.blocks[&0].block_hash().to_string()).execute(tx.conn()).await.unwrap();
    assert!(via_test_utils::reader_at(&mut tx, &chain, tip, tip, bitcoin::Network::Regtest)
        .await
        .unwrap());
    via_btc_storage::ReaderDal::new(&mut tx)
        .insert_wallets(
            &SystemWalletsDetails::try_from(wallets).unwrap(),
            via_btc_storage::WalletSource::Unbound(0),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    Result::unwrap(ViaBtcInscriptionManager::new(inscriber, pool, config).await)
}

pub struct ViaAggregatorTest {
    pub aggregator: ViaAggregator,
    pub storage: Connection<'static, Core>,
    pub protocol_version: ProtocolVersion,
}

impl ViaAggregatorTest {
    pub async fn new(
        protocol_version: ProtocolVersionId, base_system_contracts_hashes: BaseSystemContractsHashes,
        pool: ConnectionPool<Core>, mut config: Option<ViaBtcSenderConfig>,
    ) -> Self {
        let mut storage = pool.connection().await.unwrap();

        if config.is_none() {
            config = Some(ViaBtcSenderConfig::for_tests());
        }
        let aggregator = ViaAggregator::new(config.unwrap());

        let timestamp = Utc::now().timestamp() as u64;
        let protocol_version = zksync_types::ProtocolVersion {
            l1_verifier_config: L1VerifierConfig { snark_wrapper_vk_hash: H256::random() },
            base_system_contracts_hashes,
            timestamp,
            tx: None,
            version: ProtocolSemanticVersion { minor: protocol_version, patch: 0.into() },
        };

        storage.protocol_versions_dal().save_protocol_version_with_tx(&protocol_version).await.unwrap();

        Self { aggregator, storage, protocol_version }
    }

    pub async fn get_next_ready_operation(&mut self) -> Option<ViaAggregatedOperation> {
        self.aggregator.get_next_ready_operation(&mut self.storage).await.unwrap()
    }

    pub async fn insert_l1_batch(
        &mut self, header: L1BatchHeader, l1_commitment_artifacts: L1BatchCommitmentArtifacts,
    ) {
        self.storage.blocks_dal().insert_mock_l1_batch(&header).await.unwrap();

        self.storage
            .blocks_dal()
            .save_l1_batch_tree_data(
                header.number,
                &L1BatchTreeData { hash: H256::random(), rollup_last_leaf_index: 1 },
            )
            .await
            .unwrap();

        self.storage
            .blocks_dal()
            .save_l1_batch_commitment_artifacts(header.number, &l1_commitment_artifacts)
            .await
            .unwrap();

        let time = Utc::now().naive_utc();

        self.storage
            .via_data_availability_dal()
            .insert_l1_batch_da(header.number, "blob_id", time, 0)
            .await
            .expect("insert_l1_batch_da");

        let random_slice: &[u8] = &generate_random_bytes(32);

        self.storage
            .via_data_availability_dal()
            .save_l1_batch_inclusion_data(header.number, random_slice, 0)
            .await
            .expect("save_l1_batch_inclusion_data");
    }

    pub async fn update_l1_block_for_ready_to_commit_proof(
        &mut self,
        number: L1BatchNumber,
    ) -> (i64, i64) {
        let batch: ViaBtcL1BlockDetails = ViaBtcL1BlockDetails {
            number,
            hash: Some(generate_random_bytes(32)),
            blob_id: "".to_string(),
            commit_tx_id: Txid::from_byte_array(generate_random_bytes(32).try_into().unwrap()),
            reveal_tx_id: Txid::from_byte_array(generate_random_bytes(32).try_into().unwrap()),
            timestamp: 0,
            prev_l1_batch_hash: Some(generate_random_bytes(32)),
        };
        let inscription_message = self
            .aggregator
            .construct_inscription_message(&ViaBtcInscriptionRequestType::CommitL1BatchOnchain, &batch)
            .unwrap();

        let inscription_id = self
            .storage
            .btc_sender_dal()
            .via_save_btc_inscriptions_request(
                batch.number,
                ViaBtcInscriptionRequestType::CommitL1BatchOnchain.to_string(),
                InscriptionMessage::to_bytes(&inscription_message),
                0,
            )
            .await
            .unwrap();

        let inscription_request_history_id = self
            .storage
            .btc_sender_dal()
            .insert_inscription_request_history(
                batch.commit_tx_id.as_byte_array(),
                batch.reveal_tx_id.as_byte_array(),
                inscription_id,
                &generate_random_bytes(32).to_vec(),
                &generate_random_bytes(32).to_vec(),
                0,
                0,
            )
            .await
            .unwrap();

        self.storage
            .via_blocks_dal()
            .insert_l1_batch_inscription_request_id(
                batch.number,
                inscription_id,
                ViaBtcInscriptionRequestType::CommitL1BatchOnchain,
            )
            .await
            .unwrap();
        self.insert_proof_da(batch.number).await;

        (inscription_id, inscription_request_history_id as i64)
    }

    pub async fn insert_proof_da(&mut self, number: L1BatchNumber) {
        self.storage
            .via_data_availability_dal()
            .insert_proof_da(number, "blob_id", Utc::now().naive_utc(), 0)
            .await
            .unwrap();
        self.storage
            .via_data_availability_dal()
            .save_proof_inclusion_data(number, &[1], 0)
            .await
            .unwrap();
    }

    pub async fn confirme_inscription_request(&mut self, inscription_request_id: i64) {
        let inscription_request_history_id = self
            .storage
            .btc_sender_dal()
            .insert_inscription_request_history(
                Txid::from_byte_array(generate_random_bytes(32).try_into().unwrap()).as_byte_array(),
                Txid::from_byte_array(generate_random_bytes(32).try_into().unwrap()).as_byte_array(),
                inscription_request_id,
                &[],
                &[],
                0,
                1,
            )
            .await
            .unwrap();

        self.storage
            .btc_sender_dal()
            .confirm_inscription(inscription_request_id, inscription_request_history_id as i64)
            .await
            .unwrap();
    }

    pub async fn create_genesis_l1_batch(&mut self) -> anyhow::Result<()> {
        let genesis_l1_batch_header = L1BatchHeader::new(
            L1BatchNumber(0),
            0,
            self.protocol_version.base_system_contracts_hashes,
            self.protocol_version.version.minor,
        );

        let genesis_l2_block_header = L2BlockHeader {
            number: L2BlockNumber(0),
            timestamp: 0,
            hash: L2BlockHasher::legacy_hash(L2BlockNumber(0)),
            l1_tx_count: 0,
            l2_tx_count: 0,
            fee_account_address: Default::default(),
            base_fee_per_gas: 0,
            gas_per_pubdata_limit: 0,
            batch_fee_input: BatchFeeInput::l1_pegged(0, 0),
            base_system_contracts_hashes: self.protocol_version.base_system_contracts_hashes,
            protocol_version: Some(self.protocol_version.version.minor),
            virtual_blocks: 0,
            gas_limit: 0,
            logs_bloom: Bloom::zero(),
            pubdata_params: Default::default(),
        };

        let mut transaction = self.storage.start_transaction().await?;

        transaction.protocol_versions_dal().save_protocol_version_with_tx(&self.protocol_version).await?;
        transaction.blocks_dal().insert_mock_l1_batch(&genesis_l1_batch_header).await?;
        transaction.blocks_dal().insert_l2_block(&genesis_l2_block_header).await?;
        transaction.blocks_dal().mark_l2_blocks_as_executed_in_l1_batch(L1BatchNumber(0)).await?;

        let factory_deps = [BOOTLOADER_CODE_HASH_TEST, DEFAULT_AA_CODE_HASH_TEST]
            .iter()
            .map(|c| (H256::from_str(c).unwrap(), vec![]))
            .collect();

        transaction.factory_deps_dal().insert_factory_deps(L2BlockNumber(0), &factory_deps).await.unwrap();

        transaction
            .blocks_dal()
            .save_l1_batch_tree_data(
                L1BatchNumber::from(0),
                &L1BatchTreeData { hash: H256::random(), rollup_last_leaf_index: 1 },
            )
            .await
            .unwrap();

        transaction.commit().await?;
        Ok(())
    }
}
