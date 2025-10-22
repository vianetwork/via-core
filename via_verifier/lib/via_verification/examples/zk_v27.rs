use std::{
    fs::File,
    io::{Read, Write},
};

use via_da_clients::celestia::client::CelestiaClient;
use via_verification::version_27::{
    proof::{ProofTrait, ViaZKProof},
    public_inputs::generate_inputs,
    types::ProveBatches,
    utils::load_verification_key_without_l1_check,
};
use zksync_config::configs::via_secrets::ViaDASecrets;
use zksync_da_client::{types::InclusionData, DataAvailabilityClient};
use zksync_types::url::SensitiveUrl;

// Verify a proof from DA
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Path to your .bin file
    let path = "via_verifier/lib/via_verification/examples/data/data_batch_18_0_27_0.bin";

    // Open the file in read-only mode
    let mut file = File::open(path)?;

    // Create a buffer to hold the file contents
    let mut buffer = Vec::new();

    // Read the entire file into the buffer
    file.read_to_end(&mut buffer)?;

    let proof_blob = InclusionData { data: buffer };
    let proof_data: ProveBatches = bincode::deserialize(&proof_blob.data).unwrap();

    println!("Block number {:?}", &proof_data.l1_batches[0].header.number);

    let vk_inner = load_verification_key_without_l1_check(
        proof_data.l1_batches[0]
            .header
            .protocol_version
            .unwrap()
            .to_string(),
    )
    .await
    .unwrap();

    let (prev_commitment, curr_commitment) = (
        proof_data.prev_l1_batch.metadata.commitment,
        proof_data.l1_batches[0].metadata.commitment,
    );

    let mut proof = proof_data.proofs[0].scheduler_proof.clone();

    // Put correct inputs
    proof.inputs = generate_inputs(&prev_commitment, &curr_commitment);

    // Verify the proof
    let via_proof = ViaZKProof { proof };

    let is_valid = via_proof.verify(vk_inner).unwrap();
    println!("Result {:?}", is_valid);
    Ok(())
}

async fn _fetch_proof_from_da(path: &str, blob_id: &str) -> anyhow::Result<()> {
    let secrets = ViaDASecrets {
        api_node_url: "https://celestia.stage0.viablockchain.dev:26658".parse::<SensitiveUrl>()?,
        auth_token: "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJBbGxvdyI6WyJwdWJsaWMiLCJyZWFkIiwid3JpdGUiLCJhZG1pbiJdfQ.Af2fn0SJfBYtcbGT4ufgvj9lAPT9oGuj1HmepAmktf4".into(),
    };
    let client: Box<dyn DataAvailabilityClient> =
        Box::new(CelestiaClient::new(secrets, 1900000).await?);

    let data = client.get_inclusion_data(blob_id).await?;
    let mut file = File::create_new(path)?;

    file.write(&data.unwrap().data)?;

    Ok(())
}
