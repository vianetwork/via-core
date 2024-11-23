pub use circuit_definitions::snark_wrapper::franklin_crypto::bellman::bn256::Fr;
use circuit_definitions::{
    circuit_definitions::aux_layer::ZkSyncSnarkWrapperCircuit,
    snark_wrapper::franklin_crypto::bellman::{
        bn256::Bn256, plonk::better_better_cs::proof::Proof,
    },
};
use ethers::types::H256;
use serde::{Deserialize, Serialize};

use crate::block_header::{BlockAuxilaryOutput, VerifierParams};

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

#[derive(Debug, Clone)]
pub struct L1BatchAndProofData {
    pub batch_l1_data: BatchL1Data,
    pub aux_output: BlockAuxilaryOutput,
    pub scheduler_proof: Proof<Bn256, ZkSyncSnarkWrapperCircuit>,
    pub verifier_params: VerifierParams,
    pub block_number: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VerificationKeyHashJsonOutput {
    pub layer_1_vk_hash: [u8; 32],
    pub computed_vk_hash: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataJsonOutput {
    pub batch_l1_data: BatchL1Data,
    pub aux_input: BlockAuxilaryOutput,
    pub verifier_params: VerifierParams,
    pub verification_key_hash: VerificationKeyHashJsonOutput,
    pub public_input: Fr,
    pub is_proof_valid: bool,
}

impl From<L1BatchAndProofData> for DataJsonOutput {
    fn from(batch: L1BatchAndProofData) -> Self {
        Self {
            batch_l1_data: batch.batch_l1_data,
            aux_input: batch.aux_output,
            verifier_params: batch.verifier_params,
            verification_key_hash: VerificationKeyHashJsonOutput::default(),
            public_input: Fr::default(),
            is_proof_valid: false,
        }
    }
}
