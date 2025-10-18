use std::sync::Arc;

use zksync_object_store::{ObjectStore, ObjectStoreError};
use zksync_prover_interface::outputs::L1BatchProofForL1;
use zksync_types::{protocol_version::ProtocolSemanticVersion, L1BatchNumber};

/// Loads wrapped FRI proofs for a given L1 batch number and allowed protocol versions.
pub async fn load_wrapped_fri_proofs_for_range(
    blob_store: Arc<dyn ObjectStore>,
    l1_batch_number: L1BatchNumber,
    allowed_versions: &[ProtocolSemanticVersion],
) -> Option<L1BatchProofForL1> {
    for version in allowed_versions {
        match blob_store.get((l1_batch_number, *version)).await {
            Ok(proof) => {
                return Some(proof);
            }
            Err(ObjectStoreError::KeyNotFound(_)) => continue, // Proof is not ready yet.
            Err(err) => {
                tracing::error!(
                    "Failed to load proof for batch {} and version {:?}: {}",
                    l1_batch_number.0,
                    version,
                    err
                );
                return None;
            }
        }
    }

    // Check for deprecated file naming if patch 0 is allowed.
    let is_patch_0_present = allowed_versions.iter().any(|v| v.patch.0 == 0);
    if is_patch_0_present {
        match blob_store
            .get_by_encoded_key(format!("l1_batch_proof_{}.bin", l1_batch_number))
            .await
        {
            Ok(proof) => {
                return Some(proof);
            }
            Err(ObjectStoreError::KeyNotFound(_)) => {
                tracing::error!(
                    "KeyNotFound for loading proof for batch {}",
                    l1_batch_number.0
                );
            }
            Err(err) => {
                tracing::error!(
                    "Failed to load proof for batch {} from deprecated naming: {}",
                    l1_batch_number.0,
                    err
                );
                return None;
            }
        }
    }

    None
}
