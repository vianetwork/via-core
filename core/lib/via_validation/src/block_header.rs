use ethers::{
    abi::{Function, Token},
    utils::keccak256,
};
use serde::{Deserialize, Serialize};
use zksync_types::{commitment::SerializeCommitment, l2_to_l1_log::L2ToL1Log, H256};

use crate::{errors::VerificationError, utils::to_fixed_bytes};

/// Represents auxiliary output data extracted from a block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockAuxilaryOutput {
    pub system_logs_hash: [u8; 32],
    pub state_diff_hash: [u8; 32],
    pub bootloader_heap_initial_content_hash: [u8; 32],
    pub event_queue_state_hash: [u8; 32],
}

impl BlockAuxilaryOutput {
    /// Flattens the struct fields into a single byte vector.
    pub fn into_flattened_bytes(&self) -> Vec<u8> {
        let mut result = Vec::with_capacity(128);
        result.extend_from_slice(&self.system_logs_hash);
        result.extend_from_slice(&self.state_diff_hash);
        result.extend_from_slice(&self.bootloader_heap_initial_content_hash);
        result.extend_from_slice(&self.event_queue_state_hash);
        result
    }

    /// Prepares the aggregation result coordinates.
    pub fn prepare_aggregation_result_coords(&self) -> [[u8; 32]; 4] {
        [
            self.system_logs_hash,
            self.state_diff_hash,
            self.bootloader_heap_initial_content_hash,
            self.event_queue_state_hash,
        ]
    }
}

/// Parses auxiliary data from the given calldata using the provided function ABI.
pub fn parse_aux_data(
    func: &Function,
    calldata: &[u8],
) -> Result<BlockAuxilaryOutput, VerificationError> {
    if calldata.len() < 5 {
        return Err(VerificationError::FetchError(
            "Calldata is too short".to_string(),
        ));
    }

    let mut parsed_calldata = func
        .decode_input(&calldata[4..])
        .map_err(|e| VerificationError::FetchError(e.to_string()))?;

    let committed_batch = parsed_calldata.pop().ok_or_else(|| {
        VerificationError::FetchError("Failed to deconstruct committed batch".to_string())
    })?;

    let committed_batch = if let Token::Array(committed_batch) = committed_batch {
        committed_batch
    } else {
        return Err(VerificationError::FetchError(
            "Failed to deconstruct committed batch".to_string(),
        ));
    };

    let committed_batch = if let Token::Tuple(ref tuple) = committed_batch[0] {
        tuple
    } else {
        return Err(VerificationError::FetchError(
            "Failed to deconstruct committed batch".to_string(),
        ));
    };

    if committed_batch.len() != 10 {
        return Err(VerificationError::FetchError(
            "Unexpected committed batch format".to_string(),
        ));
    }

    let bootloader_contents_hash = if let Token::FixedBytes(bytes) = &committed_batch[6] {
        bytes
    } else {
        return Err(VerificationError::FetchError(
            "Failed to extract bootloader_contents_hash".to_string(),
        ));
    };

    let event_queue_state_hash = if let Token::FixedBytes(bytes) = &committed_batch[7] {
        bytes
    } else {
        return Err(VerificationError::FetchError(
            "Failed to extract event_queue_state_hash".to_string(),
        ));
    };

    let sys_logs = if let Token::Bytes(bytes) = &committed_batch[8] {
        bytes
    } else {
        return Err(VerificationError::FetchError(
            "Failed to extract sys_logs".to_string(),
        ));
    };

    if bootloader_contents_hash.len() != 32 || event_queue_state_hash.len() != 32 {
        return Err(VerificationError::FetchError(
            "Invalid hash length in committed batch".to_string(),
        ));
    }

    let bootloader_contents_hash_buffer = to_fixed_bytes(bootloader_contents_hash);
    let event_queue_state_hash_buffer = to_fixed_bytes(event_queue_state_hash);

    if sys_logs.len() % L2ToL1Log::SERIALIZED_SIZE != 0 {
        return Err(VerificationError::FetchError(
            "sys_logs length is not a multiple of L2ToL1Log::SERIALIZED_SIZE".to_string(),
        ));
    }

    let state_diff_hash_sys_log = sys_logs
        .chunks(L2ToL1Log::SERIALIZED_SIZE)
        .map(L2ToL1Log::from_slice)
        .find(|log| log.key == H256::from_low_u64_be(2_u64))
        .ok_or_else(|| {
            VerificationError::FetchError("Failed to find state_diff_hash in sys_logs".to_string())
        })?;

    let system_logs_hash = keccak256(sys_logs);

    Ok(BlockAuxilaryOutput {
        system_logs_hash,
        state_diff_hash: to_fixed_bytes(state_diff_hash_sys_log.value.as_bytes()),
        bootloader_heap_initial_content_hash: bootloader_contents_hash_buffer,
        event_queue_state_hash: event_queue_state_hash_buffer,
    })
}

/// Verifier config params describing the circuit to be verified.
#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub struct VerifierParams {
    pub recursion_node_level_vk_hash: [u8; 32],
    pub recursion_leaf_level_vk_hash: [u8; 32],
    pub recursion_circuits_set_vk_hash: [u8; 32],
}
