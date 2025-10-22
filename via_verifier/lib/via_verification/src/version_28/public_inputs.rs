use circuit_definitions::snark_wrapper::franklin_crypto::bellman::{
    pairing::bn256::Fr, PrimeField,
};
use ethers::types::U256;
use primitive_types::H256;
use sha3::{Digest, Keccak256};

use crate::version_28::utils::to_fixed_bytes;

/// Computes the public inputs for a given batch commiements.
/// Public inputs require us to fetch multiple data from L1 (like state hash etc).
pub fn generate_inputs(prev_batch_commitment: &H256, curr_batch_commitment: &H256) -> Vec<Fr> {
    // Prepare the input fields
    let input_fields = [
        prev_batch_commitment.to_fixed_bytes(),
        curr_batch_commitment.to_fixed_bytes(),
    ];
    let encoded_input_params = input_fields.into_iter().flatten().collect::<Vec<u8>>();

    // Compute the Keccak256 hash of the input parameters
    let input_keccak_hash = to_fixed_bytes(&Keccak256::digest(&encoded_input_params));
    let input_u256 = U256::from_big_endian(&input_keccak_hash);

    // Shift the input as per the protocol's requirement
    let shifted_input = input_u256 >> 32;

    vec![Fr::from_str(&shifted_input.to_string()).unwrap()]
}
