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
        let namespace = Namespace::new_v0(namespace_bytes)
            .map_err(|error| types::DAError { error: error.into(), is_retriable: false })?;

        Ok(Self { light_node_url: secrets.api_node_url, inner: Arc::new(client), blob_size_limit, namespace })
    }

    fn parse_blob_id(&self, blob_id: &str) -> anyhow::Result<(Commitment, u64)> {
        // [8]byte block height ++ [32]byte commitment
        let blob_id_bytes =
            hex::decode(blob_id).map_err(|error| types::DAError { error: error.into(), is_retriable: false })?;

        let block_height =
            u64::from_be_bytes(blob_id_bytes[..8].try_into().map_err(|_| types::DAError {
                error: anyhow!("Failed to convert block height"),
                is_retriable: false,
            })?);

        let commitment_data: [u8; 32] = blob_id_bytes[8..40]
            .try_into()
            .map_err(|_| types::DAError { error: anyhow!("Failed to convert commitment"), is_retriable: false })?;
        let commitment = Commitment(commitment_data);

        Ok((commitment, block_height))
    }
}

#[async_trait]
impl DataAvailabilityClient for CelestiaClient {
    async fn dispatch_blob(
        &self, _batch_number: u32, data: Vec<u8>,
    ) -> Result<types::DispatchResponse, types::DAError> {
        let share_version = celestia_types::consts::appconsts::SHARE_VERSION_ZERO;

        let blob = Blob::new(self.namespace, data.clone())
            .map_err(|error| types::DAError { error: error.into(), is_retriable: false })?;

        let commitment_result = match Commitment::from_blob(self.namespace, share_version, &data) {
            Ok(commit) => commit,
            Err(error) => return Err(types::DAError { error: error.into(), is_retriable: false }),
        };

        // NOTE: during refactoring add address to the config
        // we can specify the sender address for the transaction with using TxConfig
        let tx_config = TxConfig { gas_price: Some(GAS_PRICE), ..Default::default() };

        let block_height = self
            .inner
            .blob_submit(&[blob], tx_config)
            .await
            .map_err(|error| types::DAError { error: error.into(), is_retriable: true })?;

        // [8]byte block height ++ [32]byte commitment
        let mut blob_id = Vec::with_capacity(8 + 32);
        blob_id.extend_from_slice(&block_height.to_be_bytes());
        blob_id.extend_from_slice(&commitment_result.0);

        // Convert blob_id to a hex string
        let blob_id_str = hex::encode(blob_id);

        return Ok(types::DispatchResponse { blob_id: blob_id_str });
    }

