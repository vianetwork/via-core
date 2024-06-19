use std::fmt::{Debug, Formatter};

use async_trait::async_trait;
use celestia_rpc::{BlobClient, Client};
use celestia_types::{blob::GasPrice, nmt::Namespace, Blob};
use via_zksync_da_client::{types, DataAvailabilityClient};
use zksync_config::configs::clients::CelestiaConfig;
use zksync_env_config::FromEnv;

#[derive(Clone)]
pub struct CelestiaClient {
    light_node_url: String,
    private_key: String,
}

impl CelestiaClient {
    pub fn new() -> anyhow::Result<Self> {
        let config = CelestiaConfig::from_env()?;

        Ok(Self {
            light_node_url: config.api_node_url,
            private_key: config.api_private_key,
        })
    }
}

#[async_trait]
impl DataAvailabilityClient for CelestiaClient {
    async fn dispatch_blob(
        &self,
        batch_number: u32,
        data: Vec<u8>,
    ) -> Result<types::DispatchResponse, types::DAError> {
        let token = std::env::var("CELESTIA_NODE_AUTH_TOKEN").expect("Token not provided");
        let client = Client::new("http://localhost:26658", Some(&token))
            .await
            .expect("Failed creating rpc client");

        let my_namespace = Namespace::new_v0(&[0xDE, 0xAD, 0xBE, 0xEF]).expect("Invalid namespace");
        let blob = Blob::new(my_namespace, data).expect("Failed to create a blob");

        client
            .blob_submit(&[blob], GasPrice::default())
            .await
            .map_err(|error| types::DAError {
                error: error.into(),
                is_transient: true,
            })
            .map(|blob_id| types::DispatchResponse {
                blob_id: format!("{blob_id}"),
            })
    }

    async fn get_inclusion_data(
        &self,
        blob_id: &str,
    ) -> Result<Option<types::InclusionData>, types::DAError> {
        todo!()
    }

    fn clone_boxed(&self) -> Box<dyn DataAvailabilityClient> {
        Box::new(self.clone())
    }

    fn blob_size_limit(&self) -> Option<usize> {
        Some(1973786)
    }
}

impl Debug for CelestiaClient {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CelestiaClient")
            .field("light_node_url", &self.light_node_url)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_blob() {
        let client = CelestiaClient::new().unwrap();
        let result = client.dispatch_blob(0, b"cui bono".to_vec()).await;
        println!("{result:#?}");
        assert!(false);
    }
}
