use async_trait::async_trait;
use primitive_types::H256;

use crate::version_28::{errors::VerificationError, proof::ViaZKProof};

/// Trait for fetching data from L1 necessary for verification.
#[async_trait]
pub trait L1DataFetcher {
    /// Fetches the verification key hash from L1 for a given block number.
    async fn get_verification_key_hash(&self, block_number: u64)
        -> Result<H256, VerificationError>;

    /// Fetches the protocol version for a given batch number.
    async fn get_protocol_version(&self, batch_number: u64) -> Result<String, VerificationError>;

    /// Fetches proof data from L1 for a given batch number.
    async fn get_proof_from_l1(
        &self,
        batch_number: u64,
    ) -> Result<(ViaZKProof, u64), VerificationError>;
}
