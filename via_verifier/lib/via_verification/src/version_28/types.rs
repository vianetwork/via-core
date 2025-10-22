pub use circuit_definitions::snark_wrapper::franklin_crypto::bellman::bn256::Fr;
use circuit_definitions::{
    circuit_definitions::aux_layer::ZkSyncSnarkWrapperCircuit,
    snark_wrapper::franklin_crypto::bellman::{
        bn256::Bn256, plonk::better_better_cs::proof::Proof as ZkSyncProof,
    },
};
use serde::{Deserialize, Serialize};
use zksync_types::{
    protocol_version::ProtocolSemanticVersion, Address, Bloom, L1BatchNumber, ProtocolVersionId,
    H256, U256,
};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BatchL1Data {
    pub previous_enumeration_counter: u64,
    pub previous_root: Vec<u8>,
    // Enumeration counter (used for L2 -> L1 communication).
    pub new_enumeration_counter: u64,
    // Storage root.
    pub new_root: Vec<u8>,
    // Hash of the account abstraction code.
    pub default_aa_hash: [u8; 32],
    // Hash of the bootloader.yul code.
    pub bootloader_hash: [u8; 32],
    pub prev_batch_commitment: H256,
    pub curr_batch_commitment: H256,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L1BatchWithMetadata {
    pub header: L1BatchHeader,
    pub metadata: L1BatchMetadata,
    pub raw_published_factory_deps: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct BaseSystemContractsHashes {
    pub bootloader: H256,
    pub default_aa: H256,
    pub evm_emulator: Option<H256>,
}

/// Holder for the block metadata that is not available from transactions themselves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L1BatchHeader {
    /// Numeric ID of the block. Starts from 1, 0 block is considered genesis block and has no transactions.
    pub number: L1BatchNumber,
    /// Timestamp when block was first created.
    pub timestamp: u64,
    /// Total number of processed priority operations in the block
    pub l1_tx_count: u16,
    /// Total number of processed txs that was requested offchain
    pub l2_tx_count: u16,
    /// The data of the processed priority operations hash which must be sent to the smart contract.
    pub priority_ops_onchain_data: Vec<PriorityOpOnchainData>,
    /// All user generated L2 -> L1 logs in the block.
    pub l2_to_l1_logs: Vec<UserL2ToL1Log>,
    /// Preimages of the hashes that were sent as value of L2 logs by special system L2 contract.
    pub l2_to_l1_messages: Vec<Vec<u8>>,
    /// Bloom filter for the event logs in the block.
    pub bloom: Bloom,
    /// Hashes of contracts used this block
    pub used_contract_hashes: Vec<U256>,
    pub base_system_contracts_hashes: BaseSystemContractsHashes,
    /// System logs are those emitted as part of the Vm execution.
    pub system_logs: Vec<SystemL2ToL1Log>,
    /// Version of protocol used for the L1 batch.
    pub protocol_version: Option<ProtocolVersionId>,
    pub pubdata_input: Option<Vec<u8>>,
    pub fee_address: Address,
}

/// Precalculated data for the L1 batch that was used in commitment and L1 transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L1BatchMetadata {
    pub root_hash: H256,
    pub rollup_last_leaf_index: u64,
    pub initial_writes_compressed: Option<Vec<u8>>,
    pub repeated_writes_compressed: Option<Vec<u8>>,
    pub commitment: H256,
    pub l2_l1_merkle_root: H256,
    pub block_meta_params: L1BatchMetaParameters,
    pub aux_data_hash: H256,
    pub meta_parameters_hash: H256,
    pub pass_through_data_hash: H256,
    /// The commitment to the final events queue state after the batch is committed.
    /// Practically, it is a commitment to all events that happened on L2 during the batch execution.
    pub events_queue_commitment: Option<H256>,
    /// The commitment to the initial heap content of the bootloader. Practically it serves as a
    /// commitment to the transactions in the batch.
    pub bootloader_initial_content_commitment: Option<H256>,
    pub state_diffs_compressed: Vec<u8>,
    /// Hash of packed state diffs. It's present only for post-gateway batches.
    pub state_diff_hash: Option<H256>,
    /// Root hash of the local logs tree. Tree contains logs that were produced on this chain.
    /// It's present only for post-gateway batches.
    pub local_root: Option<H256>,
    /// Root hash of the aggregated logs tree. Tree aggregates `local_root`s of chains that settle on this chain.
    /// It's present only for post-gateway batches.
    pub aggregation_root: Option<H256>,
    /// Data Availability inclusion proof, that has to be verified on the settlement layer.
    pub da_inclusion_data: Option<Vec<u8>>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ProveBatches {
    pub prev_l1_batch: L1BatchWithMetadata,
    pub l1_batches: Vec<L1BatchWithMetadata>,
    pub proofs: Vec<L1BatchProofForL1>,
    pub should_verify: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct L1BatchProofForL1 {
    pub aggregation_result_coords: [[u8; 32]; 4],
    pub scheduler_proof: ZkSyncProof<Bn256, ZkSyncSnarkWrapperCircuit>,
    pub protocol_version: ProtocolSemanticVersion,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriorityOpOnchainData {
    pub layer_2_tip_fee: U256,
    pub onchain_data_hash: H256,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, Eq)]
pub struct L2ToL1Log {
    pub shard_id: u8,
    pub is_service: bool,
    pub tx_number_in_block: u16,
    pub sender: Address,
    pub key: H256,
    pub value: H256,
}

/// A struct representing a "user" L2->L1 log, i.e. the one that has been emitted by using the L1Messenger.
/// It is identical to the SystemL2ToL1Log struct, but
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, Eq)]
pub struct UserL2ToL1Log(pub L2ToL1Log);

/// A struct representing a "user" L2->L1 log, i.e. the one that has been emitted by using the L1Messenger.
/// It is identical to the SystemL2ToL1Log struct, but

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, Eq)]
pub struct SystemL2ToL1Log(pub L2ToL1Log);

/// Meta parameters for an L1 batch. They are the same for each L1 batch per run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L1BatchMetaParameters {
    pub zkporter_is_available: bool,
    pub bootloader_code_hash: H256,
    pub default_aa_code_hash: H256,
    pub evm_emulator_code_hash: Option<H256>,
    pub protocol_version: Option<ProtocolVersionId>,
}
