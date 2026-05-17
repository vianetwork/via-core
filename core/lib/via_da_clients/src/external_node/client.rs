use std::fmt::Debug;

use async_trait::async_trait;
use celestia_types::{consts::appconsts::SHARE_VERSION_ZERO, nmt::Namespace, Commitment};
use zksync_da_client::{
    types::{DAError, DispatchResponse, InclusionData},
    DataAvailabilityClient,
};
use zksync_web3_decl::{
    client::{DynClient, L2},
    namespaces::ViaNamespaceClient,
};

use crate::common::{parse_blob_id, VIA_NAME_SPACE_BYTES};

/// An implementation of the `DataAvailabilityClient` trait that retrieves data from an external node.
/// This client is used as a fallback when Celestia data is not available (e.g., after 30 days).
#[derive(Clone)]
pub struct ExternalNodeDaClient {
    client: Box<DynClient<L2>>,
    namespace: Namespace,
}

impl ExternalNodeDaClient {
    /// Creates a new ExternalNodeDaClient using the provided L2 query client.
    ///
    /// The client should be obtained from the node framework's `L2InterfaceResource`.
    pub fn new(client: Box<DynClient<L2>>) -> anyhow::Result<Self> {
        let namespace = Namespace::new_v0(&VIA_NAME_SPACE_BYTES)?;

        Ok(Self { client, namespace })
    }

    pub fn validate_commitment(&self, blob_id: &str, data: &[u8]) -> anyhow::Result<(), DAError> {
        let commitment =
            Commitment::from_blob(self.namespace, &data, SHARE_VERSION_ZERO).map_err(|error| {
                DAError {
                    error: anyhow!("Error to create commitment: {}", error.to_string()),
                    is_retriable: false,
                }
            })?;

        // let commitment_result =
        //     match Commitment::from_blob(self.namespace, SHARE_VERSION_ZERO, data) {
        //         Ok(commit) => commit,
        //         Err(error) => {
        //             return Err(DAError {
        //                 error: error.into(),
        //                 is_retriable: false,
        //             })
        //         }
        //     };

        let blob_id_bytes = hex::decode(blob_id).map_err(|error| DAError {
            error: error.into(),
            is_retriable: true,
        })?;

        // Prepend the block height to the blob_id
        // let mut blob_id_with_block_height = Vec::with_capacity(8 + 32);
        // blob_id_with_block_height.extend_from_slice(&(0 as u64).to_be_bytes());
        // blob_id_with_block_height.extend_from_slice(&blob_id_bytes);
        // let blob_id_str = hex::encode(blob_id_with_block_height.clone());

        // println!(
        //     "*****************************: {:?}",
        //     blob_id_with_block_height
        // );

        let (commitment, _) = parse_blob_id(&blob_id).map_err(|error| DAError {
            error: error.into(),
            is_retriable: true,
        })?;

        println!(
            "-----------------------------------------------: {:?}",
            commitment.0
        );

        println!(
            "-----------------------------------------------: {:?}",
            commitment_result.0
        );

        if commitment.0 != commitment_result.0 {
            return Err(DAError {
                error: anyhow::anyhow!("Commitment mismatch"),
                is_retriable: false,
            });
        }
        Ok(())
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

                    // self.validate_commitment(blob_id, &data)
                    //     .map_err(|e| DAError {
                    //         error: e.into(),
                    //         is_retriable: false,
                    //     })?;

                    Ok(Some(InclusionData { data }))
                } else {
                    let data = hex::decode(&blob.pub_data).map_err(|e| DAError {
                        error: e.into(),
                        is_retriable: false,
                    })?;
                    println!(
                        "-----------------------------------------------{:?}",
                        blob.pub_data
                    );

                    self.validate_commitment(blob_id, &data)
                        .map_err(|e| DAError {
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
