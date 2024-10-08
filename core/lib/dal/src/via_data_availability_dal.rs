use zksync_db_connection::{
    connection::Connection,
    error::DalResult,
    instrument::{InstrumentExt, Instrumented},
};
use zksync_types::{pubdata_da::DataAvailabilityBlob, L1BatchNumber};

use crate::{
    models::storage_data_availability::{L1BatchDA, ProofDA, StorageDABlob},
    Core,
};

#[derive(Debug)]
pub struct ViaDataAvailabilityDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Core>,
}

impl ViaDataAvailabilityDal<'_, '_> {
    /// Inserts the blob_id for the given L1 batch. If the blob_id is already present,
    /// verifies that it matches the one provided in the function arguments
    /// (preventing the same L1 batch from being stored twice).
    /// This method handles the non-proof data (is_proof = FALSE).
    pub async fn insert_l1_batch_da(
        &mut self,
        number: L1BatchNumber,
        blob_id: &str,
        sent_at: chrono::NaiveDateTime,
    ) -> DalResult<()> {
        let update_result = sqlx::query!(
            r#"
            INSERT INTO
                via_data_availability (l1_batch_number, is_proof, blob_id, sent_at, created_at, updated_at)
            VALUES
                ($1, FALSE, $2, $3, NOW(), NOW())
            ON CONFLICT DO NOTHING
            "#,
            i64::from(number.0),
            blob_id,
            sent_at,
        )
        .instrument("insert_l1_batch_da")
        .with_arg("number", &number)
        .with_arg("blob_id", &blob_id)
        .report_latency()
        .execute(self.storage)
        .await?;

        if update_result.rows_affected() == 0 {
            tracing::debug!(
                "L1 batch #{number}: DA blob_id wasn't updated as it's already present"
            );

            let instrumentation =
                Instrumented::new("get_matching_batch_da_blob_id").with_arg("number", &number);

            // Batch was already processed. Verify that existing DA blob_id matches
            let query = sqlx::query!(
                r#"
                SELECT
                    blob_id
                FROM
                    via_data_availability
                WHERE
                    l1_batch_number = $1
                    AND is_proof = FALSE
                "#,
                i64::from(number.0),
            );

            let matched: String = instrumentation
                .clone()
                .with(query)
                .report_latency()
                .fetch_one(self.storage)
                .await?
                .blob_id;

            if matched != blob_id {
                let err = instrumentation.constraint_error(anyhow::anyhow!(
                    "Error storing DA blob id. DA blob_id {blob_id} for L1 batch #{number} does not match the expected value"
                ));
                return Err(err);
            }
        }
        Ok(())
    }

    /// Inserts the proof DA blob for the given L1 batch.
    /// This method handles the proof data (is_proof = TRUE).
    pub async fn insert_proof_da(
        &mut self,
        number: L1BatchNumber,
        blob_id: &str,
        sent_at: chrono::NaiveDateTime,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_data_availability (l1_batch_number, is_proof, blob_id, sent_at, created_at, updated_at)
            VALUES
                ($1, TRUE, $2, $3, NOW(), NOW())
            ON CONFLICT DO NOTHING
            "#,
            i64::from(number.0),
            blob_id,
            sent_at,
        )
        .instrument("insert_proof_da")
        .with_arg("number", &number)
        .with_arg("blob_id", &blob_id)
        .report_latency()
        .execute(self.storage)
        .await?;

