use std::{
    fs::File,
    io::{self, Read},
};

use serde::{Deserialize, Serialize};
use via_verification::proof::{
    Bn256, ProofTrait, ViaZKProof, ZkSyncProof, ZkSyncSnarkWrapperCircuit,
};
use zksync_da_client::types::InclusionData;
use zksync_types::{
    commitment::L1BatchWithMetadata, protocol_version::ProtocolSemanticVersion, H256,
};

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

// Verify a proof from Blob
#[tokio::main]
async fn main() -> io::Result<()> {
    // via-prover-blob-store-dev/proofs_fri/l1_batch_proof_{number}_{version}.bin (example: .../l1_batch_proof_1_0_26_0.bin)
    let path = "proofs_fri_l1_batch_proof_1_0_26_0.bin";
    let protocol_version: String = "26".into();

    // Get the l1 batch commitments: select number, encode(commitment::bytea, 'hex') from l1_batches order by number
    let prev_commitment = H256::from_slice(
        &hex::decode("e81e1a4727269fe1ef3e2f8c3f5cfb9aab7c073722c278331b7e017033c13f8f").unwrap(),
    );
    let curr_commitment = H256::from_slice(
        &hex::decode("2663276f8fdc1e9e29c0ca9d225efc6e6fdefccfb01f92e8c645666a9c271f40").unwrap(),
    );

    // Open the file in read-only mode
    let mut file = File::open(path)?;

    // Create a buffer to hold the file contents
    let mut buffer = Vec::new();

    // Read the entire file into the buffer
    file.read_to_end(&mut buffer)?;

    let proof_blob = InclusionData { data: buffer };
    let proof: L1BatchProofForL1 = bincode::deserialize(&proof_blob.data).unwrap();

    let vk_inner =
        via_verification::utils::load_verification_key_without_l1_check(protocol_version)
            .await
            .unwrap();

    let mut proof = proof.scheduler_proof.clone();

    // Put correct inputs
    proof.inputs =
        via_verification::public_inputs::generate_inputs(&prev_commitment, &curr_commitment);

    // Verify the proof
    let via_proof = ViaZKProof { proof };

    let is_valid = via_proof.verify(vk_inner).unwrap();
    println!("Result {:?}", is_valid);
    Ok(())
}
