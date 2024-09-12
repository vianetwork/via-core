use anyhow::Context;
use via_btc_client::types::{InscriptionMessage, L1BatchDAReferenceInput, ProofDAReferenceInput};
use zksync_config::ViaBtcSenderConfig;
use zksync_contracts::BaseSystemContractsHashes;
use zksync_dal::{Connection, Core, CoreDal};
use zksync_types::{
    btc_inscription_operations::ViaBtcInscriptionRequestType, commitment::L1BatchWithMetadata,
    L1BatchNumber, ProtocolVersionId,
};

use crate::{
    aggregated_operations::ViaAggregatedOperation,
    publish_criterion::{ViaBtcL1BatchCommitCriterion, ViaNumberCriterion},
};

#[derive(Debug)]
pub struct ViaAggregator {
    commit_l1_block_criteria: Vec<Box<dyn ViaBtcL1BatchCommitCriterion>>,
    commit_proof_criteria: Vec<Box<dyn ViaBtcL1BatchCommitCriterion>>,
    config: ViaBtcSenderConfig,
}

impl ViaAggregator {
    pub fn new(config: ViaBtcSenderConfig) -> Self {
        Self {
            commit_l1_block_criteria: vec![Box::from(ViaNumberCriterion {
                op: ViaBtcInscriptionRequestType::CommitL1BatchOnchain,
                limit: config.max_aggregated_blocks_to_commit(),
            })],
            commit_proof_criteria: vec![Box::from(ViaNumberCriterion {
                op: ViaBtcInscriptionRequestType::CommitProofOnchain,
                limit: config.max_aggregated_proofs_to_commit(),
            })],
            config,
        }
    }

    pub async fn get_next_ready_operation(
        &mut self,
        storage: &mut Connection<'_, Core>,
        base_system_contracts_hashes: BaseSystemContractsHashes,
        protocol_version_id: ProtocolVersionId,
    ) -> Option<ViaAggregatedOperation> {
        if let Some(op) = self.get_commit_proof_operation(storage).await {
            Some(op)
        } else {
            self.get_commit_l1_batch_operation(
                storage,
                base_system_contracts_hashes,
                protocol_version_id,
            )
            .await
        }
    }

    async fn get_commit_l1_batch_operation(
        &mut self,
        storage: &mut Connection<'_, Core>,
        base_system_contracts_hashes: BaseSystemContractsHashes,
        protocol_version_id: ProtocolVersionId,
    ) -> Option<ViaAggregatedOperation> {
        let ready_for_commit_l1_batches = storage
            .blocks_dal()
            .get_ready_for_commit_l1_batches(
                self.config.max_aggregated_blocks_to_commit() as usize,
                base_system_contracts_hashes.bootloader,
                base_system_contracts_hashes.default_aa,
                protocol_version_id,
                true,
            )
            .await
            .unwrap();

        if let Some(l1_batches) = extract_ready_subrange(
            storage,
            &mut self.commit_l1_block_criteria,
            ready_for_commit_l1_batches,
        )
        .await
        {
            // Todo: update the 'da_identifier' and 'blob_id.
            let batch = l1_batches[0].clone();
            let input = L1BatchDAReferenceInput {
                l1_batch_hash: batch.metadata.root_hash,
                l1_batch_index: batch.header.number,
                da_identifier: "da_identifier_celestia".to_string(),
                blob_id: "batch_temp_blob_id".to_string(),
            };

            return Some(ViaAggregatedOperation::CommitL1BatchOnchain(
                batch,
                InscriptionMessage::L1BatchDAReference(input),
            ));
        }
        None
    }

    async fn get_commit_proof_operation(
        &mut self,
        storage: &mut Connection<'_, Core>,
    ) -> Option<ViaAggregatedOperation> {
        let l1_batches = storage
            .blocks_dal()
            .get_ready_for_dummy_proof_l1_batches(
                self.config.max_aggregated_proofs_to_commit() as usize
            )
            .await
            .unwrap();

        if let Some(l1_batches) =
            extract_ready_subrange(storage, &mut self.commit_proof_criteria, l1_batches.clone())
                .await
        {
            let batch = l1_batches[0].clone();

            let batch_details = storage
                .via_blocks_dal()
                .get_block_commit_details(batch.header.number.0 as i64)
                .await;

            match batch_details.context("Error get batch details") {
                Ok(b) => {
                    let inputs = ProofDAReferenceInput {
                        l1_batch_reveal_txid: b.unwrap().reveal_tx_id,
                        da_identifier: "da_identifier_celestia".to_string(),
                        blob_id: "proof_temp_blob_id".to_string(),
                    };
                    return Some(ViaAggregatedOperation::CommitProofOnchain(
                        batch,
                        InscriptionMessage::ProofDAReference(inputs),
                    ));
                }
                Err(_) => (),
            }
        }
        None
    }
}

async fn extract_ready_subrange(
    storage: &mut Connection<'_, Core>,
    publish_criteria: &mut [Box<dyn ViaBtcL1BatchCommitCriterion>],
    uncommited_l1_batches: Vec<L1BatchWithMetadata>,
) -> Option<Vec<L1BatchWithMetadata>> {
    let mut last_l1_batch: Option<L1BatchNumber> = None;
    for criterion in publish_criteria {
        let l1_batch_by_criterion = criterion
            .last_l1_batch_to_publish(storage, &uncommited_l1_batches)
            .await;
        if let Some(l1_batch) = l1_batch_by_criterion {
            last_l1_batch = Some(last_l1_batch.map_or(l1_batch, |number| number.min(l1_batch)));
        }
    }

    let last_l1_batch = last_l1_batch?;
    Some(
        uncommited_l1_batches
            .into_iter()
            .take_while(|l1_batch| l1_batch.header.number <= last_l1_batch)
            .collect(),
    )
}