    async fn get_inclusion_data(&self, blob_id: &str) -> Result<Option<types::InclusionData>, types::DAError> {
        let (commitment, block_height) =
            self.parse_blob_id(&blob_id).map_err(|error| types::DAError { error: error.into(), is_retriable: true })?;

        let blob = self
            .inner
            .blob_get(block_height, self.namespace, commitment)
            .await
            .map_err(|error| types::DAError { error: error.into(), is_retriable: true })?;

        let data = match ViaDaBlob::from_bytes(&blob.data) {
            Some(blob) => {
                if blob.chunks == 1 {
                    blob.data
                } else {
                    let blob_ids: Vec<String> = deserialize_blob_ids(&blob.data).map_err(|_| types::DAError {
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
                        let (commitment, block_height) = self
                            .parse_blob_id(&blob_id)
                            .map_err(|error| types::DAError { error: error.into(), is_retriable: true })?;

                        let blob = self
                            .inner
                            .blob_get(block_height, self.namespace, commitment)
                            .await
                            .map_err(|error| types::DAError { error: error.into(), is_retriable: true })?;

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
        f.debug_struct("CelestiaClient").field("light_node_url", &self.light_node_url).finish()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use httpmock::{Method::POST, Mock, MockServer};
    use serde_json::json;

    use super::*;

    // Hand-assembled height BE || opaque commitment bytes, independent of Via's codec:
    // https://github.com/vianetwork/via-core/blob/ca11943ebc679f6f40a4f810cbe4fc21d296aae9/core/lib/via_da_clients/src/celestia/client.rs
    const CHILD_ZERO: &str =
        "0102030405060708000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const ROOT_ID: &str =
        "2122232425262728404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f";
    // Literal u64 LE count/length and u32 BE child lengths; children have no envelopes:
    // https://github.com/servo/bincode/blob/c44b5e364e7084cdbabf9f94b63a3c7f32b8fb68/src/lib.rs
    // https://github.com/vianetwork/via-core/blob/ca11943ebc679f6f40a4f810cbe4fc21d296aae9/core/node/via_da_dispatcher/src/da_dispatcher.rs
    const SINGLE_ENVELOPE: &str = "01000000000000000400000000000000000102ff";
    const ROOT_ENVELOPE: &str = concat!(
        "020000000000000058000000000000000000002801020304050607080001020304",
        "05060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f000000281112",
        "131415161718202122232425262728292a2b2c2d2e2f303132333435363738393a3b",
        "3c3d3e3f",
    );

    fn namespace() -> Namespace {
        Namespace::new_v0(b"VIA\0\0\0\0\0").unwrap()
    }

    async fn client(server: &MockServer) -> CelestiaClient {
        let url: SensitiveUrl = server.url("/").parse().unwrap();
        CelestiaClient {
            inner: Arc::new(Client::new(url.expose_str(), None).await.unwrap()),
            light_node_url: url,
            blob_size_limit: 1_000_000,
            namespace: namespace(),
        }
    }

    async fn mock_get<'a>(
        server: &'a MockServer,
        request_id: u64,
        height: u64,
        commitment: [u8; 32],
        data: &[u8],
    ) -> Mock<'a> {
        let commitment = Commitment(commitment);
        let response_blob = Blob {
            namespace: namespace(),
            data: data.to_vec(),
            share_version: 0,
            commitment,
            index: None,
        };
        server
            .mock_async(|when, then| {
                when.method(POST).path("/").json_body(json!({
                    "jsonrpc": "2.0", "id": request_id, "method": "blob.Get",
                    "params": [height, namespace(), commitment],
                }));
                then.status(200).json_body(json!({
                    "jsonrpc": "2.0", "id": request_id, "result": response_blob,
                }));
            })
            .await
    }

    #[tokio::test]
    async fn blob_id_golden_layout_and_submitted_commitment() {
        let server = MockServer::start_async().await;
        let client = client(&server).await;
        let (commitment, height) = client.parse_blob_id(CHILD_ZERO).unwrap();
        assert_eq!(height, 0x0102030405060708);
        assert_eq!(
            hex::encode(commitment.0),
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
        );
        for malformed in ["0", "zz"] {
            assert!(client.parse_blob_id(malformed).is_err());
        }

        let payload = hex::decode(SINGLE_ENVELOPE).unwrap();
        let submitted = Blob::new(namespace(), payload.clone()).unwrap();
        let submitted_commitment = submitted.commitment.0;
        let submit = server
            .mock_async(|when, then| {
                when.method(POST).path("/").json_body(json!({
                    "jsonrpc": "2.0", "id": 0, "method": "blob.Submit",
                    "params": [[submitted], TxConfig { gas_price: Some(-1.0), ..Default::default() }],
                }));
                then.status(200).json_body(json!({
                    "jsonrpc": "2.0", "id": 0, "result": 0x0102030405060708_u64,
                }));
            })
            .await;
        let response =
            tokio::time::timeout(Duration::from_secs(5), client.dispatch_blob(1, payload))
                .await
                .unwrap()
                .unwrap();
        submit.assert_async().await;
        let id = hex::decode(&response.blob_id).unwrap();
        assert_eq!(id.len(), 40);
        assert_eq!(&id[..8], &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
        assert_eq!(&id[8..], &submitted_commitment);
        assert_eq!(response.blob_id, response.blob_id.to_ascii_lowercase());
    }

    #[tokio::test]
    async fn multi_chunk_golden_manifest_reassembles_raw_children() {
        let server = MockServer::start_async().await;
        let client = client(&server).await;
        let root = mock_get(
            &server,
            0,
            0x2122232425262728,
            std::array::from_fn(|i| 0x40 + i as u8),
            &hex::decode(ROOT_ENVELOPE).unwrap(),
        )
        .await;
        let mut children = Vec::new();
        for (height, commitment, bytes) in [
            (0x0102030405060708, std::array::from_fn(|i| i as u8), vec![0x00, 0x01, 0x02, 0xff]),
            (
                0x1112131415161718,
                std::array::from_fn(|i| 0x20 + i as u8),
                vec![0x56, 0x49, 0x41, 0xaa, 0xbb],
            ),
        ] {
            let mut alternatives = Vec::new();
            for request_id in [1, 2] {
                alternatives.push(mock_get(&server, request_id, height, commitment, &bytes).await);
            }
            children.push(alternatives);
        }
        let included =
            tokio::time::timeout(Duration::from_secs(5), client.get_inclusion_data(ROOT_ID))
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        assert_eq!(included.data, hex::decode("000102ff564941aabb").unwrap());
        root.assert_async().await;
        for alternatives in children {
            let mut hits = 0;
            for mock in alternatives {
                hits += mock.hits_async().await;
            }
            assert_eq!(hits, 1);
        }
    }

    #[tokio::test]
    async fn manifest_chunk_count_mismatch_is_not_retriable() {
        let server = MockServer::start_async().await;
        let client = client(&server).await;
        let mut data = hex::decode(ROOT_ENVELOPE).unwrap();
        data[0] = 3;
        let root = mock_get(
            &server,
            0,
            0x2122232425262728,
            std::array::from_fn(|i| 0x40 + i as u8),
            &data,
        )
        .await;
        let error =
            tokio::time::timeout(Duration::from_secs(5), client.get_inclusion_data(ROOT_ID))
                .await
                .unwrap()
                .err()
                .expect("manifest chunk-count mismatch must fail");
        assert!(!error.is_retriable);
        root.assert_async().await;
    }

    #[tokio::test]
    async fn fixed_single_envelope_and_legacy_raw_blob_data() {
        for (stored, expected) in [(SINGLE_ENVELOPE, "000102ff"), ("aabb", "aabb")] {
            let server = MockServer::start_async().await;
            let client = client(&server).await;
            let get = mock_get(
                &server,
                0,
                0x0102030405060708,
                std::array::from_fn(|i| i as u8),
                &hex::decode(stored).unwrap(),
            )
            .await;
            let included =
                tokio::time::timeout(Duration::from_secs(5), client.get_inclusion_data(CHILD_ZERO))
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
            assert_eq!(included.data, hex::decode(expected).unwrap());
            get.assert_async().await;
        }
    }
}
