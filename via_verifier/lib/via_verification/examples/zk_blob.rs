use std::{
    collections::HashMap,
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
    // Get the l1 batch commitments: select number, encode(commitment::bytea, 'hex') from l1_batches order by number
    let mut commitments: HashMap<u8, &str> = HashMap::new();

    commitments.insert(
        0,
        "e81e1a4727269fe1ef3e2f8c3f5cfb9aab7c073722c278331b7e017033c13f8f",
    );
    commitments.insert(
        1,
        "2663276f8fdc1e9e29c0ca9d225efc6e6fdefccfb01f92e8c645666a9c271f40",
    );
    commitments.insert(
        2,
        "a93829edb5306e2a57033ab33dfd30e00fe1165e5b9f217adfcaab47464200c7",
    );
    commitments.insert(
        3,
        "380c8ffbb37556b479562026f2cb9e86edc1e28442e4933473c902ca82f59524",
    );
    commitments.insert(
        4,
        "5c77f6fee5c2aa50797e60b05084408e98ae02c9612e3465f28d88803483dc52",
    );
    commitments.insert(
        5,
        "155958f4f0f5958b56243e68a77d4963b13a2913d9ad12bb39a234d7c51ace1a",
    );
    commitments.insert(
        6,
        "f0bd206237151a1a7054f42ea9f3ca9fc5c409004cfb6cddff0d42dd93dbc947",
    );
    commitments.insert(
        7,
        "18f7b4a0c967ae093af4e1f12fe69e985252492436dcfc173f3450681f7241db",
    );
    commitments.insert(
        8,
        "0a1ef4627b55144f698b39f998c44422679546f324503633e4a4c3a13a93cbd1",
    );
    commitments.insert(
        9,
        "afa26666d372fec9341db534b243d335e26bf13c3d48d5e7e3b498e545694802",
    );
    commitments.insert(
        10,
        "130673e6cfef078c68a4d0c806817ddb74333d6161495ee00af88e718d227405",
    );
    commitments.insert(
        11,
        "4d13aa5d3b36e95a7d5f089075aa4f0d6cc92204499954b451dd8ec6653189a4",
    );
    let protocol_version: String = "26".into();

    for i in 1..=11 {
        let prev_commitment =
            H256::from_slice(&hex::decode(commitments.get(&(i - 1)).unwrap()).unwrap());
        let curr_commitment =
            H256::from_slice(&hex::decode(commitments.get(&(i)).unwrap()).unwrap());

        // via-prover-blob-store-dev/proofs_fri/l1_batch_proof_{number}_{version}.bin (example: .../l1_batch_proof_1_0_26_0.bin)
        let path = format!("proofs_fri_l1_batch_proof_{}_0_26_0.bin", i);
        // Open the file in read-only mode
        let mut file = File::open(path)?;

        // Create a buffer to hold the file contents
        let mut buffer = Vec::new();

        // Read the entire file into the buffer
        file.read_to_end(&mut buffer)?;

        let proof_blob = InclusionData { data: buffer };
        let proof: L1BatchProofForL1 = bincode::deserialize(&proof_blob.data).unwrap();

        let vk_inner = via_verification::utils::load_verification_key_without_l1_check(
            protocol_version.clone(),
        )
        .await
        .unwrap();
        let mut proof = proof.scheduler_proof.clone();

        // Put correct inputs
        proof.inputs =
            via_verification::public_inputs::generate_inputs(&prev_commitment, &curr_commitment);

        // Verify the proof
        let via_proof = ViaZKProof { proof };

        let is_valid = via_proof.verify(vk_inner).unwrap();
        println!("Result batch {i} -> {:?}", is_valid);
    }

    Ok(())
}
