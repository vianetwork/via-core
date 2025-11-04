use zksync_db_connection::{
    connection::Connection,
    error::DalResult,
    instrument::{InstrumentExt, Instrumented},
};
use zksync_l1_contract_interface::i_executor::methods::ProveBatches;
use zksync_types::{
    protocol_version::ProtocolSemanticVersion, pubdata_da::DataAvailabilityBlob, L1BatchNumber,
};

use crate::{
    models::storage_data_availability::{L1BatchDA, ProofDA, ViaStorageDABlob},
    Core, CoreDal,
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
        index: i32,
    ) -> DalResult<()> {
        let update_result = sqlx::query!(
            r#"
            INSERT INTO
            via_data_availability (
                l1_batch_number, is_proof, index, blob_id, sent_at, created_at, updated_at
            )
            VALUES
            ($1, FALSE, $2, $3, $4, NOW(), NOW())
            ON CONFLICT DO NOTHING
            "#,
            i64::from(number.0),
            index,
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
        index: i32,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
            via_data_availability (
                l1_batch_number, is_proof, index, blob_id, sent_at, created_at, updated_at
            )
            VALUES
            ($1, TRUE, $2, $3, $4, NOW(), NOW())
            ON CONFLICT DO NOTHING
            "#,
            i64::from(number.0),
            index,
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
        index: i32,
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
                AND index = $3
                AND inclusion_data IS NULL
            "#,
            da_inclusion_data,
            i64::from(number.0),
            index,
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
                    AND index = $2
                    AND is_proof = FALSE
                "#,
                i64::from(number.0),
                index,
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
        index: i32,
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
                AND index = $3
                AND inclusion_data IS NULL
            "#,
            proof_inclusion_data,
            i64::from(number.0),
            index,
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
                    AND index = $2
                "#,
                i64::from(number.0),
                index,
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
            ViaStorageDABlob,
            r#"
            SELECT
                l1_batch_number,
                blob_id,
                inclusion_data,
                sent_at,
                index
            FROM
                via_data_availability
            WHERE
                inclusion_data IS NULL
                AND is_proof = FALSE
            ORDER BY
                l1_batch_number ASC,
                index ASC
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
            ViaStorageDABlob,
            r#"
            SELECT
                l1_batch_number,
                blob_id,
                inclusion_data,
                sent_at,
                index
            FROM
                via_data_availability
            WHERE
                inclusion_data IS NULL
                AND is_proof = TRUE
            ORDER BY
                l1_batch_number ASC,
                index ASC
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
            SELECT DISTINCT ON (l1_batches.number)
                number,
                pubdata_input
            FROM
                l1_batches
            LEFT JOIN
                via_data_availability
                ON via_data_availability.l1_batch_number = l1_batches.number
            LEFT JOIN via_l1_batch_inscription_request
                ON
                    via_l1_batch_inscription_request.l1_batch_number = l1_batches.number
                    AND via_data_availability.is_proof = FALSE
            WHERE
                via_l1_batch_inscription_request.commit_l1_batch_inscription_id IS NULL
                AND number != 0
                AND via_data_availability.blob_id IS NULL
                AND pubdata_input IS NOT NULL
            ORDER BY
                number ASC,
                via_data_availability.index DESC
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

    pub async fn is_batch_inclusion_done(
        &mut self,
        l1_batch_number: i64,
        is_proof: bool,
    ) -> DalResult<bool> {
        let result = sqlx::query!(
            r#"
            SELECT
                1 AS cnt
            FROM
                via_data_availability
            WHERE
                l1_batch_number = $1
                AND is_proof = $2
                AND inclusion_data IS NULL
            LIMIT
            1
        "#,
            l1_batch_number,
            is_proof
        )
        .instrument("is_batch_inclusion_done")
        .fetch_optional(self.storage)
        .await?;

        Ok(result.is_none())
    }

    pub async fn get_ready_for_da_dispatch_proofs(
        &mut self,
        limit: usize,
    ) -> DalResult<Vec<ProofDA>> {
        let rows = sqlx::query!(
            r#"
            SELECT DISTINCT ON (l1_batch_number)
                l1_batch_number,
                proof_blob_url
            FROM (
                SELECT
                    pgd.l1_batch_number,
                    pgd.proof_blob_url,
                    COALESCE(
                        (SELECT MAX(vda.index)
                        FROM via_data_availability vda
                        WHERE vda.l1_batch_number = pgd.l1_batch_number
                        AND vda.is_proof = FALSE
                        AND vda.blob_id IS NOT NULL),
                        0
                    ) AS max_index
                FROM
                    proof_generation_details pgd
                WHERE
                    pgd.status = 'generated'
                    AND pgd.proof_blob_url IS NOT NULL
                    AND EXISTS (
                        SELECT 1
                        FROM via_data_availability
                        WHERE l1_batch_number = pgd.l1_batch_number
                        AND is_proof = FALSE
                        AND blob_id IS NOT NULL
                    )
                    AND NOT EXISTS (
                        SELECT 1
                        FROM via_data_availability
                        WHERE l1_batch_number = pgd.l1_batch_number
                        AND is_proof = TRUE
                        AND blob_id IS NOT NULL
                    )
            ) subquery
            ORDER BY
                l1_batch_number ASC,
                max_index DESC
            LIMIT $1
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

    pub async fn get_ready_for_dummy_proof_dispatch_l1_batches(
        &mut self,
        limit: usize,
    ) -> DalResult<Vec<L1BatchNumber>> {
        let rows = sqlx::query!(
            r#"
            SELECT DISTINCT ON (vda.l1_batch_number)
                vda.l1_batch_number
            FROM
                via_data_availability vda
            JOIN l1_batches lb ON vda.l1_batch_number = lb.number
            WHERE
                vda.is_proof = FALSE
                AND vda.blob_id IS NOT NULL
                AND lb.commitment IS NOT NULL
                AND NOT EXISTS (
                    SELECT
                        1
                    FROM
                        via_data_availability vda2
                    WHERE
                        vda2.is_proof = TRUE
                        AND vda2.blob_id IS NOT NULL
                        AND vda2.l1_batch_number = vda.l1_batch_number
                )
            ORDER BY
                vda.l1_batch_number,
                vda.index DESC
            LIMIT
                $1
            "#,
            limit as i64,
        )
        .instrument("get_ready_for_dummy_proof_dispatch_l1_batches")
        .with_arg("limit", &limit)
        .fetch_all(self.storage)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                // check if commitment exist or not

                L1BatchNumber(row.l1_batch_number as u32) // Ensure this conversion is defined
            })
            .collect())
    }

    /// Returns the data availability blob of the block.
    pub async fn get_da_blob(
        &mut self,
        l1_batch_number: L1BatchNumber,
    ) -> DalResult<Vec<DataAvailabilityBlob>> {
        let rows = sqlx::query_as!(
            ViaStorageDABlob,
            r#"
            SELECT
                l1_batch_number,
                blob_id,
                inclusion_data,
                sent_at,
                index
            FROM
                via_data_availability
            WHERE
                inclusion_data IS NOT NULL
                AND is_proof = FALSE
                AND l1_batch_number = $1
            "#,
            i64::from(l1_batch_number.0)
        )
        .instrument("get_da_blob")
        .fetch_all(self.storage)
        .await?;
        Ok(rows.into_iter().map(DataAvailabilityBlob::from).collect())
    }

    /// Returns the data availability blob by blob_id.
    pub async fn get_blob_type(&mut self, blob_id: &str) -> DalResult<Option<bool>> {
        let result = sqlx::query!(
            r#"
            SELECT
                is_proof
            FROM
                via_data_availability
            WHERE
                blob_id = $1
                AND inclusion_data IS NOT NULL
            LIMIT
                1
            "#,
            blob_id
        )
        .instrument("get_blob_type")
        .fetch_optional(self.storage)
        .await?;

        Ok(result.map(|row| (row.is_proof)))
    }

    /// Returns the L1 batch number blob by blob_id.
    pub async fn get_da_batch_number(&mut self, blob_id: &str) -> DalResult<Option<i64>> {
        let result = sqlx::query!(
            r#"
            SELECT
                l1_batch_number
            FROM
                l1_batches
            LEFT JOIN
                via_data_availability
                ON via_data_availability.l1_batch_number = l1_batches.number
            WHERE
                blob_id = $1
                AND inclusion_data IS NOT NULL
            LIMIT
                1
            "#,
            blob_id
        )
        .instrument("get_da_batch_number")
        .fetch_optional(self.storage)
        .await?;

        Ok(result.map(|row| (row.l1_batch_number)))
    }

    /// Returns the DA blob pubdata by blob_id.
    pub async fn get_da_blob_pub_data_by_blob_id(
        &mut self,
        blob_id: &str,
    ) -> DalResult<Option<(i64, Vec<u8>)>> {
        let result = sqlx::query!(
            r#"
            SELECT
                l1_batch_number,
                pubdata_input
            FROM
                l1_batches
            LEFT JOIN
                via_data_availability
                ON via_data_availability.l1_batch_number = l1_batches.number
            WHERE
                blob_id = $1
                AND inclusion_data IS NOT NULL
            LIMIT
                1
            "#,
            blob_id
        )
        .instrument("get_da_blob_by_blob_id")
        .fetch_optional(self.storage)
        .await?;

        Ok(result.map(|row| (row.l1_batch_number, row.pubdata_input.unwrap())))
    }

    pub async fn get_proof_data_by_blob_id(
        &mut self,
        blob_id: &str,
    ) -> DalResult<Option<(ProveBatches, Vec<ProtocolSemanticVersion>)>> {
        let Some(l1_block_number) = self.get_da_batch_number(blob_id).await? else {
            return Ok(None);
        };

        Ok(self
            .get_proof_data(L1BatchNumber(l1_block_number as u32))
            .await)
    }

    /// Get the real proof data for a given L1 batch number. The proof is not returned and should be query from the blob storage.
    pub async fn get_proof_data(
        &mut self,
        batch_to_prove: L1BatchNumber,
    ) -> Option<(ProveBatches, Vec<ProtocolSemanticVersion>)> {
        let previous_batch_number = batch_to_prove - 1;

        let minor_version = match self
            .storage
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

        let latest_semantic_version = match self
            .storage
            .protocol_versions_dal()
            .latest_semantic_version()
            .await
        {
            Ok(Some(version)) => version,
            Ok(None) | Err(_) => {
                tracing::error!("Failed to retrieve the latest semantic version");
                return None;
            }
        };

        let l1_verifier_config = self
            .storage
            .protocol_versions_dal()
            .l1_verifier_config_for_version(latest_semantic_version)
            .await?;

        let allowed_patch_versions = match self
            .storage
            .protocol_versions_dal()
            .get_patch_versions_for_vk(minor_version, l1_verifier_config.snark_wrapper_vk_hash)
            .await
        {
            Ok(versions) if !versions.is_empty() => versions,
            Ok(_) | Err(_) => {
                tracing::warn!(
                    "No patch version corresponds to the verification key on L1: {:?}",
                    l1_verifier_config.snark_wrapper_vk_hash
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

        let previous_proven_batch_metadata = match self
            .storage
            .blocks_dal()
            .get_l1_batch_metadata(previous_batch_number)
            .await
        {
            Ok(Some(metadata)) => metadata,
            Ok(None) => {
                tracing::error!(
                    "L1 batch #{} with submitted proof is not complete in the DB",
                    previous_batch_number
                );
                return None;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to retrieve L1 batch #{} metadata: {}",
                    previous_batch_number,
                    e
                );
                return None;
            }
        };

        let metadata_for_batch_being_proved = match self
            .storage
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

        let res = ProveBatches {
            prev_l1_batch: previous_proven_batch_metadata,
            l1_batches: vec![metadata_for_batch_being_proved],
            proofs: vec![],
            should_verify: true,
        };

        Some((res, allowed_versions))
    }
}
