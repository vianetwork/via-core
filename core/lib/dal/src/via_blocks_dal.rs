use zksync_db_connection::{
    connection::Connection,
    error::DalResult,
    instrument::{InstrumentExt, Instrumented},
};
use zksync_types::{
    block::L1BatchStatistics, btc_block::ViaBtcL1BlockDetails,
    btc_inscription_operations::ViaBtcInscriptionRequestType, L1BatchNumber, ProtocolVersionId,
    H256,
};

pub use crate::models::storage_block::{L1BatchMetadataError, L1BatchWithOptionalMetadata};
use crate::{models::storage_btc_block::ViaBtcStorageL1BlockDetails, Core};

#[derive(Debug)]
pub struct ViaBlocksDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Core>,
}

impl ViaBlocksDal<'_, '_> {
    /// Inserts an inscription request ID for a given L1 batch.
    /// Handles both commit batch and commit proof inscription types.
    pub async fn insert_l1_batch_inscription_request_id(
        &mut self,
        batch_number: L1BatchNumber,
        inscription_request_id: i64,
        inscription_request: ViaBtcInscriptionRequestType,
    ) -> DalResult<()> {
        match inscription_request {
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain => {
                let instrumentation = Instrumented::new("set_inscription_request_tx_id#commit")
                    .with_arg("batch_number", &batch_number)
                    .with_arg("inscription_request_id", &inscription_request_id);

                let query = sqlx::query!(
                    r#"
                    INSERT INTO
                        via_l1_batch_inscription_request (l1_batch_number, commit_l1_batch_inscription_id, created_at, updated_at)
                    VALUES
                        ($1, $2, NOW(), NOW())
                    ON CONFLICT DO NOTHING
                    "#,
                    i64::from(batch_number.0),
                    inscription_request_id as i32,
                );
                let result = instrumentation
                    .clone()
                    .with(query)
                    .execute(self.storage)
                    .await?;

                if result.rows_affected() == 0 {
                    let err = instrumentation.constraint_error(anyhow::anyhow!(
                        "Update commit_l1_batch_inscription_id that is is not null is not allowed"
                    ));
                    return Err(err);
                }
                Ok(())
            }
            ViaBtcInscriptionRequestType::CommitProofOnchain => {
                let instrumentation = Instrumented::new("set_inscription_request_tx_id#prove")
                    .with_arg("batch_number", &batch_number)
                    .with_arg("inscription_request_id", &inscription_request_id);
                let query = sqlx::query!(
                    r#"
                    UPDATE via_l1_batch_inscription_request
                    SET
                        commit_proof_inscription_id = $1,
                        updated_at = NOW()
                    WHERE
                        l1_batch_number = $2
                        AND commit_l1_batch_inscription_id IS NOT NULL
                        AND commit_proof_inscription_id IS NULL
                    "#,
                    inscription_request_id as i32,
                    i64::from(batch_number.0),
                );

                let result = instrumentation
                    .clone()
                    .with(query)
                    .execute(self.storage)
                    .await?;

                if result.rows_affected() == 0 {
                    let err = instrumentation.constraint_error(anyhow::anyhow!(
                        "Update commit_proof_inscription_id that is is not null is not allowed"
                    ));
                    return Err(err);
                }
                Ok(())
            }
        }
    }

