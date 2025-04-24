use std::{
    fs::File,
    io::{self, Read},
};

use serde::{Deserialize, Serialize};
use via_verification::proof::{
    Bn256, ProofTrait, ViaZKProof, ZkSyncProof, ZkSyncSnarkWrapperCircuit,
};
use zksync_da_client::types::InclusionData;
use zksync_types::{commitment::L1BatchWithMetadata, protocol_version::ProtocolSemanticVersion};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProveBatches {
    pub prev_l1_batch: L1BatchWithMetadata,
    pub l1_batches: Vec<L1BatchWithMetadata>,
    pub proofs: Vec<L1BatchProofForL1>,
    pub should_verify: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L1BatchProofForL1 {
    pub aggregation_result_coords: [[u8; 32]; 4],
    pub scheduler_proof: ZkSyncProof<Bn256, ZkSyncSnarkWrapperCircuit>,
    pub protocol_version: ProtocolSemanticVersion,
}

// Verify a proof from DA
#[tokio::main]
async fn main() -> io::Result<()> {
    // Path to your .bin file
    let path = "data_batch_10.bin";

    // Open the file in read-only mode
    let mut file = File::open(path)?;

    // Create a buffer to hold the file contents
    let mut buffer = Vec::new();

    // Read the entire file into the buffer
    file.read_to_end(&mut buffer)?;

    let proof_blob = InclusionData { data: buffer };
    let proof_data: ProveBatches = bincode::deserialize(&proof_blob.data).unwrap();

    println!("Block number {:?}", &proof_data.l1_batches[0].header.number);

    let vk_inner = via_verification::utils::load_verification_key_without_l1_check(
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

    println!("{:?}", &prev_commitment);
    println!("{:?}", &curr_commitment);
    let mut proof = proof_data.proofs[0].scheduler_proof.clone();

    // Put correct inputs
    proof.inputs =
        via_verification::public_inputs::generate_inputs(&prev_commitment, &curr_commitment);

    // Verify the proof
    let via_proof = ViaZKProof { proof };

    let is_valid = via_proof.verify(vk_inner).unwrap();
    println!("Result {:?}", is_valid);
    Ok(())
}
