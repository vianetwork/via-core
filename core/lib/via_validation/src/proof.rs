use circuit_definitions::{
    circuit_definitions::aux_layer::ZkSyncSnarkWrapperCircuit,
    snark_wrapper::franklin_crypto::bellman::{
        bn256::Bn256,
        plonk::{
            better_better_cs::{
                proof::Proof as ZkSyncProof, setup::VerificationKey, verifier::verify,
            },
            commitments::transcript::keccak_transcript::RollingKeccakTranscript,
        },
    },
};

use crate::{errors::VerificationError, types::Fr};

/// Trait for a proof that can be verified.
pub trait Proof {
    /// Verifies the proof with the given verification key and public inputs.
    fn verify(
        &self,
        verification_key: VerificationKey<Bn256, ZkSyncSnarkWrapperCircuit>,
    ) -> Result<bool, VerificationError>;

    /// Returns the public inputs of the proof.
    fn get_public_inputs(&self) -> &[Fr];
}

/// A struct representing an L1 batch proof.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct L1BatchProof {
    pub aggregation_result_coords: [[u8; 32]; 4],
    pub scheduler_proof: ZkSyncProof<Bn256, ZkSyncSnarkWrapperCircuit>,
    pub inputs: Vec<Fr>,
}

impl Proof for L1BatchProof {
    fn verify(
        &self,
        vk: VerificationKey<Bn256, ZkSyncSnarkWrapperCircuit>,
    ) -> Result<bool, VerificationError> {
        // Ensure the proof's 'n' matches the verification key's 'n'.
        let mut scheduler_proof = self.scheduler_proof.clone();
        scheduler_proof.n = vk.n;

        tracing::debug!("Verifying proof with n = {}", scheduler_proof.n);

        // Verify the proof
        verify::<_, _, RollingKeccakTranscript<_>>(&vk, &scheduler_proof, None)
            .map_err(|_| VerificationError::ProofVerificationFailed)
    }

    fn get_public_inputs(&self) -> &[Fr] {
        &self.inputs
    }
}

impl Default for L1BatchProof {
    fn default() -> Self {
        Self {
            aggregation_result_coords: [[0u8; 32]; 4],
            scheduler_proof: ZkSyncProof::empty(),
            inputs: vec![],
        }
    }
}
