use std::str::FromStr;

use zksync_types::H256;

use crate::{
    version_28::types::ProveBatches,
    version_28::{
        proof::{ProofTrait, ViaZKProof},
        public_inputs::generate_inputs,
        // types::ProveBatches,
        utils::load_verification_key_with_db_check,
    },
};

pub mod crypto;
pub mod errors;
pub mod l1_data_fetcher;
pub mod proof;
pub mod public_inputs;
pub mod types;
pub mod utils;
pub mod verification;

pub async fn verify_proof(proof_data: ProveBatches) -> anyhow::Result<bool> {
    // `should_verify` stays decodable for old packages but never selects the outcome:
    // the external-node DA path rewrites it from the serving node's current config.
    let [batch] = proof_data.l1_batches.as_slice() else {
        anyhow::bail!(
            "Expected exactly one L1 batch, got {}",
            proof_data.l1_batches.len()
        );
    };
    let [wrapped_proof] = proof_data.proofs.as_slice() else {
        anyhow::bail!(
            "Expected exactly one proof, got {}",
            proof_data.proofs.len()
        );
    };

    let recursion_scheduler_level_vk_hash: H256 =
        H256::from_str("14f97b81e54b35fe673d8708cc1a19e1ea5b5e348e12d31e39824ed4f42bbca2")?;
    let protocol_version_id = batch
        .header
        .protocol_version
        .ok_or_else(|| anyhow::anyhow!("Protocol version is missing"))?;

    tracing::info!(
        "Recursion_scheduler_level_vk_hash {}, protocol_version_id {}",
        recursion_scheduler_level_vk_hash,
        protocol_version_id
    );

    let vk_inner = load_verification_key_with_db_check(
        protocol_version_id.to_string(),
        recursion_scheduler_level_vk_hash,
    )
    .await?;

    let mut proof = wrapped_proof.scheduler_proof.clone();
    proof.inputs = generate_inputs(
        &proof_data.prev_l1_batch.metadata.commitment,
        &batch.metadata.commitment,
    );
    let is_valid = ViaZKProof { proof }.verify(vk_inner)?;

    tracing::info!("Proof verification result: {}", is_valid);

    Ok(is_valid)
}
