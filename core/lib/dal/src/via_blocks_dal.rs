use zksync_db_connection::{
    connection::Connection,
    error::DalResult,
    instrument::{InstrumentExt, Instrumented},
};
use zksync_types::{
    btc_block::ViaBtcL1BlockDetails, btc_inscription_operations::ViaBtcInscriptionRequestType,
    L1BatchNumber, ProtocolVersionId, H256,
};

pub use crate::models::storage_block::{L1BatchMetadataError, L1BatchWithOptionalMetadata};
use crate::{models::storage_btc_block::ViaBtcStorageL1BlockDetails, Core};

#[derive(Debug)]
pub struct ViaBlocksDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Core>,
}

impl ViaBlocksDal<'_, '_> {
    pub async fn set_inscription_request_id(
        &mut self,
        batch_number: L1BatchNumber,
        inscription_request_id: i64,
        inscription_request: ViaBtcInscriptionRequestType,
    ) -> DalResult<()> {
        match inscription_request {
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain => {
                let instrumentation = Instrumented::new("set_eth_tx_id#commit")
                    .with_arg("batch_number", &batch_number)
                    .with_arg("inscription_request_id", &inscription_request_id);

                let query = sqlx::query!(
                    r#"
                    UPDATE l1_batches
                    SET
                        eth_commit_tx_id = $1,
                        updated_at = NOW()
                    WHERE
                        number = $2
                        AND eth_commit_tx_id IS NULL
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
                        "Update eth_commit_tx_id that is is not null is not allowed"
                    ));
                    return Err(err);
                }
                Ok(())
            }
            ViaBtcInscriptionRequestType::CommitProofOnchain => {
                let instrumentation = Instrumented::new("set_eth_tx_id#prove")
                    .with_arg("batch_number", &batch_number)
                    .with_arg("inscription_request_id", &inscription_request_id);
                let query = sqlx::query!(
                    r#"
                    UPDATE l1_batches
                    SET
                        eth_prove_tx_id = $1,
                        updated_at = NOW()
                    WHERE
                        number = $2
                        AND eth_prove_tx_id IS NULL
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
                        "Update eth_prove_tx_id that is is not null is not allowed"
                    ));
                    return Err(err);
                }
                Ok(())
            }
        }
    }

    pub async fn get_inscription_commit_tx_id(
        &mut self,
        l1_batch_number: L1BatchNumber,
    ) -> DalResult<Option<u64>> {
        let row = sqlx::query!(
            r#"
            SELECT
                eth_commit_tx_id
            FROM
                l1_batches
            WHERE
                number = $1
            "#,
            i64::from(l1_batch_number.0)
        )
        .instrument("get_inscription_commit_tx_id")
        .with_arg("l1_batch_number", &l1_batch_number)
        .fetch_optional(self.storage)
        .await?;

        Ok(row.and_then(|row| row.eth_commit_tx_id.map(|n| n as u64)))
    }

    pub async fn get_ready_for_commit_l1_batches(
        &mut self,
        limit: usize,
        bootloader_hash: H256,
        default_aa_hash: H256,
        protocol_version_id: ProtocolVersionId,
    ) -> anyhow::Result<Vec<ViaBtcL1BlockDetails>> {
        let batches = sqlx::query_as!(
            ViaBtcStorageL1BlockDetails,
            r#"
            SELECT
                number,
                l1_batches.timestamp AS timestamp,
                hash,
                '' AS commit_tx_id,
                '' AS reveal_tx_id,
                via_data_availability.blob_id
            FROM
                l1_batches
                LEFT JOIN commitments ON commitments.l1_batch_number = l1_batches.number
                LEFT JOIN via_data_availability ON via_data_availability.l1_batch_number = l1_batches.number
                JOIN protocol_versions ON protocol_versions.id = l1_batches.protocol_version
            WHERE
                eth_commit_tx_id IS NULL
                AND number != 0
                AND protocol_versions.bootloader_code_hash = $1
                AND protocol_versions.default_account_code_hash = $2
                AND via_data_availability.is_proof = FALSE
                AND commitment IS NOT NULL
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
        .with_arg("limit", &limit)
        .with_arg("bootloader_hash", &bootloader_hash)
        .with_arg("default_aa_hash", &default_aa_hash)
        .with_arg("protocol_version_id", &protocol_version_id)
        .fetch_all(self.storage)
        .await?;

        Ok(batches.into_iter().map(|details| details.into()).collect())
    }

    pub async fn get_ready_for_commit_proof_l1_batches(
        &mut self,
        limit: usize,
    ) -> anyhow::Result<Vec<ViaBtcL1BlockDetails>> {
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
                lh.commit_tx_id,
                lh.reveal_tx_id,
                via_data_availability.blob_id
            FROM
                l1_batches
                LEFT JOIN via_data_availability ON via_data_availability.l1_batch_number = l1_batches.number
                LEFT JOIN via_btc_inscriptions_request ON l1_batches.eth_commit_tx_id = via_btc_inscriptions_request.id
                LEFT JOIN (
                    SELECT
                        *
                    FROM
                        latest_history
                    WHERE
                        rn = 1
                ) AS lh ON via_btc_inscriptions_request.id = lh.inscription_request_id
            WHERE
                eth_commit_tx_id IS NOT NULL
                AND eth_prove_tx_id IS NULL
                AND via_data_availability.is_proof = TRUE
            ORDER BY
                number
            LIMIT
                $1
            "#,
            limit as i64,
        )
        .instrument("get_ready_for_commit_l1_batches")
        .with_arg("limit", &limit)
        .fetch_all(self.storage)
        .await?;

        Ok(batches.into_iter().map(|details| details.into()).collect())
    }
}