    /// Retrieves L1 batches that are ready to have their pubdata committed to bitcoin chain.
    /// Filters batches based on protocol version and required commitments.
    pub async fn get_ready_for_commit_l1_batches(
        &mut self,
        limit: usize,
        bootloader_hash: &H256,
        default_aa_hash: &H256,
        protocol_version_id: ProtocolVersionId,
    ) -> DalResult<Vec<ViaBtcL1BlockDetails>> {
        let batches = sqlx::query_as!(
            ViaBtcStorageL1BlockDetails,
            r#"
            SELECT
                l1_batches.number AS number,
                l1_batches.timestamp AS timestamp,
                l1_batches.hash AS hash,
                ''::bytea AS commit_tx_id,
                ''::bytea AS reveal_tx_id,
                via_data_availability.blob_id,
                prev_l1_batches.hash AS prev_l1_batch_hash
            FROM
                l1_batches
                LEFT JOIN l1_batches prev_l1_batches ON prev_l1_batches.number = l1_batches.number - 1
                LEFT JOIN via_l1_batch_inscription_request ON via_l1_batch_inscription_request.l1_batch_number = l1_batches.number
                LEFT JOIN commitments ON commitments.l1_batch_number = l1_batches.number
                LEFT JOIN via_data_availability ON via_data_availability.l1_batch_number = l1_batches.number
                JOIN protocol_versions ON protocol_versions.id = l1_batches.protocol_version
            WHERE
                commit_l1_batch_inscription_id IS NULL
                AND l1_batches.number != 0
                AND protocol_versions.bootloader_code_hash = $1
                AND protocol_versions.default_account_code_hash = $2
                AND via_data_availability.is_proof = FALSE
                AND events_queue_commitment IS NOT NULL
                AND (
                    protocol_versions.id = $3
                    OR protocol_versions.upgrade_tx_hash IS NULL
                )
                AND events_queue_commitment IS NOT NULL
                AND bootloader_initial_content_commitment IS NOT NULL
                AND via_data_availability.inclusion_data IS NOT NULL
            ORDER BY
                number
            LIMIT
                $4
            "#,
            bootloader_hash.as_bytes(),
            default_aa_hash.as_bytes(),
            protocol_version_id as i32,
            limit as i64,
        )
        .instrument("get_ready_for_commit_l1_batches")
        .report_latency()
        .with_arg("limit", &limit)
        .with_arg("bootloader_hash", &bootloader_hash)
        .with_arg("default_aa_hash", &default_aa_hash)
        .with_arg("protocol_version_id", &protocol_version_id)
        .fetch_all(self.storage)
        .await?;

        Ok(batches.into_iter().map(|details| details.into()).collect())
    }

    /// Retrieves L1 batches that are ready to have their proofs committed to bitcoin chain.
    pub async fn get_ready_for_commit_proof_l1_batches(
        &mut self,
        limit: usize,
    ) -> DalResult<Vec<ViaBtcL1BlockDetails>> {
        let batches = sqlx::query_as!(
            ViaBtcStorageL1BlockDetails,
            r#"
            WITH
                latest_history AS (
                    SELECT
                        *,
                        ROW_NUMBER() OVER (
                            PARTITION BY
                                inscription_request_id
                            ORDER BY
                                created_at DESC
                        ) AS rn
                    FROM
                        via_btc_inscriptions_request_history
                )
            SELECT
                l1_batches.number,
                l1_batches.timestamp,
                l1_batches.hash,
                COALESCE(lh.commit_tx_id, '') AS commit_tx_id,
                COALESCE(lh.reveal_tx_id, '') AS reveal_tx_id,
                via_data_availability.blob_id,
                prev_l1_batches.hash AS prev_l1_batch_hash
            FROM
                l1_batches
                LEFT JOIN l1_batches prev_l1_batches ON prev_l1_batches.number = l1_batches.number - 1
                LEFT JOIN via_l1_batch_inscription_request ON via_l1_batch_inscription_request.l1_batch_number = l1_batches.number
                LEFT JOIN via_data_availability ON via_data_availability.l1_batch_number = l1_batches.number
                LEFT JOIN via_btc_inscriptions_request ON via_l1_batch_inscription_request.commit_l1_batch_inscription_id = via_btc_inscriptions_request.id
                LEFT JOIN (
                    SELECT
                        *
                    FROM
                        latest_history
                    WHERE
                        rn = 1
                ) AS lh ON via_btc_inscriptions_request.id = lh.inscription_request_id
            WHERE
                via_l1_batch_inscription_request.commit_l1_batch_inscription_id IS NOT NULL
                AND via_l1_batch_inscription_request.commit_proof_inscription_id IS NULL
                AND via_btc_inscriptions_request.confirmed_inscriptions_request_history_id IS NOT NULL
                AND via_data_availability.is_proof = TRUE
            ORDER BY
                number
            LIMIT
                $1
            "#,
            limit as i64,
        )
        .instrument("get_ready_for_commit_proof_l1_batches")
        .report_latency()
        .with_arg("limit", &limit)
        .fetch_all(self.storage)
        .await?;

        Ok(batches.into_iter().map(|details| details.into()).collect())
    }

    /// Returns the first L1 batch number that has been reverted by the verifier network.
    /// Returns None if no batches have been reverted.
    pub async fn get_reverted_batch_by_verifier_network(
        &mut self,
    ) -> DalResult<Option<L1BatchNumber>> {
        let row = sqlx::query!(
            r#"
            SELECT
                MIN(l1_batch_number) AS l1_batch_number
            FROM
                via_l1_batch_inscription_request
            WHERE
                is_finalized = FALSE
            "#
        )
        .instrument("get_reverted_batch_by_verifier_network")
        .report_latency()
        .fetch_one(self.storage)
        .await?;

        if let Some(l1_batch_number) = row.l1_batch_number {
            return Ok(Some(L1BatchNumber(l1_batch_number as u32)));
        }
        Ok(None)
    }

