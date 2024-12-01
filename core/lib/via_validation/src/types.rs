pub use circuit_definitions::snark_wrapper::franklin_crypto::bellman::bn256::Fr;
use ethers::types::H256;
use serde::{Deserialize, Serialize};

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
