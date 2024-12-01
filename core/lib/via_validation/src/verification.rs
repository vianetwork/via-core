use std::fs;

use circuit_definitions::{
    boojum::pairing::{ff::PrimeFieldRepr, Engine},
    circuit_definitions::aux_layer::ZkSyncSnarkWrapperCircuit,
    snark_wrapper::franklin_crypto::bellman::{
        pairing::bn256::{Bn256, Fq},
        plonk::better_better_cs::{cs::Circuit, setup::VerificationKey},
        CurveAffine, PrimeField,
    },
};
use primitive_types::H256;
use sha3::{Digest, Keccak256};
use tracing::debug;

use crate::{errors::VerificationError, proof::ProofTrait, types::Fr};

/// Verifies a SNARK proof with a given verification key, checking the verification key hash if provided.
/// Returns the public input, auxiliary witness, and computed VK hash on success.
pub async fn verify_snark(
    protocol_version: &str,
    proof: impl ProofTrait,
    vk_hash_from_l1: Option<H256>,
) -> Result<(Fr, H256), VerificationError> {
    debug!("Verifying SNARK wrapped FRI proof.");

    let snark_vk_scheduler_key_file = format!(
        "keys/protocol_version/{}/scheduler_key.json",
        protocol_version
    );
    debug!(
        "Loading verification key from {}",
        snark_vk_scheduler_key_file
    );

    // Load the verification key from the specified file.
    let verification_key_content = fs::read_to_string(snark_vk_scheduler_key_file.clone())
        .map_err(|e| {
            VerificationError::Other(format!(
                "Failed to read verification key from {}: {}",
                snark_vk_scheduler_key_file, e
            ))
        })?;

    // Deserialize the verification key.
    let vk_inner: VerificationKey<Bn256, ZkSyncSnarkWrapperCircuit> =
        serde_json::from_str(&verification_key_content).map_err(|e| {
            VerificationError::Other(format!("Failed to deserialize verification key: {}", e))
        })?;

    // Compute the VK hash and check against the provided hash if any.
    let vk_hash = check_verification_key_hash(&vk_inner, vk_hash_from_l1)?;

    // Verify the proof.
    let is_valid = proof.verify(vk_inner)?;

    if !is_valid {
        return Err(VerificationError::ProofVerificationFailed);
    }

    // Extract the public input from the proof.
    let public_inputs = proof.get_public_inputs();
    let public_input = public_inputs
        .first()
        .cloned()
        .ok_or_else(|| VerificationError::Other("No public inputs found in proof".to_string()))?;

    Ok((public_input, vk_hash))
}

/// Checks that the hash of the verification key matches the supplied hash.
/// Returns the computed VK hash on success.
fn check_verification_key_hash(
    verification_key: &VerificationKey<Bn256, ZkSyncSnarkWrapperCircuit>,
    vk_hash_from_l1: Option<H256>,
) -> Result<H256, VerificationError> {
    if let Some(vk_hash_from_l1) = vk_hash_from_l1 {
        let computed_vk_hash = calculate_verification_key_hash(verification_key);

        debug!("Verification Key Hash Check:");
        debug!(
            "  Verification Key Hash from L1:       0x{}",
            hex::encode(vk_hash_from_l1)
        );
        debug!(
            "  Computed Verification Key Hash:      0x{}",
            hex::encode(computed_vk_hash)
        );

        if computed_vk_hash != vk_hash_from_l1 {
            return Err(VerificationError::VerificationKeyHashMismatch);
        }

        Ok(computed_vk_hash)
    } else {
        debug!("Supplied VK hash is None, skipping check...");
        Ok(H256::default())
    }
}

/// Calculates the hash of a verification key.
fn calculate_verification_key_hash<E: Engine, C: Circuit<E>>(
    verification_key: &VerificationKey<E, C>,
) -> H256 {
    let mut res = Vec::new();

    // Serialize gate setup commitments.
    for gate_setup in &verification_key.gate_setup_commitments {
        let (x, y) = gate_setup.as_xy();
        x.into_repr()
            .write_be(&mut res)
            .expect("Failed to write x coordinate");
        y.into_repr()
            .write_be(&mut res)
            .expect("Failed to write y coordinate");
    }

    // Serialize gate selectors commitments.
    for gate_selector in &verification_key.gate_selectors_commitments {
        let (x, y) = gate_selector.as_xy();
        x.into_repr()
            .write_be(&mut res)
            .expect("Failed to write x coordinate");
        y.into_repr()
            .write_be(&mut res)
            .expect("Failed to write y coordinate");
    }

    // Serialize permutation commitments.
    for permutation in &verification_key.permutation_commitments {
        let (x, y) = permutation.as_xy();
        x.into_repr()
            .write_be(&mut res)
            .expect("Failed to write x coordinate");
        y.into_repr()
            .write_be(&mut res)
            .expect("Failed to write y coordinate");
    }

    // Serialize lookup selector commitment if present.
    if let Some(lookup_selector) = &verification_key.lookup_selector_commitment {
        let (x, y) = lookup_selector.as_xy();
        x.into_repr()
            .write_be(&mut res)
            .expect("Failed to write x coordinate");
        y.into_repr()
            .write_be(&mut res)
            .expect("Failed to write y coordinate");
    }

    // Serialize lookup tables commitments.
    for table_commit in &verification_key.lookup_tables_commitments {
        let (x, y) = table_commit.as_xy();
        x.into_repr()
            .write_be(&mut res)
            .expect("Failed to write x coordinate");
        y.into_repr()
            .write_be(&mut res)
            .expect("Failed to write y coordinate");
    }

    // Serialize table type commitment if present.
    if let Some(lookup_table) = &verification_key.lookup_table_type_commitment {
        let (x, y) = lookup_table.as_xy();
        x.into_repr()
            .write_be(&mut res)
            .expect("Failed to write x coordinate");
        y.into_repr()
            .write_be(&mut res)
            .expect("Failed to write y coordinate");
    }

    // Serialize flag for using recursive part.
    Fq::default()
        .into_repr()
        .write_be(&mut res)
        .expect("Failed to write recursive flag");

    // Compute Keccak256 hash of the serialized data.
    let mut hasher = Keccak256::new();
    hasher.update(&res);
    let computed_vk_hash = hasher.finalize();

    H256::from_slice(&computed_vk_hash)
}