    /// Returns the first L1 batch number that doesn't have its proof committed.
    /// Returns None if all batches have their proofs committed.
    pub async fn get_l1_batch_proof_not_commited(&mut self) -> DalResult<Option<L1BatchNumber>> {
        let row = sqlx::query!(
            r#"
            SELECT
                MIN(number) AS l1_batch_number
            FROM
                l1_batches
                LEFT JOIN via_l1_batch_inscription_request ON number = l1_batch_number
            WHERE
                commit_proof_inscription_id IS NULL
                AND number != 0
            "#
        )
        .instrument("get_l1_batch_proof_not_commited")
        .report_latency()
        .fetch_one(self.storage)
        .await?;

        if let Some(l1_batch_number) = row.l1_batch_number {
            return Ok(Some(L1BatchNumber(l1_batch_number as u32)));
        }
        Ok(None)
    }

    /// Returns the first L1 batch that can be reverted, checking both verifier network
    /// reverts and uncommitted proofs.
    pub async fn get_first_l1_batch_can_be_reverted(&mut self) -> DalResult<Option<L1BatchNumber>> {
        if let Some(l1_batch_number) = self.get_reverted_batch_by_verifier_network().await? {
            return Ok(Some(l1_batch_number));
        } else if let Some(l1_batch_number) = self.get_l1_batch_proof_not_commited().await? {
            return Ok(Some(l1_batch_number));
        }
        Ok(None)
    }

    /// Checks if a proof transaction exists for a given L1 batch number.
    pub async fn l1_batch_proof_tx_exists(
        &mut self,
        l1_batch_number: i64,
        proof_reveal_tx_id: &[u8],
    ) -> DalResult<bool> {
        let row = sqlx::query!(
            r#"
            SELECT
                EXISTS (
                    SELECT
                        1
                    FROM
                        via_l1_batch_inscription_request ir
                        LEFT JOIN via_btc_inscriptions_request a ON ir.commit_proof_inscription_id = a.id
                        LEFT JOIN via_btc_inscriptions_request_history irh ON irh.id = a.confirmed_inscriptions_request_history_id
                    WHERE
                        ir.l1_batch_number = $1
                        AND irh.reveal_tx_id = $2
                )
            "#,
            l1_batch_number,
            proof_reveal_tx_id
        )
        .instrument("l1_batch_proof_tx_exists")
        .report_latency()
        .fetch_one(self.storage)
        .await?;

        Ok(row.exists.unwrap())
    }

    pub async fn prev_used_protocol_version_id_to_commit_l1_batch(
        &mut self,
    ) -> DalResult<Option<ProtocolVersionId>> {
        let protocol_version_opt = sqlx::query_scalar!(
            r#"
            SELECT
                protocol_version
            FROM
                l1_batches
            LEFT JOIN
                via_l1_batch_inscription_request ON via_l1_batch_inscription_request.l1_batch_number = l1_batches.number
            WHERE
                via_l1_batch_inscription_request.commit_l1_batch_inscription_id IS NOT NULL
            ORDER BY
                number DESC
            LIMIT
                1
            "#
        )
        .instrument("prev_used_protocol_version_id_to_commit_l1_batch")
        .fetch_optional(self.storage)
        .await?;

        Ok(protocol_version_opt
            .flatten()
            .and_then(|protocol_version| ProtocolVersionId::try_from(protocol_version as u16).ok()))
    }

    pub async fn get_last_committed_to_btc_l1_batch(
        &mut self,
    ) -> DalResult<Option<ViaBtcL1BlockDetails>> {
        let batch = sqlx::query_as!(
            ViaBtcStorageL1BlockDetails,
            r#"
            SELECT
                l1_batches.number AS number,
                l1_batches.timestamp AS timestamp,
                l1_batches.hash AS hash,
                ''::bytea AS commit_tx_id,
                ''::bytea AS reveal_tx_id,
                '' AS blob_id,
                prev_l1_batches.hash AS prev_l1_batch_hash
            FROM
                l1_batches
                LEFT JOIN l1_batches prev_l1_batches ON prev_l1_batches.number = l1_batches.number - 1
                LEFT JOIN via_l1_batch_inscription_request ON via_l1_batch_inscription_request.l1_batch_number = l1_batches.number
                LEFT JOIN commitments ON commitments.l1_batch_number = l1_batches.number
            WHERE
                via_l1_batch_inscription_request.commit_l1_batch_inscription_id IS NOT NULL
                AND events_queue_commitment IS NOT NULL
            ORDER BY
                l1_batches.number DESC
            LIMIT
                1
            "#,
        )
        .instrument("get_last_committed_to_btc_l1_batch")
        .fetch_optional(self.storage)
        .await?;

        Ok(batch.map(|details| details.into()))
    }

