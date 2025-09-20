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
                    limit: config.max_aggregated_blocks_to_commit as u32,
                }),
                Box::from(TimestampDeadlineCriterion {
                    deadline_seconds: config.block_time_to_commit(),
                }),
            ],
            commit_proof_criteria: vec![
                Box::from(ViaNumberCriterion {
                    limit: config.max_aggregated_proofs_to_commit as u32,
                }),
                Box::from(TimestampDeadlineCriterion {
                    deadline_seconds: config.block_time_to_proof(),
                }),
            ],
            config,
        }
    }

    pub async fn get_next_ready_operation(
        &mut self,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<Option<ViaAggregatedOperation>> {
        if let Some(op) = self.get_commit_proof_operation(storage).await? {
            Ok(Some(op))
        } else {
            Ok(self.get_commit_l1_batch_operation(storage).await?)
        }
    }

    async fn get_commit_l1_batch_operation(
        &mut self,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<Option<ViaAggregatedOperation>> {
        let last_committed_l1_batch = storage
            .via_blocks_dal()
            .get_last_committed_to_btc_l1_batch()
            .await?;

        let ready_for_commit_l1_batches = self.get_ready_for_commit_l1_batches(storage).await?;

        if !ready_for_commit_l1_batches.is_empty() {
            tracing::debug!(
                "Found {} l1 batches ready for commit",
                ready_for_commit_l1_batches.len()
            );
        }

        validate_l1_batch_sequence(last_committed_l1_batch, &ready_for_commit_l1_batches)?;

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
        let last_committed_proof = storage
            .via_blocks_dal()
            .get_last_committed_proof_to_btc_l1_batch()
            .await?;

        let ready_for_commit_proof_l1_batches = storage
            .via_blocks_dal()
            .get_ready_for_commit_proof_l1_batches(
                self.config.max_aggregated_proofs_to_commit as usize,
            )
            .await?;

        validate_l1_batch_sequence(last_committed_proof, &ready_for_commit_proof_l1_batches)?;

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
                    prev_l1_batch_hash: H256::from_slice(
                        batch
                            .prev_l1_batch_hash
                            .as_ref()
                            .ok_or_else(|| anyhow!("Via previous l1 batch hash is None"))?,
                    ),
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

    async fn get_ready_for_commit_l1_batches(
        &self,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<Vec<ViaBtcL1BlockDetails>> {
        let protocol_version_id = self.get_last_protocol_version_id(storage).await?;
        let prev_protocol_version_id = self.get_prev_used_protocol_version(storage).await?;

        let base_system_contracts_hashes = self
            .load_base_system_contracts(storage, protocol_version_id)
            .await?;

        // In case of a protocol upgrade, we first process the l1 batches created with the previous protocol version
        // then switch to the new one.
        if prev_protocol_version_id != protocol_version_id {
            let prev_base_system_contracts_hashes = self
                .load_base_system_contracts(storage, prev_protocol_version_id)
                .await?;
            let ready_for_commit_l1_batches = storage
                .via_blocks_dal()
                .get_ready_for_commit_l1_batches(
                    self.config.max_aggregated_blocks_to_commit as usize,
                    &prev_base_system_contracts_hashes.bootloader,
                    &prev_base_system_contracts_hashes.default_aa,
                    prev_protocol_version_id,
                )
                .await?;

            if !ready_for_commit_l1_batches.is_empty() {
                return Ok(ready_for_commit_l1_batches);
            }
        }

        let ready_for_commit_l1_batches = storage
            .via_blocks_dal()
            .get_ready_for_commit_l1_batches(
                self.config.max_aggregated_blocks_to_commit as usize,
                &base_system_contracts_hashes.bootloader,
                &base_system_contracts_hashes.default_aa,
                protocol_version_id,
            )
            .await?;
        Ok(ready_for_commit_l1_batches)
    }

    async fn load_base_system_contracts(
        &self,
        storage: &mut Connection<'_, Core>,
        protocol_version: ProtocolVersionId,
    ) -> anyhow::Result<BaseSystemContractsHashes> {
        let base_system_contracts = storage
            .protocol_versions_dal()
            .load_base_system_contracts_by_version_id(protocol_version as u16)
            .await?;
        if let Some(contracts) = base_system_contracts {
            return Ok(BaseSystemContractsHashes {
                bootloader: contracts.bootloader.hash,
                default_aa: contracts.default_aa.hash,
                evm_emulator: contracts.evm_emulator.map(|c| c.hash),
            });
        }
        anyhow::bail!(
            "Failed to load the base system contracts for version {}",
            protocol_version
        )
    }

    async fn get_prev_used_protocol_version(
        &self,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<ProtocolVersionId> {
        let prev_protocol_version_id_opt = storage
            .via_blocks_dal()
            .prev_used_protocol_version_id_to_commit_l1_batch()
            .await?;
        if let Some(prev_protocol_version_id) = prev_protocol_version_id_opt {
            return Ok(prev_protocol_version_id);
        }
        self.get_last_protocol_version_id(storage).await
    }

    async fn get_last_protocol_version_id(
        &self,
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<ProtocolVersionId> {
        let protocol_version_id_opt = storage
            .protocol_versions_dal()
            .latest_semantic_version()
            .await?;
        if let Some(protocol_version_id) = protocol_version_id_opt {
            return Ok(protocol_version_id.minor);
        }
        anyhow::bail!("Failed to get the previous protocol version");
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
        if l1_batch_by_criterion.is_none() {
            return None;
        }

        if let Some(l1_batch) = l1_batch_by_criterion {
            last_l1_batch = Some(last_l1_batch.map_or(l1_batch, |number| number.min(l1_batch)));
        }
    }

    if let Some(last_l1_batch_number) = last_l1_batch {
        return Some(
            uncommited_l1_batches
                .into_iter()
                .take_while(|l1_batch| l1_batch.number <= last_l1_batch_number)
                .collect(),
        );
    }
    None
}

fn validate_l1_batch_sequence(
    last_committed_l1_batch_opt: Option<ViaBtcL1BlockDetails>,
    ready_for_commit_l1_batches: &[ViaBtcL1BlockDetails],
) -> anyhow::Result<()> {
    let mut all_batches = vec![];
    // The last_committed_l1_batch should be empty only in case of genesis.
    if let Some(last_committed_l1_batch) = last_committed_l1_batch_opt {
        all_batches.extend_from_slice(&[last_committed_l1_batch.clone()]);
    } else if let Some(batch) = ready_for_commit_l1_batches.first() {
        if batch.number.0 != 1 {
            anyhow::bail!("Invalid batch after genesis, not sequential")
        }
    }

    all_batches.extend_from_slice(ready_for_commit_l1_batches);

    for i in 1..all_batches.len() {
        let last_batch = &all_batches[i - 1];
        let next_batch = &all_batches[i];

        if last_batch.number + 1 != next_batch.number {
            anyhow::bail!(
                "L1 batches prepared for commit or proof batch numbers are not sequential"
            );
        }
        if last_batch.hash != next_batch.prev_l1_batch_hash {
            anyhow::bail!(
                "L1 batches prepared for commit or proof batch hashes are not sequential"
            );
        }
    }

    Ok(())
}
