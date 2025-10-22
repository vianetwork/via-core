use circuit_definitions::snark_wrapper::franklin_crypto::bellman::plonk::{
    better_better_cs::{setup::VerificationKey, verifier::verify},
    commitments::transcript::keccak_transcript::RollingKeccakTranscript,
};
// Re-export the necessary types from the `circuit_definitions` crate.
pub use circuit_definitions::{
    circuit_definitions::aux_layer::ZkSyncSnarkWrapperCircuit,
    snark_wrapper::franklin_crypto::bellman::{
        bn256::Bn256, plonk::better_better_cs::proof::Proof as ZkSyncProof,
    },
};

use crate::version_27::{errors::VerificationError, types::Fr};

/// Trait for a proof that can be verified.
pub trait ProofTrait {
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
pub struct ViaZKProof {
    pub proof: ZkSyncProof<Bn256, ZkSyncSnarkWrapperCircuit>,
}

impl ProofTrait for ViaZKProof {
    fn verify(
        &self,
        vk: VerificationKey<Bn256, ZkSyncSnarkWrapperCircuit>,
    ) -> Result<bool, VerificationError> {
        // Ensure the proof's 'n' matches the verification key's 'n'.
        let mut scheduler_proof = self.proof.clone();
        scheduler_proof.n = vk.n;

        tracing::debug!("Verifying proof with n = {}", scheduler_proof.n);

        // Verify the proof
        verify::<_, _, RollingKeccakTranscript<_>>(&vk, &scheduler_proof, None)
            .map_err(|_| VerificationError::ProofVerificationFailed)
    }

    fn get_public_inputs(&self) -> &[Fr] {
        &self.proof.inputs
    }
}

impl Default for ViaZKProof {
    fn default() -> Self {
        Self {
            proof: ZkSyncProof::empty(),
        }
    }
}
