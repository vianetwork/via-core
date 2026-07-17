use std::{
    fmt::{Debug, Formatter},
    sync::Arc,
};

use anyhow::anyhow;
use async_trait::async_trait;
use celestia_rpc::{BlobClient, Client, P2PClient};
use celestia_types::{nmt::Namespace, Blob, Commitment, TxConfig};
use hex;
use zksync_config::configs::via_secrets::ViaDASecrets;
pub use zksync_config::ViaCelestiaConfig;
pub use zksync_da_client::{types, DataAvailabilityClient};
use zksync_types::{
    url::SensitiveUrl,
    via_da_dispatcher::{deserialize_blob_ids, ViaDaBlob},
};

/// If no value is provided for GasPrice, then this will be serialized to `-1.0` which means the node that
/// receives the request will calculate the GasPrice for given blob.
const GAS_PRICE: f64 = -1.0;

/// An implementation of the `DataAvailabilityClient` trait that stores the pubdata in the Celestia DA.
#[derive(Clone)]
pub struct CelestiaClient {
    light_node_url: SensitiveUrl,
    inner: Arc<Client>,
    blob_size_limit: usize,
    namespace: Namespace,
}

impl CelestiaClient {
    pub async fn new(secrets: ViaDASecrets, blob_size_limit: usize) -> anyhow::Result<Self> {
        let client = Client::new(secrets.api_node_url.expose_str(), Some(&secrets.auth_token))
            .await
            .map_err(|error| anyhow!("Failed to create a client: {}", error))?;

        // connection test
        let _info = client.p2p_info().await?;

        let namespace_bytes = [b'V', b'I', b'A', 0, 0, 0, 0, 0]; // Pad with zeros to reach 8 bytes
        let namespace_bytes: &[u8] = &namespace_bytes;
        let namespace = Namespace::new_v0(namespace_bytes).map_err(|error| types::DAError {
            error: error.into(),
            is_retriable: false,
        })?;

        Ok(Self {
            light_node_url: secrets.api_node_url,
            inner: Arc::new(client),
            blob_size_limit,
            namespace,
        })
    }

    fn parse_blob_id(&self, blob_id: &str) -> anyhow::Result<(Commitment, u64)> {
        // [8]byte block height ++ [32]byte commitment
        let blob_id_bytes = hex::decode(blob_id).map_err(|error| types::DAError {
            error: error.into(),
            is_retriable: false,
        })?;

        let block_height =
            u64::from_be_bytes(blob_id_bytes[..8].try_into().map_err(|_| types::DAError {
                error: anyhow!("Failed to convert block height"),
                is_retriable: false,
            })?);

        let commitment_data: [u8; 32] =
            blob_id_bytes[8..40]
                .try_into()
                .map_err(|_| types::DAError {
                    error: anyhow!("Failed to convert commitment"),
                    is_retriable: false,
                })?;
        let commitment = Commitment(commitment_data);

        Ok((commitment, block_height))
    }

    /// Verifies that the blob data matches the expected commitment.
    /// Returns an error if the commitment doesn't match.
    /// 
    /// This is a static method to allow unit testing without requiring
    /// a full CelestiaClient instance with network connections.
    fn verify_blob_commitment(
        namespace: Namespace,
        blob_data: &[u8],
        expected_commitment: &Commitment,
    ) -> Result<(), types::DAError> {
        let share_version = celestia_types::consts::appconsts::SHARE_VERSION_ZERO;

        let computed_commitment = Commitment::from_blob(namespace, share_version, blob_data)
            .map_err(|error| types::DAError {
            error: anyhow!("Failed to compute commitment: {}", error),
            is_retriable: false,
        })?;

        if computed_commitment != *expected_commitment {
            return Err(types::DAError {
                error: anyhow!(
                    "Commitment mismatch: expected {}, computed {}",
                    hex::encode(&expected_commitment.0),
                    hex::encode(&computed_commitment.0)
                ),
                is_retriable: false,
            });
        }

        Ok(())
    }
}

