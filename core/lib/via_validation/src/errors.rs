use ethers::abi::ethabi;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum VerificationError {
    #[error("Network not supported")]
    UnsupportedNetwork,

    #[error("Failed to fetch data: {0}")]
    FetchError(String),

    #[error("Verification key hash mismatch")]
    VerificationKeyHashMismatch,

    #[error("Proof verification failed")]
    ProofVerificationFailed,

    #[error("Invalid proof")]
    InvalidProof,

    #[error("Abi error: {0}")]
    AbiError(ethers::contract::AbiError),

    #[error("Provider error: {0}")]
    ProviderError(String),

    #[error("Contract error: {0}")]
    ContractError(String),

    #[error("Other error: {0}")]
    Other(String),
}

impl From<reqwest::Error> for VerificationError {
    fn from(e: reqwest::Error) -> Self {
        VerificationError::FetchError(e.to_string())
    }
}

impl From<ethers::providers::ProviderError> for VerificationError {
    fn from(e: ethers::providers::ProviderError) -> Self {
        VerificationError::FetchError(e.to_string())
    }
}

impl From<ethabi::Error> for VerificationError {
    fn from(e: ethabi::Error) -> Self {
        VerificationError::Other(e.to_string())
    }
}

impl From<ethers::contract::AbiError> for VerificationError {
    fn from(e: ethers::contract::AbiError) -> Self {
        VerificationError::AbiError(e)
    }
}
