use anyhow::anyhow;
use via_btc_client::types::{InscriptionMessage, L1BatchDAReferenceInput, ProofDAReferenceInput};
use zksync_config::ViaBtcSenderConfig;
use zksync_contracts::BaseSystemContractsHashes;
use zksync_dal::{Connection, Core, CoreDal};
use zksync_types::{
    btc_block::ViaBtcL1BlockDetails, btc_inscription_operations::ViaBtcInscriptionRequestType,
    L1BatchNumber, ProtocolVersionId, H256,
};

use crate::{
    aggregated_operations::ViaAggregatedOperation,
    config::{BLOCK_TIME_TO_COMMIT, BLOCK_TIME_TO_PROOF},
    publish_criterion::{
        TimestampDeadlineCriterion, ViaBtcL1BatchCommitCriterion, ViaNumberCriterion,
    },
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
            commit_l1_block_criteria: vec![
                Box::from(ViaNumberCriterion {
                    limit: config.max_aggregated_blocks_to_commit() as u32,
                }),
                Box::from(TimestampDeadlineCriterion {
                    deadline_seconds: BLOCK_TIME_TO_COMMIT,
                }),
            ],
            commit_proof_criteria: vec![
                Box::from(ViaNumberCriterion {
                    limit: config.max_aggregated_proofs_to_commit() as u32,
                }),
                Box::from(TimestampDeadlineCriterion {
                    deadline_seconds: BLOCK_TIME_TO_PROOF,
                }),
            ],
            config,
        }
    }

    pub async fn get_next_ready_operation(
        &mut self,
        storage: &mut Connection<'_, Core>,
        base_system_contracts_hashes: BaseSystemContractsHashes,
        protocol_version_id: ProtocolVersionId,
    ) -> anyhow::Result<Option<ViaAggregatedOperation>> {
        if let Some(op) = self.get_commit_proof_operation(storage).await? {
            Ok(Some(op))
        } else {
            Ok(self
                .get_commit_l1_batch_operation(
                    storage,
                    base_system_contracts_hashes,
                    protocol_version_id,
                )
                .await?)
        }
    }

    async fn get_commit_l1_batch_operation(
        &mut self,
        storage: &mut Connection<'_, Core>,
        base_system_contracts_hashes: BaseSystemContractsHashes,
        protocol_version_id: ProtocolVersionId,
    ) -> anyhow::Result<Option<ViaAggregatedOperation>> {
        let ready_for_commit_l1_batches = storage
            .via_blocks_dal()
            .get_ready_for_commit_l1_batches(
                self.config.max_aggregated_blocks_to_commit() as usize,
                base_system_contracts_hashes.bootloader,
                base_system_contracts_hashes.default_aa,
                protocol_version_id,
            )
            .await?;

        tracing::debug!(
            "Found {} l1 batches ready for commit",
            ready_for_commit_l1_batches.len()
        );

        validate_l1_batch_sequence(&ready_for_commit_l1_batches);

        tracing::debug!("Extracting ready subrange");
        if let Some(l1_batches) = extract_ready_subrange(
            &mut self.commit_l1_block_criteria,
            ready_for_commit_l1_batches,
        )
        .await
        {
            tracing::debug!("Extracted ready subrange");
            return Ok(Some(ViaAggregatedOperation::CommitL1BatchOnchain(
                l1_batches,
            )));
        }
        Ok(None)
    }

    async fn get_commit_proof_operation(
        &mut self,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<Option<ViaAggregatedOperation>> {
        let ready_for_commit_proof_l1_batches = storage
            .via_blocks_dal()
            .get_ready_for_commit_proof_l1_batches(
                self.config.max_aggregated_proofs_to_commit() as usize
            )
            .await?;

        validate_l1_batch_sequence(&ready_for_commit_proof_l1_batches);

        if let Some(l1_batches) = extract_ready_subrange(
            &mut self.commit_proof_criteria,
            ready_for_commit_proof_l1_batches.clone(),
        )
        .await
        {
            return Ok(Some(ViaAggregatedOperation::CommitProofOnchain(l1_batches)));
        }
        Ok(None)
    }

    pub fn construct_inscription_message(
        &self,
        inscription_request_type: &ViaBtcInscriptionRequestType,
        batch: &ViaBtcL1BlockDetails,
    ) -> anyhow::Result<InscriptionMessage> {
        match inscription_request_type {
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain => {
                let input = L1BatchDAReferenceInput {
                    l1_batch_hash: H256::from_slice(
                        batch
                            .hash
                            .as_ref()
                            .ok_or_else(|| anyhow!("Via l1 batch hash is None"))?,
                    ),
                    l1_batch_index: batch.number,
                    da_identifier: self.config.da_identifier().to_string(),
                    blob_id: batch.blob_id.clone(),
                };
                Ok(InscriptionMessage::L1BatchDAReference(input))
            }
            ViaBtcInscriptionRequestType::CommitProofOnchain => {
                let input = ProofDAReferenceInput {
                    l1_batch_reveal_txid: batch.reveal_tx_id,
                    da_identifier: self.config.da_identifier().to_string(),
                    blob_id: batch.blob_id.clone(),
                };
                Ok(InscriptionMessage::ProofDAReference(input))
            }
        }
    }
}

async fn extract_ready_subrange(
    publish_criteria: &mut [Box<dyn ViaBtcL1BatchCommitCriterion>],
    uncommited_l1_batches: Vec<ViaBtcL1BlockDetails>,
) -> Option<Vec<ViaBtcL1BlockDetails>> {
    let mut last_l1_batch: Option<L1BatchNumber> = None;
    for criterion in publish_criteria {
        let l1_batch_by_criterion = criterion
            .last_l1_batch_to_publish(&uncommited_l1_batches)
            .await;
        if let Some(l1_batch) = l1_batch_by_criterion {
            last_l1_batch = Some(last_l1_batch.map_or(l1_batch, |number| number.min(l1_batch)));
        }
    }

    let last_l1_batch = last_l1_batch?;
    Some(
        uncommited_l1_batches
            .into_iter()
            .take_while(|l1_batch| l1_batch.number <= last_l1_batch)
            .collect(),
    )
}

fn validate_l1_batch_sequence(ready_for_commit_l1_batches: &[ViaBtcL1BlockDetails]) {
    ready_for_commit_l1_batches
        .iter()
        .reduce(|last_batch, next_batch| {
            if last_batch.number + 1 == next_batch.number {
                next_batch
            } else {
                panic!("L1 batches prepared for commit are not sequential");
            }
        });
}
