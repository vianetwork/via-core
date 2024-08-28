use std::{
    fmt::{Debug, Formatter},
    sync::Arc,
};

use anyhow::anyhow;
use async_trait::async_trait;
use celestia_rpc::{BlobClient, Client};
use celestia_types::{blob::GasPrice, nmt::Namespace, Blob, Commitment};
use hex;
pub use zksync_config::ViaCelestiaConfig;
pub use zksync_da_client::{types, DataAvailabilityClient};

/// An implementation of the `DataAvailabilityClient` trait that stores the pubdata in the Celestia DA.
#[derive(Clone)]
pub struct CelestiaClient {
    light_node_url: String,
    inner: Arc<Client>,
    blob_size_limit: usize,
}

impl CelestiaClient {
    pub async fn new(celestia_conf: ViaCelestiaConfig) -> anyhow::Result<Self> {
        let client = Client::new(&celestia_conf.api_node_url, Some(&celestia_conf.auth_token))
            .await
            .expect("Failed creating rpc client");

        Ok(Self {
            light_node_url: celestia_conf.api_node_url,
            inner: Arc::new(client),
            blob_size_limit: celestia_conf.blob_size_limit,
        })
    }
}

#[async_trait]
impl DataAvailabilityClient for CelestiaClient {
    async fn dispatch_blob(
        &self,
        _batch_number: u32,
        data: Vec<u8>,
    ) -> Result<types::DispatchResponse, types::DAError> {
        let my_namespace = Namespace::new_v0(&[0xDA, 0xAD, 0xBE, 0xEF]).expect("Invalid namespace");

        let share_version = celestia_types::consts::appconsts::SHARE_VERSION_ZERO;

        let blob = Blob::new(my_namespace, data.clone()).expect("Failed to create a blob");

        let commitment_result = match Commitment::from_blob(my_namespace, share_version, &data) {
            Ok(commit) => commit,
            Err(error) => {
                return Err(types::DAError {
                    error: error.into(),
                    is_transient: true,
                })
            }
        };

        let block_hight = self
            .inner
            .blob_submit(&[blob], GasPrice::default())
            .await
            .map_err(|error| types::DAError {
                error: error.into(),
                is_transient: true,
            })?;

        // [8]byte block height ++ [32]byte commitment
        let mut blob_id = Vec::with_capacity(8 + 32);
        blob_id.extend_from_slice(&block_hight.to_be_bytes());
        blob_id.extend_from_slice(&commitment_result.0);

        // Convert blob_id to a hex string
        let blob_id_str = hex::encode(blob_id);

        return Ok(types::DispatchResponse {
            blob_id: blob_id_str,
        });
    }

    async fn get_inclusion_data(
        &self,
        blob_id: &str,
    ) -> Result<Option<types::InclusionData>, types::DAError> {
        let my_namespace = Namespace::new_v0(&[0xDA, 0xAD, 0xBE, 0xEF]).expect("Invalid namespace");

        // [8]byte block height ++ [32]byte commitment
        let blob_id_bytes = hex::decode(blob_id).map_err(|error| types::DAError {
            error: error.into(),
            is_transient: true,
        })?;

        let block_height =
            u64::from_be_bytes(blob_id_bytes[..8].try_into().map_err(|_| types::DAError {
                error: anyhow!("Failed to convert block height"),
                is_transient: true,
            })?);

        let commitment_data: [u8; 32] = blob_id_bytes[8..40]
            .try_into()
            .expect("slice with incorrect length");
        let commitment = Commitment(commitment_data);

        let blob = self
            .inner
            .blob_get(block_height, my_namespace, commitment)
            .await
            .map_err(|error| types::DAError {
                error: error.into(),
                is_transient: true,
            })?;

        let inclusion_data = types::InclusionData { data: blob.data };

        Ok(Some(inclusion_data))
    }

    fn clone_boxed(&self) -> Box<dyn DataAvailabilityClient> {
        Box::new(self.clone())
    }

    fn blob_size_limit(&self) -> Option<usize> {
        Some(self.blob_size_limit)
    }
}

impl Debug for CelestiaClient {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CelestiaClient")
            .field("light_node_url", &self.light_node_url)
            .finish()
    }
}