        Ok(())
    }

    /// Saves the inclusion data for the given L1 batch. If the inclusion data is already present,
    /// verifies that it matches the one provided in the function arguments
    /// (meaning that the inclusion data corresponds to the same DA blob).
    /// This method handles the non-proof data (is_proof = FALSE).
    pub async fn save_l1_batch_inclusion_data(
        &mut self,
        number: L1BatchNumber,
        da_inclusion_data: &[u8],
    ) -> DalResult<()> {
        let update_result = sqlx::query!(
            r#"
            UPDATE via_data_availability
            SET
                inclusion_data = $1,
                updated_at = NOW()
            WHERE
                l1_batch_number = $2
                AND is_proof = FALSE
                AND inclusion_data IS NULL
            "#,
            da_inclusion_data,
            i64::from(number.0),
        )
        .instrument("save_l1_batch_inclusion_data")
        .with_arg("number", &number)
        .report_latency()
        .execute(self.storage)
        .await?;

        if update_result.rows_affected() == 0 {
            tracing::debug!("L1 batch #{number}: DA data wasn't updated as it's already present");

            let instrumentation =
                Instrumented::new("get_matching_batch_da_data").with_arg("number", &number);

            // Batch was already processed. Verify that existing DA data matches
            let query = sqlx::query!(
                r#"
                SELECT
                    inclusion_data
                FROM
                    via_data_availability
                WHERE
                    l1_batch_number = $1
                    AND is_proof = FALSE
                "#,
                i64::from(number.0),
            );

            let matched: Option<Vec<u8>> = instrumentation
                .clone()
                .with(query)
                .report_latency()
                .fetch_one(self.storage)
                .await?
                .inclusion_data;

            if matched.unwrap_or_default() != da_inclusion_data {
                let err = instrumentation.constraint_error(anyhow::anyhow!(
                    "Error storing DA inclusion data. DA data for L1 batch #{number} does not match the one provided before"
                ));
                return Err(err);
            }
        }
        Ok(())
    }

    /// Saves the inclusion data for the proof blob. If the inclusion data is already present,
    /// verifies that it matches the one provided in the function arguments.
    /// This method handles the proof data (is_proof = TRUE).
    pub async fn save_proof_inclusion_data(
        &mut self,
        number: L1BatchNumber,
        proof_inclusion_data: &[u8],
    ) -> DalResult<()> {
        let update_result = sqlx::query!(
            r#"
            UPDATE via_data_availability
            SET
                inclusion_data = $1,
                updated_at = NOW()
            WHERE
                l1_batch_number = $2
                AND is_proof = TRUE
                AND inclusion_data IS NULL
            "#,
            proof_inclusion_data,
            i64::from(number.0),
        )
        .instrument("save_proof_inclusion_data")
        .with_arg("number", &number)
        .report_latency()
        .execute(self.storage)
        .await?;

        if update_result.rows_affected() == 0 {
            tracing::debug!(
                "L1 batch #{number}: Proof DA data wasn't updated as it's already present"
            );

            let instrumentation =
                Instrumented::new("get_matching_proof_da_data").with_arg("number", &number);

            // Proof data was already processed. Verify that existing proof DA data matches
            let query = sqlx::query!(
                r#"
                SELECT
                    inclusion_data
                FROM
                    via_data_availability
                WHERE
                    l1_batch_number = $1
                    AND is_proof = TRUE
                "#,
                i64::from(number.0),
            );

            let matched: Option<Vec<u8>> = instrumentation
                .clone()
                .with(query)
                .report_latency()
                .fetch_one(self.storage)
                .await?
                .inclusion_data;

            if matched.unwrap_or_default() != proof_inclusion_data {
                let err = instrumentation.constraint_error(anyhow::anyhow!(
                    "Error storing proof DA inclusion data. Proof DA data for L1 batch #{number} does not match the one provided before"
                ));
                return Err(err);
            }
        }
        Ok(())
    }

    /// Returns the first L1 batch data availability blob that is awaiting inclusion.
    /// This method handles the non-proof data (`is_proof = FALSE`).
    pub async fn get_first_da_blob_awaiting_inclusion(
        &mut self,
    ) -> DalResult<Option<DataAvailabilityBlob>> {
        let result = sqlx::query_as!(
            StorageDABlob,
            r#"
            SELECT
                l1_batch_number,
                blob_id,
                inclusion_data,
                sent_at
            FROM
                via_data_availability
            WHERE
                inclusion_data IS NULL
                AND is_proof = FALSE
            ORDER BY
                l1_batch_number ASC
            LIMIT
                1
            "#,
        )
        .instrument("get_first_da_blob_awaiting_inclusion")
        .fetch_optional(self.storage)
        .await?;

        Ok(result.map(DataAvailabilityBlob::from))
    }

    /// Assumes that the L1 batches are sorted by number, and returns the first proof blob that is ready for inclusion.
    /// This method handles the proof data (is_proof = TRUE).
    pub async fn get_first_proof_blob_awaiting_inclusion(
        &mut self,
    ) -> DalResult<Option<DataAvailabilityBlob>> {
        Ok(sqlx::query_as!(
            StorageDABlob,
            r#"
            SELECT
                l1_batch_number,
                blob_id,
                inclusion_data,
                sent_at
            FROM
                via_data_availability
            WHERE
                inclusion_data IS NULL
                AND is_proof = TRUE
            ORDER BY
                l1_batch_number
            LIMIT
                1
            "#,
        )
        .instrument("get_first_proof_blob_awaiting_inclusion")
        .fetch_optional(self.storage)
        .await?
        .map(DataAvailabilityBlob::from))
    }

    /// Fetches the pubdata and `l1_batch_number` for the L1 batches that are ready for DA dispatch.
    /// This method handles the non-proof data (is_proof = FALSE).
    pub async fn get_ready_for_da_dispatch_l1_batches(
        &mut self,
        limit: usize,
    ) -> DalResult<Vec<L1BatchDA>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                number,
                pubdata_input
            FROM
                l1_batches
                LEFT JOIN via_data_availability ON via_data_availability.l1_batch_number = l1_batches.number
                LEFT JOIN via_l1_batch_inscription_request ON via_l1_batch_inscription_request.l1_batch_number = l1_batches.number
                AND via_data_availability.is_proof = FALSE
            WHERE
                via_l1_batch_inscription_request.commit_l1_batch_inscription_id IS NULL
                AND number != 0
                AND via_data_availability.blob_id IS NULL
                AND pubdata_input IS NOT NULL
            ORDER BY
                number
            LIMIT
                $1
            "#,
            limit as i64,
        )
        .instrument("get_ready_for_da_dispatch_l1_batches")
        .with_arg("limit", &limit)
        .fetch_all(self.storage)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| L1BatchDA {
                // `unwrap` is safe here because we have a `WHERE` clause that filters out `NULL` values
                pubdata: row.pubdata_input.unwrap(),
                l1_batch_number: L1BatchNumber(row.number as u32),
            })
            .collect())
    }

    pub async fn get_ready_for_da_dispatch_proofs(
        &mut self,
        limit: usize,
    ) -> DalResult<Vec<ProofDA>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                proof_generation_details.l1_batch_number,
                proof_generation_details.proof_blob_url
            FROM
                proof_generation_details
            WHERE
                proof_generation_details.status = 'generated'
                AND proof_generation_details.proof_blob_url IS NOT NULL
                AND EXISTS (
                    SELECT
                        1
                    FROM
                        via_data_availability
                    WHERE
                        l1_batch_number = proof_generation_details.l1_batch_number
                        AND is_proof = FALSE
                        AND blob_id IS NOT NULL
                )
                AND NOT EXISTS (
                    SELECT
                        1
                    FROM
                        via_data_availability
                    WHERE
                        l1_batch_number = proof_generation_details.l1_batch_number
                        AND is_proof = TRUE
                        AND blob_id IS NOT NULL
                )
            ORDER BY
                proof_generation_details.l1_batch_number
            LIMIT
                $1
            "#,
            limit as i64,
        )
        .instrument("get_ready_for_da_dispatch_proofs")
        .with_arg("limit", &limit)
        .fetch_all(self.storage)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| ProofDA {
                proof_blob_url: row.proof_blob_url.unwrap(),
                l1_batch_number: L1BatchNumber(row.l1_batch_number as u32),
            })
            .collect())
    }
}
