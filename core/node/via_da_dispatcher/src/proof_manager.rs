/*
1- create migration for new table
2- finalizing the via_data_availability_dal (add the new method and update the existing one to use the new table)
3- polish proof_manager
4- review the da_dispatcher task manager
5- update pull for inclusion to use the new table and adopt the proof_manager too.
*/

use std::sync::Arc;

use zksync_dal::{Connection, Core, CoreDal};
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
        let previous_proven_batch_number =
            match storage.blocks_dal().get_last_l1_batch_with_prove_tx().await {
                Ok(batch_number) => batch_number,
                Err(e) => {
                    tracing::error!("Failed to retrieve the last L1 batch with proof tx: {}", e);
                    return None;
                }
            };
        let batch_to_prove = previous_proven_batch_number + 1;

        let _commit_tx_id = match storage
            .blocks_dal()
            .get_eth_commit_tx_id(batch_to_prove)
            .await
        {
            Ok(Some(tx_id)) => tx_id,
            Ok(None) | Err(_) => return None, // No commit transaction, exit early.
        };

        let minor_version = match storage
            .blocks_dal()
            .get_batch_protocol_version_id(batch_to_prove)
            .await
        {
            Ok(Some(version)) => version,
            Ok(None) | Err(_) => {
                tracing::error!(
                    "Failed to retrieve protocol version for batch {}",
                    batch_to_prove
                );
                return None;
            }
        };

        let allowed_patch_versions = match storage
            .protocol_versions_dal()
            .get_patch_versions_for_vk(
                minor_version,
                l1_verifier_config.recursion_scheduler_level_vk_hash,
            )
            .await
        {
            Ok(versions) if !versions.is_empty() => versions,
            Ok(_) | Err(_) => {
                tracing::warn!(
                    "No patch version corresponds to the verification key on L1: {:?}",
                    l1_verifier_config.recursion_scheduler_level_vk_hash
                );
                return None;
            }
        };

        let allowed_versions: Vec<_> = allowed_patch_versions
            .into_iter()
            .map(|patch| ProtocolSemanticVersion {
                minor: minor_version,
                patch,
            })
            .collect();

        let proof = match self
            .load_wrapped_fri_proofs_for_range(batch_to_prove, &allowed_versions)
            .await
        {
            Some(proof) => proof,
            None => {
                tracing::error!("Failed to load proof for batch {}", batch_to_prove);
                return None;
            }
        };

        let previous_proven_batch_metadata = match storage
            .blocks_dal()
            .get_l1_batch_metadata(previous_proven_batch_number)
            .await
        {
            Ok(Some(metadata)) => metadata,
            Ok(None) => {
                tracing::error!(
                    "L1 batch #{} with submitted proof is not complete in the DB",
                    previous_proven_batch_number
                );
                return None;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to retrieve L1 batch #{} metadata: {}",
                    previous_proven_batch_number,
                    e
                );
                return None;
            }
        };

        let metadata_for_batch_being_proved = match storage
            .blocks_dal()
            .get_l1_batch_metadata(batch_to_prove)
            .await
        {
            Ok(Some(metadata)) => metadata,
            Ok(None) => {
                tracing::error!(
                    "L1 batch #{} with generated proof is not complete in the DB",
                    batch_to_prove
                );
                return None;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to retrieve L1 batch #{} metadata: {}",
                    batch_to_prove,
                    e
                );
                return None;
            }
        };

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
            match self
                .blob_store
                .get_by_encoded_key(format!("l1_batch_proof_{}.bin", l1_batch_number))
                .await
            {
                Ok(proof) => return Some(proof),
                Err(ObjectStoreError::KeyNotFound(_)) => (),
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
}