#[async_trait]
impl DataAvailabilityClient for CelestiaClient {
    async fn dispatch_blob(
        &self,
        _batch_number: u32,
        data: Vec<u8>,
    ) -> Result<types::DispatchResponse, types::DAError> {
        let share_version = celestia_types::consts::appconsts::SHARE_VERSION_ZERO;

        let blob = Blob::new(self.namespace, data.clone()).map_err(|error| types::DAError {
            error: error.into(),
            is_retriable: false,
        })?;

        let commitment_result = match Commitment::from_blob(self.namespace, share_version, &data) {
            Ok(commit) => commit,
            Err(error) => {
                return Err(types::DAError {
                    error: error.into(),
                    is_retriable: false,
                })
            }
        };

        // NOTE: during refactoring add address to the config
        // we can specify the sender address for the transaction with using TxConfig
        let tx_config = TxConfig {
            gas_price: Some(GAS_PRICE),
            ..Default::default()
        };

        let block_height = self
            .inner
            .blob_submit(&[blob], tx_config)
            .await
            .map_err(|error| types::DAError {
                error: error.into(),
                is_retriable: true,
            })?;

        // [8]byte block height ++ [32]byte commitment
        let mut blob_id = Vec::with_capacity(8 + 32);
        blob_id.extend_from_slice(&block_height.to_be_bytes());
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
        let (commitment, block_height) =
            self.parse_blob_id(&blob_id)
                .map_err(|error| types::DAError {
                    error: error.into(),
                    is_retriable: true,
                })?;

        let blob = self
            .inner
            .blob_get(block_height, self.namespace, commitment)
            .await
            .map_err(|error| types::DAError {
                error: error.into(),
                is_retriable: true,
            })?;

        // Verify the blob commitment before processing
        Self::verify_blob_commitment(self.namespace, &blob.data, &commitment)?;

        let data = match ViaDaBlob::from_bytes(&blob.data) {
            Some(blob) => {
                if blob.chunks == 1 {
                    blob.data
                } else {
                    let blob_ids: Vec<String> =
                        deserialize_blob_ids(&blob.data).map_err(|_| types::DAError {
                            error: anyhow!("Failed to deserialize blob ids"),
                            is_retriable: false,
                        })?;
                    if blob_ids.len() != blob.chunks {
                        return Err(types::DAError {
                            error: anyhow!(
                                "Mismatch, blob ids len [{}] != chunk size [{}]",
                                blob_ids.len(),
                                blob.chunks
                            ),
                            is_retriable: false,
                        });
                    }

                    let mut batch_blob = vec![];

                    for blob_id in blob_ids {
                        let (commitment, block_height) =
                            self.parse_blob_id(&blob_id)
                                .map_err(|error| types::DAError {
                                    error: error.into(),
                                    is_retriable: true,
                                })?;

                        let blob = self
                            .inner
                            .blob_get(block_height, self.namespace, commitment)
                            .await
                            .map_err(|error| types::DAError {
                                error: error.into(),
                                is_retriable: true,
                            })?;

                        // Verify each chunk's commitment
                        Self::verify_blob_commitment(self.namespace, &blob.data, &commitment)?;

                        batch_blob.extend_from_slice(&blob.data);
                    }

                    batch_blob
                }
            }
            None => blob.data,
        };

        let inclusion_data = types::InclusionData { data };

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

#[cfg(test)]
mod tests {
    use super::*;
    use celestia_types::{nmt::Namespace, Commitment};

    /// Helper to create the VIA namespace used in tests
    fn test_namespace() -> Namespace {
        let namespace_bytes = [b'V', b'I', b'A', 0, 0, 0, 0, 0];
        Namespace::new_v0(&namespace_bytes).expect("Failed to create test namespace")
    }

    #[test]
    fn test_commitment_computation_is_deterministic() {
        let namespace = test_namespace();
        let share_version = celestia_types::consts::appconsts::SHARE_VERSION_ZERO;
        let blob_data = b"test blob data for commitment verification";

        // Compute commitment twice
        let commitment1 =
            Commitment::from_blob(namespace, share_version, blob_data).expect("First commitment");
        let commitment2 =
            Commitment::from_blob(namespace, share_version, blob_data).expect("Second commitment");

        // Same data should produce same commitment
        assert_eq!(
            commitment1, commitment2,
            "Commitment computation should be deterministic"
        );
    }

    #[test]
    fn test_commitment_differs_for_different_data() {
        let namespace = test_namespace();
        let share_version = celestia_types::consts::appconsts::SHARE_VERSION_ZERO;
        let original_data = b"original data";
        let tampered_data = b"tampered data";

        let original_commitment = Commitment::from_blob(namespace, share_version, original_data)
            .expect("Original commitment");
        let tampered_commitment = Commitment::from_blob(namespace, share_version, tampered_data)
            .expect("Tampered commitment");

        // Different data should produce different commitments
        assert_ne!(
            original_commitment, tampered_commitment,
            "Different data should produce different commitments"
        );
    }

    #[test]
    fn test_commitment_differs_for_different_namespaces() {
        let namespace1 = test_namespace();
        let namespace2 = Namespace::new_v0(&[b'T', b'E', b'S', b'T', 0, 0, 0, 0])
            .expect("Failed to create second namespace");
        let share_version = celestia_types::consts::appconsts::SHARE_VERSION_ZERO;
        let blob_data = b"same data";

        let commitment1 =
            Commitment::from_blob(namespace1, share_version, blob_data).expect("Commitment 1");
        let commitment2 =
            Commitment::from_blob(namespace2, share_version, blob_data).expect("Commitment 2");

        // Same data in different namespaces should produce different commitments
        assert_ne!(
            commitment1, commitment2,
            "Same data in different namespaces should produce different commitments"
        );
    }

    #[test]
    fn test_verify_blob_commitment_logic_valid() {
        let namespace = test_namespace();
        let blob_data = b"test data for verification";

        // Compute the expected commitment
        let expected_commitment = Commitment::from_blob(
            namespace,
            celestia_types::consts::appconsts::SHARE_VERSION_ZERO,
            blob_data,
        )
        .expect("Failed to compute commitment");

        // Verification should succeed for valid data
        let result =
            CelestiaClient::verify_blob_commitment(namespace, blob_data, &expected_commitment);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_blob_commitment_logic_mismatch() {
        let namespace = test_namespace();
        let original_data = b"original data";
        let tampered_data = b"tampered data";

        // Compute commitment for original data
        let expected_commitment = Commitment::from_blob(
            namespace,
            celestia_types::consts::appconsts::SHARE_VERSION_ZERO,
            original_data,
        )
        .expect("Failed to compute original commitment");

        // Verification should fail for tampered data
        let result =
            CelestiaClient::verify_blob_commitment(namespace, tampered_data, &expected_commitment);
        assert!(result.is_err());

        // Assert on the error message
        let error = result.unwrap_err();
        assert!(error.error.to_string().contains("Commitment mismatch"));
    }

    #[test]
    fn test_commitment_is_32_bytes() {
        let namespace = test_namespace();
        let share_version = celestia_types::consts::appconsts::SHARE_VERSION_ZERO;
        let blob_data = b"any data";

        let commitment =
            Commitment::from_blob(namespace, share_version, blob_data).expect("Commitment");

        // Commitment should be exactly 32 bytes
        assert_eq!(commitment.0.len(), 32, "Commitment should be 32 bytes");
    }
}
