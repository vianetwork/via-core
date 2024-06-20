use std::{
    fmt::{Debug, Formatter},
    sync::Arc,
};

use async_trait::async_trait;
use celestia_rpc::{BlobClient, Client};
use celestia_types::{blob::GasPrice, nmt::Namespace, Blob};
pub use via_zksync_da_client::{types, DataAvailabilityClient};
use zksync_config::configs::clients::CelestiaConfig;
use zksync_env_config::FromEnv;

#[derive(Clone)]
pub struct CelestiaClient {
    light_node_url: String,
    private_key: String,
    auth_token: String,
    inner: Arc<Client>,
}

impl CelestiaClient {
    pub async fn new() -> anyhow::Result<Self> {
        let config = CelestiaConfig::from_env()?;

        let client = Client::new(&config.api_node_url, Some(&config.auth_token))
            .await
            .expect("Failed creating rpc client");

        Ok(Self {
            light_node_url: config.api_node_url,
            private_key: config.api_private_key,
            auth_token: config.auth_token,
            inner: Arc::new(client),
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
        let my_namespace = Namespace::new_v0(&[0xDE, 0xAD, 0xBE, 0xEF]).expect("Invalid namespace");
        let blob = Blob::new(my_namespace, data).expect("Failed to create a blob");

        self.inner
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
        // let my_namespace = Namespace::new_v0(&[0u8; 28]);
        let my_namespace =
            Namespace::new_v0(&[0x42, 0x69, 0x0C, 0x20, 0x4D, 0x39, 0x60, 0x0F, 0xDD, 0xD3])
                .expect("Invalid namespace");
        let r = self
            .inner
            .blob_get_all(blob_id.parse().unwrap(), &[my_namespace])
            .await
            .unwrap();
        println!("return {r:?}");
        Ok(Some(types::InclusionData { data: vec![] }))
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
        let client = CelestiaClient::new().await.unwrap();
        // let result = client.dispatch_blob(0, b"cui bono?".to_vec()).await;
        // println!("{result:#?}");
        let result = client.get_inclusion_data("1292258").await;
        let x = result.unwrap().unwrap().data;
        // assert!(result.is_ok());
        assert!(false);
    }
}
