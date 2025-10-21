use std::str::FromStr;

use zksync_types::H256;

use crate::version_27::{
    proof::{ProofTrait, ViaZKProof},
    public_inputs::generate_inputs,
    types::ProveBatches,
    utils::load_verification_key_with_db_check,
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
    let recursion_scheduler_level_vk_hash: H256 =
        H256::from_str("14f97b81e54b35fe673d8708cc1a19e1ea5b5e348e12d31e39824ed4f42bbca2")?;

    // let proof_data: ProveBatches = bincode::deserialize(data)?;

    let protocol_version_id = proof_data.l1_batches[0]
        .header
        .protocol_version
        .ok_or_else(|| anyhow::anyhow!("Protocol version is missing"))?;

    tracing::info!(
        "Recursion_scheduler_level_vk_hash {}, protocol_version_id {}",
        recursion_scheduler_level_vk_hash,
        protocol_version_id
    );

    if proof_data.l1_batches.len() != 1 {
        tracing::error!(
            "Expected exactly one L1Batch and one proof, got {} and {}",
            proof_data.l1_batches.len(),
            proof_data.proofs.len()
        );
        return Ok(false);
    }

    let vk_inner = load_verification_key_with_db_check(
        protocol_version_id.to_string(),
        recursion_scheduler_level_vk_hash,
    )
    .await?;

    tracing::info!(
        "Found valid recursion_scheduler_level_vk_hash {}",
        recursion_scheduler_level_vk_hash,
    );

    if !proof_data.should_verify {
        tracing::info!(
            "Proof verification is disabled for proof with batch number : {:?}",
            proof_data.l1_batches[0].header.number
        );
        return Ok(true);
    } else {
        if proof_data.proofs.len() != 1 {
            tracing::error!(
                "Expected exactly one proof, got {}",
                proof_data.proofs.len()
            );
            return Ok(false);
        }

        let (prev_commitment, curr_commitment) = (
            proof_data.prev_l1_batch.metadata.commitment,
            proof_data.l1_batches[0].metadata.commitment,
        );
        let mut proof = proof_data.proofs[0].scheduler_proof.clone();

        // Put correct inputs
        proof.inputs = generate_inputs(&prev_commitment, &curr_commitment);

        // Verify the proof
        let via_proof = ViaZKProof { proof };

        let is_valid = via_proof.verify(vk_inner)?;

        tracing::info!("Proof verification result: {}", is_valid);

        Ok(is_valid)
    }
}
