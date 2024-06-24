use std::{
    fmt::{Debug, Formatter},
    sync::Arc,
};

use anyhow::anyhow;
use async_trait::async_trait;
use celestia_rpc::{BlobClient, Client};
use celestia_types::{blob::GasPrice, nmt::Namespace, Blob};
use zksync_config::configs::clients::CelestiaConfig;
pub use zksync_da_client::{types, DataAvailabilityClient};
use zksync_env_config::FromEnv;
pub mod wiring_layer;

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
        let my_namespace = Namespace::new_v0(&[0xDA, 0xAD, 0xBE, 0xEF]).expect("Invalid namespace");
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
        let my_namespace = Namespace::new_v0(&[0xDA, 0xAD, 0xBE, 0xEF]).expect("Invalid namespace");
        match self
            .inner
            .blob_get_all(blob_id.parse().unwrap(), &[my_namespace])
            .await
        {
            // the vector must has exactly 1 item, otherwise it's an error
            Ok(data) if data.len() != 1 => Err(types::DAError {
                error: anyhow!(if data.is_empty() {
                    "No blobs found"
                } else {
                    "More than one blob found"
                }),
                is_transient: true,
            }),
            Ok(mut data) => Ok(Some(types::InclusionData {
                data: std::mem::take(&mut data[0].data),
            })),
            Err(error) => Err(types::DAError {
                error: error.into(),
                is_transient: true,
            }),
        }
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

        let result = client.dispatch_blob(0, b"cui bono?".to_vec()).await;

        assert!(result.is_ok());

        let result = client
            .get_inclusion_data(&result.unwrap().blob_id)
            .await
            .unwrap();

        assert!(result.is_some());
    }
}
