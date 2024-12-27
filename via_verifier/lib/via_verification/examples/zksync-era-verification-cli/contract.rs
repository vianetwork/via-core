use std::str::FromStr;

use async_trait::async_trait;
use ethers::{
    abi::Abi,
    contract::Contract,
    prelude::{Address, Http, Provider},
    types::H256,
};
use tracing::debug;
use via_verification::{
    errors::VerificationError, l1_data_fetcher::L1DataFetcher, proof::ViaZKProof,
    public_inputs::generate_inputs,
};

use crate::fetching::{fetch_batch_protocol_version, fetch_l1_commit_data, fetch_proof_from_l1};

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

    async fn get_proof_from_l1(
        &self,
        batch_number: u64,
    ) -> Result<(ViaZKProof, u64), VerificationError> {
        let protocol_version = self.get_protocol_version(batch_number).await?;
        let protocol_version_id = protocol_version.parse::<u16>().map_err(|_| {
            VerificationError::FetchError("Failed to parse protocol version".to_string())
        })?;
        debug!(
            "Protocol version: {} for batch # {}",
            protocol_version, batch_number
        );

        let (mut proof, block_number) = fetch_proof_from_l1(
            batch_number,
            self.provider.url().as_ref(),
            protocol_version_id,
        )
        .await?;
        let batch_l1_data =
            fetch_l1_commit_data(batch_number, &self.provider.url().to_string()).await?;
        let inputs = generate_inputs(
            &batch_l1_data.prev_batch_commitment,
            &batch_l1_data.curr_batch_commitment,
        );
        fetch_l1_commit_data(batch_number, self.provider.url().as_ref()).await?;
        let inputs = generate_inputs(&batch_l1_data);
        proof.proof.inputs = inputs.clone();

        Ok((proof, block_number))
    }
}
