use async_trait::async_trait;
use primitive_types::H256;

use crate::{
    block_header::{BlockAuxilaryOutput, VerifierParams},
    errors::VerificationError,
    proof::L1BatchProof,
    types::BatchL1Data,
};

/// Trait for fetching data from L1 necessary for verification.
#[async_trait]
pub trait L1DataFetcher {
    /// Fetches the verification key hash from L1 for a given block number.
    async fn get_verification_key_hash(&self, block_number: u64)
        -> Result<H256, VerificationError>;

    /// Fetches the protocol version for a given batch number.
    async fn get_protocol_version(&self, batch_number: u64) -> Result<String, VerificationError>;

    /// Fetches batch commit transaction hash for a given batch number.
    async fn get_batch_commit_tx_hash(
        &self,
        batch_number: u64,
    ) -> Result<(String, Option<String>), VerificationError>;

    /// Fetches L1 commit data for a given batch number.
    async fn get_l1_commit_data(
        &self,
        batch_number: u64,
    ) -> Result<(BatchL1Data, BlockAuxilaryOutput), VerificationError>;

    /// Fetches proof data from L1 for a given batch number.
    async fn get_proof_from_l1(
        &self,
        batch_number: u64,
    ) -> Result<(L1BatchProof, u64), VerificationError>;

    /// Fetches verifier parameters from L1 for a given block number.
    async fn get_verifier_params(
        &self,
        block_number: u64,
    ) -> Result<VerifierParams, VerificationError>;
}
