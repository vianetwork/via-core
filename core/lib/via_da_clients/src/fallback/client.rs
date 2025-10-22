use std::fmt::Debug;
use zksync_types::web3::keccak256;

use async_trait::async_trait;
use zksync_da_client::{
    types::{DAError, DispatchResponse, InclusionData},
    DataAvailabilityClient,
};

/// A fallback DA client that tries the primary client first (typically Celestia),
/// and falls back to a secondary client (typically External Node) if the primary fails.
/// This is useful for handling scenarios where Celestia data expires after 30 days.
#[derive(Clone)]
pub struct FallbackDaClient {
    primary: Box<dyn DataAvailabilityClient>,
    fallback: Option<Box<dyn DataAvailabilityClient>>,
    /// If true, verifies that data from fallback matches data from primary when both are available
    verify_consistency: bool,
}

impl FallbackDaClient {
    pub fn new(
        primary: Box<dyn DataAvailabilityClient>,
        fallback: Option<Box<dyn DataAvailabilityClient>>,
        verify_consistency: bool,
    ) -> Self {
        Self {
            primary,
            fallback,
            verify_consistency,
        }
    }

    /// Verifies that the data from both sources matches
    fn verify_data_consistency(
        &self,
        primary_data: &InclusionData,
        fallback_data: &InclusionData,
    ) -> Result<(), DAError> {
        if primary_data.data != fallback_data.data {
            return Err(DAError {
                error: anyhow::anyhow!(
                    "Data mismatch between primary and fallback DA sources. Primary data hash: {}, Fallback data hash: {}",
                    hex::encode(keccak256(&primary_data.data)),
                    hex::encode(keccak256(&fallback_data.data))
                ),
                is_retriable: false,
            });
        }
        Ok(())
    }
}

#[async_trait]
impl DataAvailabilityClient for FallbackDaClient {
    /// Dispatches blob to the primary client only
    async fn dispatch_blob(
        &self,
        batch_number: u32,
        data: Vec<u8>,
    ) -> Result<DispatchResponse, DAError> {
        self.primary.dispatch_blob(batch_number, data).await
    }

    /// Fetches inclusion data, trying primary first, then fallback if primary fails
    async fn get_inclusion_data(&self, blob_id: &str) -> Result<Option<InclusionData>, DAError> {
        // Try primary client first
        match self.primary.get_inclusion_data(blob_id).await {
            Ok(Some(primary_data)) => {
                // If verification is enabled, also fetch from fallback and verify consistency
                if self.verify_consistency {
                    tracing::info!(
                        "Primary DA client returned data for blob_id: {}, verifying with fallback",
                        blob_id
                    );

                    // If fallback solution
                    if let Some(fallback) = self.fallback.clone() {
                        match fallback.get_inclusion_data(blob_id).await {
                            Ok(Some(fallback_data)) => {
                                if let Err(e) =
                                    self.verify_data_consistency(&primary_data, &fallback_data)
                                {
                                    tracing::error!(
                                        "Data consistency verification failed for blob_id {}: {}",
                                        blob_id,
                                        e.error
                                    );
                                    return Err(e);
                                }
                                tracing::info!(
                                    "Data consistency verified successfully for blob_id: {}",
                                    blob_id
                                );
                            }
                            Ok(None) => {
                                tracing::warn!(
                                    "Fallback DA client has no data for blob_id: {} (primary has data)",
                                    blob_id
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to fetch from fallback DA for verification: {}",
                                    e.error
                                );
                            }
                        }
                    }
                }
                Ok(Some(primary_data))
            }
            Ok(None) => {
                let Some(fallback) = self.fallback.clone() else {
                    return Ok(None);
                };

                // Primary returned None, try fallback
                tracing::info!(
                    "Primary DA client returned no data for blob_id: {}, trying fallback",
                    blob_id
                );

                match fallback.get_inclusion_data(blob_id).await {
                    Ok(Some(fallback_data)) => {
                        tracing::info!(
                            "Fallback DA client successfully retrieved data for blob_id: {}",
                            blob_id
                        );
                        Ok(Some(fallback_data))
                    }
                    Ok(None) => {
                        tracing::warn!(
                            "Neither primary nor fallback DA client has data for blob_id: {}",
                            blob_id
                        );
                        Ok(None)
                    }
                    Err(e) => {
                        tracing::error!(
                            "Both primary and fallback DA clients failed for blob_id: {}. Fallback error: {}",
                            blob_id,
                            e.error
                        );
                        Err(e)
                    }
                }
            }
            Err(primary_error) => {
                // Primary failed with error, try fallback
                tracing::warn!(
                    "Primary DA client failed for blob_id: {} with error: {}. Trying fallback",
                    blob_id,
                    primary_error.error
                );

                let Some(fallback) = self.fallback.clone() else {
                    return Err(primary_error);
                };

                match fallback.get_inclusion_data(blob_id).await {
                    Ok(result) => {
                        if result.is_some() {
                            tracing::info!(
                                "Fallback DA client successfully retrieved data after primary failure for blob_id: {}",
                                blob_id
                            );
                        }
                        Ok(result)
                    }
                    Err(fallback_error) => {
                        tracing::error!(
                            "Both primary and fallback DA clients failed for blob_id: {}. Primary error: {}, Fallback error: {}",
                            blob_id,
                            primary_error.error,
                            fallback_error.error
                        );
                        // Return the primary error as it's likely more relevant
                        Err(primary_error)
                    }
                }
            }
        }
    }

    fn clone_boxed(&self) -> Box<dyn DataAvailabilityClient> {
        Box::new(self.clone())
    }

    fn blob_size_limit(&self) -> Option<usize> {
        // Use the primary client's size limit
        self.primary.blob_size_limit()
    }
}

impl Debug for FallbackDaClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackDaClient")
            .field("primary", &self.primary)
            .field("fallback", &self.fallback)
            .field("verify_consistency", &self.verify_consistency)
            .finish()
    }
}
