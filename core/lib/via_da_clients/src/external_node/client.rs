use std::fmt::Debug;

use async_trait::async_trait;
use zksync_da_client::{
    types::{DAError, DispatchResponse, InclusionData},
    DataAvailabilityClient,
};
use zksync_web3_decl::{
    client::{DynClient, L2},
    namespaces::ViaNamespaceClient,
};

/// An implementation of the `DataAvailabilityClient` trait that retrieves data from an external node.
/// This client is used as a fallback when Celestia data is not available (e.g., after 30 days).
#[derive(Clone)]
pub struct ExternalNodeDaClient {
    client: Box<DynClient<L2>>,
}

impl ExternalNodeDaClient {
    /// Creates a new ExternalNodeDaClient using the provided L2 query client.
    ///
    /// The client should be obtained from the node framework's `L2InterfaceResource`.
    pub fn new(client: Box<DynClient<L2>>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DataAvailabilityClient for ExternalNodeDaClient {
    /// External nodes don't dispatch blobs - this operation is not supported
    async fn dispatch_blob(
        &self,
        _batch_number: u32,
        _data: Vec<u8>,
    ) -> Result<DispatchResponse, DAError> {
        Err(DAError {
            error: anyhow::anyhow!("ExternalNodeDaClient does not support dispatching blobs"),
            is_retriable: false,
        })
    }

    /// Fetches the inclusion data for a given blob_id from the external node via RPC
    async fn get_inclusion_data(&self, blob_id: &str) -> Result<Option<InclusionData>, DAError> {
        let result = self
            .client
            .get_da_blob_data(blob_id.to_string())
            .await
            .map_err(|e| DAError {
                error: e.into(),
                is_retriable: true,
            })?;

        match result {
            Some(blob) => {
                if blob.is_proof {
                    let data = hex::decode(&blob.proof_data).map_err(|e| DAError {
                        error: e.into(),
                        is_retriable: false,
                    })?;
                    Ok(Some(InclusionData { data }))
                } else {
                    let data = hex::decode(&blob.pub_data).map_err(|e| DAError {
                        error: e.into(),
                        is_retriable: false,
                    })?;
                    Ok(Some(InclusionData { data }))
                }
            }
            None => Ok(None),
        }
    }

    fn clone_boxed(&self) -> Box<dyn DataAvailabilityClient> {
        Box::new(self.clone())
    }

    fn blob_size_limit(&self) -> Option<usize> {
        None // External node doesn't enforce size limits
    }
}

impl Debug for ExternalNodeDaClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalNodeDaClient").finish()
    }
}
