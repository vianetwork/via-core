use crate::version_28::{
    errors::VerificationError, l1_data_fetcher::L1DataFetcher, proof::ProofTrait, types::Fr,
    utils::load_verification_key,
};

/// Verifies a SNARK proof with a given verification key, checking the verification key hash if provided.
/// Returns the public input, auxiliary witness, and computed VK hash on success.
pub async fn verify_snark<P: ProofTrait, F: L1DataFetcher>(
    l1_data_fetcher: &F,
    proof: P,
    batch_number: u64,
    l1_block_number: u64,
) -> Result<Fr, VerificationError> {
    let vk_inner = load_verification_key(l1_data_fetcher, batch_number, l1_block_number).await?;

    // Verify the proof.
    if !proof.verify(vk_inner)? {
        return Err(VerificationError::ProofVerificationFailed);
    }

    // Extract the public input from the proof.
    let public_inputs = proof.get_public_inputs();
    let public_input = public_inputs
        .first()
        .cloned()
        .ok_or_else(|| VerificationError::Other("No public inputs found in proof".to_string()))?;

    Ok(public_input)
}
