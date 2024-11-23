use std::str::FromStr;

use async_trait::async_trait;
use ethers::{
    abi::Abi,
    contract::Contract,
    prelude::{Address, Http, Provider},
    types::H256,
};
use via_validator::{
    block_header::{BlockAuxilaryOutput, VerifierParams},
    errors::VerificationError,
    l1_data_fetcher::L1DataFetcher,
    proof::L1BatchProof,
    types::BatchL1Data,
};

use crate::fetching::{
    fetch_batch_commit_tx, fetch_batch_protocol_version, fetch_l1_commit_data, fetch_proof_from_l1,
    fetch_verifier_param_from_l1,
};

pub struct ContractConfig {
    pub provider: Provider<Http>,
    pub diamond_proxy_contract: Contract<Provider<Http>>,
    pub verifier_contract_abi: Abi,
}

impl ContractConfig {
    pub fn new(l1_rpc_url: &str) -> Result<Self, VerificationError> {
        let provider = Provider::<Http>::try_from(l1_rpc_url)
            .map_err(|e| VerificationError::ProviderError(e.to_string()))?;

        let diamond_proxy_abi: Abi = serde_json::from_slice(include_bytes!("abis/IZkSync.json"))
            .map_err(|e| VerificationError::Other(e.to_string()))?;
        let verifier_contract_abi: Abi =
            serde_json::from_slice(include_bytes!("abis/IVerifier.json"))
                .map_err(|e| VerificationError::Other(e.to_string()))?;

        // Diamond proxy contract address on mainnet.
        let diamond_proxy_address = Address::from_str("0x32400084c286cf3e17e7b677ea9583e60a000324")
            .map_err(|e| VerificationError::Other(e.to_string()))?;

        let diamond_proxy_contract =
            Contract::new(diamond_proxy_address, diamond_proxy_abi, provider.clone());

        Ok(Self {
            provider,
            diamond_proxy_contract,
            verifier_contract_abi,
        })
    }
}

#[async_trait]
impl L1DataFetcher for ContractConfig {
    async fn get_verification_key_hash(
        &self,
        block_number: u64,
    ) -> Result<H256, VerificationError> {
        let verifier_address: Address = self
            .diamond_proxy_contract
            .method::<_, Address>("getVerifier", ())?
            .block(block_number)
            .call()
            .await
            .map_err(|e| VerificationError::ContractError(e.to_string()))?;

        let verifier_contract = Contract::new(
            verifier_address,
            self.verifier_contract_abi.clone(),
            self.provider.clone(),
        );

        let vk_hash: H256 = verifier_contract
            .method::<_, H256>("verificationKeyHash", ())?
            .block(block_number)
            .call()
            .await
            .map_err(|e| VerificationError::ContractError(e.to_string()))?;

        Ok(vk_hash)
    }

    async fn get_protocol_version(&self, batch_number: u64) -> Result<String, VerificationError> {
        fetch_batch_protocol_version(batch_number).await
    }

    async fn get_batch_commit_tx_hash(
        &self,
        batch_number: u64,
    ) -> Result<(String, Option<String>), VerificationError> {
        fetch_batch_commit_tx(batch_number).await
    }

    async fn get_l1_commit_data(
        &self,
        batch_number: u64,
    ) -> Result<(BatchL1Data, BlockAuxilaryOutput), VerificationError> {
        let protocol_version = self.get_protocol_version(batch_number).await?;
        let protocol_version_id = protocol_version.parse::<u16>().map_err(|_| {
            VerificationError::FetchError("Failed to parse protocol version".to_string())
        })?;
        fetch_l1_commit_data(
            batch_number,
            protocol_version_id,
            &self.provider.url().to_string(),
        )
        .await
    }

    async fn get_proof_from_l1(
        &self,
        batch_number: u64,
    ) -> Result<(L1BatchProof, u64), VerificationError> {
        let protocol_version = self.get_protocol_version(batch_number).await?;
        let protocol_version_id = protocol_version.parse::<u16>().map_err(|_| {
            VerificationError::FetchError("Failed to parse protocol version".to_string())
        })?;
        fetch_proof_from_l1(
            batch_number,
            &self.provider.url().to_string(),
            protocol_version_id,
        )
        .await
    }

    async fn get_verifier_params(
        &self,
        block_number: u64,
    ) -> Result<VerifierParams, VerificationError> {
        Ok(fetch_verifier_param_from_l1(block_number, &self.provider.url().to_string()).await)
    }
}
