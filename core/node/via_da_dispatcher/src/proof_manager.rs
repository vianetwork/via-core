// proof_manager.rs

use std::sync::Arc;

use zksync_dal::{Connection, Core};
use zksync_l1_contract_interface::i_executor::methods::ProveBatches;
use zksync_object_store::{ObjectStore, ObjectStoreError};
use zksync_prover_interface::outputs::L1BatchProofForL1;
use zksync_types::{
    protocol_version::{L1VerifierConfig, ProtocolSemanticVersion},
    L1BatchNumber,
};

#[derive(Debug)]
pub struct ProofManager {
    blob_store: Arc<dyn ObjectStore>,
}

impl ProofManager {
    /// Creates a new instance of `ProofManager`.
    pub fn new(blob_store: Arc<dyn ObjectStore>) -> Self {
        Self { blob_store }
    }

    /// Loads a real proof operation for a given L1 batch number.
    pub async fn load_real_proof_operation(
        &self,
        storage: &mut Connection<'_, Core>,
        l1_verifier_config: L1VerifierConfig,
    ) -> Option<ProveBatches> {
        let previous_proven_batch_number = storage
            .blocks_dal()
            .get_last_l1_batch_with_prove_tx()
            .await
            .unwrap_or_default();
        let batch_to_prove = previous_proven_batch_number + 1;

        // Return `None` if batch is not committed yet.
        let commit_tx_id = storage
            .blocks_dal()
            .get_eth_commit_tx_id(batch_to_prove)
            .await
            .unwrap_or(None)?;

        // Note: Removed `is_4844_mode` logic that checks if the commit transaction is confirmed.

        let minor_version = storage
            .blocks_dal()
            .get_batch_protocol_version_id(batch_to_prove)
            .await
            .unwrap_or(None)
            .unwrap();

        // Fetch allowed protocol versions corresponding to the verification key on L1.
        let allowed_patch_versions = storage
            .protocol_versions_dal()
            .get_patch_versions_for_vk(
                minor_version,
                l1_verifier_config.recursion_scheduler_level_vk_hash,
            )
            .await
            .unwrap_or_default();

        if allowed_patch_versions.is_empty() {
            tracing::warn!(
                "No patch version corresponds to the verification key on L1: {:?}",
                l1_verifier_config.recursion_scheduler_level_vk_hash
            );
            return None;
        }

        let allowed_versions: Vec<_> = allowed_patch_versions
            .into_iter()
            .map(|patch| ProtocolSemanticVersion {
                minor: minor_version,
                patch,
            })
            .collect();

        // Attempt to load the proof from the object store.
        let proof = self
            .load_wrapped_fri_proofs_for_range(batch_to_prove, &allowed_versions)
            .await?;

        let previous_proven_batch_metadata = storage
            .blocks_dal()
            .get_l1_batch_metadata(previous_proven_batch_number)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "L1 batch #{} with submitted proof is not complete in the DB",
                    previous_proven_batch_number
                );
            });
        let metadata_for_batch_being_proved = storage
            .blocks_dal()
            .get_l1_batch_metadata(batch_to_prove)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "L1 batch #{} with generated proof is not complete in the DB",
                    batch_to_prove
                );
            });

        Some(ProveBatches {
            prev_l1_batch: previous_proven_batch_metadata,
            l1_batches: vec![metadata_for_batch_being_proved],
            proofs: vec![proof],
            should_verify: true,
        })
    }

    /// Loads wrapped FRI proofs for a given L1 batch number and allowed protocol versions.
    pub async fn load_wrapped_fri_proofs_for_range(
        &self,
        l1_batch_number: L1BatchNumber,
        allowed_versions: &[ProtocolSemanticVersion],
    ) -> Option<L1BatchProofForL1> {
        for version in allowed_versions {
            match self.blob_store.get((l1_batch_number, *version)).await {
                Ok(proof) => return Some(proof),
                Err(ObjectStoreError::KeyNotFound(_)) => continue, // Proof is not ready yet.
                Err(err) => panic!(
                    "Failed to load proof for batch {}: {}",
                    l1_batch_number.0, err
                ),
            }
        }

        // Check for deprecated file naming if patch 0 is allowed.
        // TODO: Remove this in the next release.
        let is_patch_0_present = allowed_versions.iter().any(|v| v.patch.0 == 0);
        if is_patch_0_present {
            match self
                .blob_store
                .get_by_encoded_key(format!("l1_batch_proof_{}.bin", l1_batch_number))
                .await
            {
                Ok(proof) => return Some(proof),
                Err(ObjectStoreError::KeyNotFound(_)) => (), // Proof is not ready yet.
                Err(err) => panic!(
                    "Failed to load proof for batch {}: {}",
                    l1_batch_number.0, err
                ),
            }
        }

        None
    }
}