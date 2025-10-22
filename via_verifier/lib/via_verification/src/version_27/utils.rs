use std::{env, fs, path::PathBuf};

use circuit_definitions::{
    boojum::pairing::bn256::Bn256, circuit_definitions::aux_layer::ZkSyncSnarkWrapperCircuit,
    snark_wrapper::franklin_crypto::bellman::plonk::better_better_cs::cs::VerificationKey,
};
use tracing::debug;
use zksync_types::H256;

use crate::version_27::{
    crypto::calculate_verification_key_hash, errors::VerificationError,
    l1_data_fetcher::L1DataFetcher,
};

/// Load the verification key for a given batch number.
pub async fn load_verification_key<F: L1DataFetcher>(
    l1_data_fetcher: &F,
    batch_number: u64,
    l1_block_number: u64,
) -> Result<VerificationKey<Bn256, ZkSyncSnarkWrapperCircuit>, VerificationError> {
    let protocol_version = l1_data_fetcher.get_protocol_version(batch_number).await?;

    let file_path = format!(
        "keys/protocol_version/{}/scheduler_key.json",
        protocol_version
    );
    let base_dir =
        env::var("CARGO_MANIFEST_DIR").map_err(|e| VerificationError::Other(e.to_string()))?;
    let base_path = PathBuf::from(base_dir);
    let file = base_path.join(&file_path);

    // Load the verification key from the specified file.
    let verification_key_content = fs::read_to_string(file).map_err(|e| {
        VerificationError::Other(format!(
            "Failed to read verification key from {}: {}",
            file_path, e
        ))
    })?;
    let vk_inner: VerificationKey<Bn256, ZkSyncSnarkWrapperCircuit> =
        serde_json::from_str(&verification_key_content).map_err(|e| {
            VerificationError::Other(format!("Failed to deserialize verification key: {}", e))
        })?;

    // Get the verification key hash from L1.
    let vk_hash_from_l1 = l1_data_fetcher
        .get_verification_key_hash(l1_block_number)
        .await?;

    // Calculate the verification key hash from the verification key.
    let computed_vk_hash = calculate_verification_key_hash(&vk_inner);

    // Check that the verification key hash from L1 matches the computed hash.
    debug!("Verification Key Hash Check:");
    debug!(
        "  Verification Key Hash from L1:       0x{}",
        hex::encode(vk_hash_from_l1)
    );
    debug!(
        "  Computed Verification Key Hash:      0x{}",
        hex::encode(computed_vk_hash)
    );

    (computed_vk_hash == vk_hash_from_l1)
        .then_some(vk_inner)
        .ok_or(VerificationError::VerificationKeyHashMismatch)
}

/// Load the verification key for a given batch number.
pub async fn load_verification_key_without_l1_check(
    protocol_version: String,
) -> Result<VerificationKey<Bn256, ZkSyncSnarkWrapperCircuit>, VerificationError> {
    let key_path = match env::var("VIA_VK_KEY_PATH") {
        Ok(path) => {
            let file_path = format!("protocol_version/{}/scheduler_key.json", protocol_version);
            let base_path = PathBuf::from(path);
            base_path.join(&file_path)
        }
        Err(_) => {
            // from VIA_HOME
            let base_dir =
                env::var("VIA_HOME").map_err(|e| VerificationError::Other(e.to_string()))?;
            let base_path = PathBuf::from(base_dir);
            let file_path = format!(
                "via_verifier/lib/via_verification/keys/protocol_version/{}/scheduler_key.json",
                protocol_version
            );
            base_path.join(&file_path)
        }
    };

    // Load the verification key from the specified file.
    let verification_key_content = fs::read_to_string(key_path.clone()).map_err(|e| {
        VerificationError::Other(format!(
            "Failed to read verification key from {:?}: {}",
            key_path, e
        ))
    })?;
    let vk_inner: VerificationKey<Bn256, ZkSyncSnarkWrapperCircuit> =
        serde_json::from_str(&verification_key_content).map_err(|e| {
            VerificationError::Other(format!("Failed to deserialize verification key: {}", e))
        })?;

    Ok(vk_inner)
}

/// Load the verification key for a given batch number.
pub async fn load_verification_key_with_db_check(
    protocol_version: String,
    recursion_scheduler_level_vk_hash: H256,
) -> Result<VerificationKey<Bn256, ZkSyncSnarkWrapperCircuit>, VerificationError> {
    let key_path = match env::var("VIA_VK_KEY_PATH") {
        Ok(path) => {
            let file_path = format!("protocol_version/{}/scheduler_key.json", protocol_version);
            let base_path = PathBuf::from(path);
            base_path.join(&file_path)
        }
        Err(_) => {
            // from VIA_HOME
            let base_dir =
                env::var("VIA_HOME").map_err(|e| VerificationError::Other(e.to_string()))?;
            let base_path = PathBuf::from(base_dir);
            let file_path = format!(
                "via_verifier/lib/via_verification/keys/protocol_version/{}/scheduler_key.json",
                protocol_version
            );
            base_path.join(&file_path)
        }
    };

    // Load the verification key from the specified file.
    let verification_key_content = fs::read_to_string(key_path.clone()).map_err(|e| {
        VerificationError::Other(format!(
            "Failed to read verification key from {:?}: {}",
            key_path, e
        ))
    })?;
    let vk_inner: VerificationKey<Bn256, ZkSyncSnarkWrapperCircuit> =
        serde_json::from_str(&verification_key_content).map_err(|e| {
            VerificationError::Other(format!("Failed to deserialize verification key: {}", e))
        })?;

    // Calculate the verification key hash from the verification key.
    let computed_vk_hash = calculate_verification_key_hash(&vk_inner);

    // Check that the verification key hash from L1 matches the computed hash.
    debug!("Verification Key Hash Check:");
    debug!(
        "  Verification Key Hash from DB:       0x{}",
        hex::encode(recursion_scheduler_level_vk_hash)
    );
    debug!(
        "  Computed Verification Key Hash:      0x{}",
        hex::encode(computed_vk_hash)
    );

    (computed_vk_hash == recursion_scheduler_level_vk_hash)
        .then_some(vk_inner)
        .ok_or(VerificationError::VerificationKeyHashMismatch)
}

pub(crate) fn to_fixed_bytes(ins: &[u8]) -> [u8; 32] {
    let mut result = [0u8; 32];
    result.copy_from_slice(ins);

    result
}