    pub async fn get_last_committed_proof_to_btc_l1_batch(
        &mut self,
    ) -> DalResult<Option<ViaBtcL1BlockDetails>> {
        let batch = sqlx::query_as!(
            ViaBtcStorageL1BlockDetails,
            r#"
            SELECT
                l1_batches.number AS number,
                l1_batches.timestamp AS timestamp,
                l1_batches.hash AS hash,
                ''::bytea AS commit_tx_id,
                ''::bytea AS reveal_tx_id,
                '' AS blob_id,
                prev_l1_batches.hash AS prev_l1_batch_hash
            FROM
                l1_batches
                LEFT JOIN l1_batches prev_l1_batches ON prev_l1_batches.number = l1_batches.number - 1
                LEFT JOIN via_l1_batch_inscription_request ON via_l1_batch_inscription_request.l1_batch_number = l1_batches.number
            WHERE
                via_l1_batch_inscription_request.commit_l1_batch_inscription_id IS NOT NULL
                AND via_l1_batch_inscription_request.commit_proof_inscription_id IS NOT NULL
            ORDER BY
                l1_batches.number DESC
            LIMIT
                1
            "#,
        )
        .instrument("get_last_committed_to_btc_l1_batch")
        .fetch_optional(self.storage)
        .await?;
        Ok(batch.map(|details| details.into()))
    }

    pub async fn get_l1_batches_statistics_for_inscription_tx_id(
        &mut self,
        inscription_id: u32,
    ) -> DalResult<Vec<L1BatchStatistics>> {
        Ok(sqlx::query!(
            r#"
            SELECT
                number,
                l1_tx_count,
                l2_tx_count,
                timestamp
            FROM
                l1_batches
                LEFT JOIN via_l1_batch_inscription_request ON l1_batches.number = via_l1_batch_inscription_request.l1_batch_number
            WHERE
                commit_l1_batch_inscription_id = $1
                OR commit_proof_inscription_id = $1
            "#,
            inscription_id as i32
        )
        .instrument("get_l1_batches_statistics_for_inscription_tx_id")
        .with_arg("inscription_id", &inscription_id)
        .fetch_all(self.storage)
        .await?
        .into_iter()
        .map(|row| L1BatchStatistics {
            number: L1BatchNumber(row.number as u32),
            timestamp: row.timestamp as u64,
            l2_tx_count: row.l2_tx_count as u32,
            l1_tx_count: row.l1_tx_count as u32,
        })
        .collect())
    }

    pub async fn get_last_finalized_l1_batch(&mut self) -> DalResult<u32> {
        let row = sqlx::query!(
            r#"
            SELECT
                MAX(l1_batch_number) AS l1_batch_number
            FROM
                via_l1_batch_inscription_request
            WHERE
                is_finalized = TRUE
            "#
        )
        .instrument("get_last_finalized_l1_batch")
        .report_latency()
        .fetch_one(self.storage)
        .await?;

        Ok(row.l1_batch_number.unwrap_or(0) as u32)
    }

    pub async fn get_first_stuck_l1_batch_number_inscription_request(
        &mut self,
        delay_btc_blocks: u32,
        current_btc_blocks: u64,
    ) -> DalResult<u32> {
        let record = sqlx::query_scalar!(
            r#"
            SELECT 
                MIN(l1_batch_number) as l1_batch_number
            FROM
                via_btc_inscriptions_request
            LEFT JOIN
                via_btc_inscriptions_request_history
            ON
                via_btc_inscriptions_request.id = via_btc_inscriptions_request_history.inscription_request_id
            WHERE
                sent_at_block + $1 < $2 
            "#,
            i64::from(delay_btc_blocks),
            current_btc_blocks as i64
        )
        .instrument("get_first_stuck_l1_batch_number_inscription_request")
        .fetch_one(self.storage)
        .await?;

        Ok(record.unwrap_or(0) as u32)
    }
}
