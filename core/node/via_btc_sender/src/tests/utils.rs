use std::str::FromStr;

use bitcoin::{
    hashes::Hash,
    key::rand::{self},
    Txid,
};
use chrono::Utc;
use via_btc_client::{
    inscriber::test_utils::{get_mock_inscriber_and_conditions, MockBitcoinOpsConfig},
    traits::Serializable,
    types::InscriptionMessage,
};
use zksync_config::{configs::via_btc_sender::ProofSendingMode, ViaBtcSenderConfig};
use zksync_contracts::BaseSystemContractsHashes;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_types::{
    block::{L1BatchHeader, L1BatchTreeData},
    btc_block::ViaBtcL1BlockDetails,
    btc_inscription_operations::ViaBtcInscriptionRequestType,
    commitment::{L1BatchCommitmentArtifacts, L1BatchMetaParameters, L1BatchMetadata},
    l2_to_l1_log::{L2ToL1Log, UserL2ToL1Log},
    protocol_version::{L1VerifierConfig, ProtocolSemanticVersion},
    L1BatchNumber, ProtocolVersion, ProtocolVersionId, H160, H256,
};

use crate::{
    aggregated_operations::ViaAggregatedOperation, aggregator::ViaAggregator,
    btc_inscription_aggregator::ViaBtcInscriptionAggregator,
    btc_inscription_manager::ViaBtcInscriptionManager,
};

pub const BOOTLOADER_CODE_HASH_TEST: &str =
    "010008e74e40a94b1c6e6eb5a1dfbbdbd9eb9e0ec90fd358d29e8c07c30d8491";
pub const DEFAULT_AA_CODE_HASH_TEST: &str =
    "01000563426437b886b132bf5bcf9b0d98c3648f02a6e362893db4345078d09f";

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
        },
        aux_data_hash: H256::default(),
        meta_parameters_hash: H256::default(),
        pass_through_data_hash: H256::default(),
        events_queue_commitment: Some(H256::zero()),
        bootloader_initial_content_commitment: Some(H256::zero()),
        state_diffs_compressed: vec![],
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
        actor_role: "sender".to_string(),
        network: "testnet".to_string(),
        private_key: "0x0".to_string(),
        rpc_password: "password".to_string(),
        rpc_url: "password".to_string(),
        rpc_user: "rpc".to_string(),
        poll_interval: 5000,
        da_identifier: "CELESTIA".to_string(),
        max_aggregated_blocks_to_commit,
        max_aggregated_proofs_to_commit,
        max_txs_in_flight: 1,
        proof_sending_mode: ProofSendingMode::SkipEveryProof,
        block_confirmations: 0,
    }
}

pub async fn get_inscription_aggregator_mock(
    pool: ConnectionPool<Core>,
    config: ViaBtcSenderConfig,
) -> ViaBtcInscriptionAggregator {
    let inscriber = get_mock_inscriber_and_conditions(MockBitcoinOpsConfig::default());
    Result::unwrap(ViaBtcInscriptionAggregator::new(inscriber, pool, config).await)
}

pub async fn get_inscription_manager_mock(
    pool: ConnectionPool<Core>,
    config: ViaBtcSenderConfig,
    mock_btc_ops_config: MockBitcoinOpsConfig,
) -> ViaBtcInscriptionManager {
    let inscriber = get_mock_inscriber_and_conditions(mock_btc_ops_config);
    Result::unwrap(ViaBtcInscriptionManager::new(inscriber, pool, config).await)
}

pub struct ViaAggregatorTest {
    pub aggregator: ViaAggregator,
    pub storage: Connection<'static, Core>,
    pub protocol_version: ProtocolVersion,
}

impl ViaAggregatorTest {
    pub async fn new(
        protocol_version: ProtocolVersionId,
        base_system_contracts_hashes: BaseSystemContractsHashes,
        pool: ConnectionPool<Core>,
        mut config: Option<ViaBtcSenderConfig>,
    ) -> Self {
        let mut storage = pool.connection().await.unwrap();

        if config.is_none() {
            config = Some(ViaBtcSenderConfig::for_tests());
        }
        let aggregator = ViaAggregator::new(config.unwrap());

        let timestamp = Utc::now().timestamp() as u64;
        let protocol_version = zksync_types::ProtocolVersion {
            l1_verifier_config: L1VerifierConfig {
                recursion_scheduler_level_vk_hash: H256::random(),
            },
            base_system_contracts_hashes,
            timestamp,
            tx: None,
            version: ProtocolSemanticVersion {
                minor: protocol_version,
                patch: 0.into(),
            },
        };

        storage
            .protocol_versions_dal()
            .save_protocol_version_with_tx(&protocol_version)
            .await
            .unwrap();

        Self {
            aggregator,
            storage,
            protocol_version,
        }
    }

    pub async fn get_next_ready_operation(&mut self) -> Option<ViaAggregatedOperation> {
        self.aggregator
            .get_next_ready_operation(
                &mut self.storage,
                self.protocol_version.base_system_contracts_hashes,
                self.protocol_version.version.minor,
            )
            .await
            .unwrap()
    }

    pub async fn insert_l1_batch(
        &mut self,
        header: L1BatchHeader,
        l1_commitment_artifacts: L1BatchCommitmentArtifacts,
    ) {
        self.storage
            .blocks_dal()
            .insert_mock_l1_batch(&header)
            .await
            .unwrap();

        self.storage
            .blocks_dal()
            .save_l1_batch_tree_data(
                header.number,
                &L1BatchTreeData {
                    hash: H256::random(),
                    rollup_last_leaf_index: 1,
                },
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
            .insert_l1_batch_da(header.number, "blob_id", time)
            .await
            .expect("insert_l1_batch_da");

        let random_slice: &[u8] = &generate_random_bytes(32);

        self.storage
            .via_data_availability_dal()
            .save_l1_batch_inclusion_data(header.number, random_slice)
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
            .construct_inscription_message(
                &ViaBtcInscriptionRequestType::CommitL1BatchOnchain,
                &batch,
            )
            .unwrap();

        let inscription = self
            .storage
            .btc_sender_dal()
            .via_save_btc_inscriptions_request(
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
                batch.commit_tx_id,
                batch.reveal_tx_id,
                inscription.id,
                generate_random_bytes(32),
                generate_random_bytes(32),
                0,
                0,
            )
            .await
            .unwrap()
            .unwrap();

        self.storage
            .via_blocks_dal()
            .insert_l1_batch_inscription_request_id(
                batch.number,
                inscription.id,
                ViaBtcInscriptionRequestType::CommitL1BatchOnchain,
            )
            .await
            .unwrap();
        let sent_at = Utc::now().naive_utc();

        let _ = self
            .storage
            .via_data_availability_dal()
            .insert_proof_da(batch.number, "blob_id", sent_at)
            .await;

        (inscription.id, inscription_request_history_id as i64)
    }

    pub async fn confirme_inscription_request(&mut self, inscription_request_id: i64) {
        let inscription_request_history_id = self
            .storage
            .btc_sender_dal()
            .insert_inscription_request_history(
                Txid::from_byte_array(generate_random_bytes(32).try_into().unwrap()),
                Txid::from_byte_array(generate_random_bytes(32).try_into().unwrap()),
                inscription_request_id,
                vec![],
                vec![],
                0,
                1,
            )
            .await
            .unwrap()
            .unwrap();

        self.storage
            .btc_sender_dal()
            .confirm_inscription(
                inscription_request_id,
                inscription_request_history_id as i64,
            )
            .await
            .unwrap();
    }
}
