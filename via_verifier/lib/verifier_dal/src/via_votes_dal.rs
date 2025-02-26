use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::H256;

use crate::Verifier;

pub struct ViaVotesDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Verifier>,
}

impl ViaVotesDal<'_, '_> {
    /// Inserts a new row in `via_votable_transactions`.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_votable_transaction(
        &mut self,
        l1_batch_number: u32,
        l1_batch_hash: H256,
        prev_l1_batch_hash: H256,
        da_identifier: String,
        proof_reveal_tx_id: H256,
        proof_blob_id: String,
        pubdata_reveal_tx_id: String,
        pubdata_blob_id: String,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_votable_transactions (
                    l1_batch_number,
                    l1_batch_hash,
                    prev_l1_batch_hash,
                    proof_reveal_tx_id,
                    da_identifier,
                    proof_blob_id,
                    pubdata_reveal_tx_id,
                    pubdata_blob_id
                )
            VALUES
                ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (l1_batch_hash) DO NOTHING
            "#,
            i64::from(l1_batch_number),
            l1_batch_hash.as_bytes(),
            prev_l1_batch_hash.as_bytes(),
            proof_reveal_tx_id.as_bytes(),
            da_identifier,
            proof_blob_id,
            pubdata_reveal_tx_id,
            pubdata_blob_id
        )
        .instrument("insert_votable_transaction")
        .fetch_optional(self.storage)
        .await?;

        Ok(())
    }

    pub async fn get_votable_transaction_id(
        &mut self,
        proof_reveal_tx_id: H256,
    ) -> DalResult<Option<i64>> {
        let row = sqlx::query!(
            r#"
            SELECT
                id
            FROM
                via_votable_transactions
            WHERE
                proof_reveal_tx_id = $1
            "#,
            proof_reveal_tx_id.as_bytes(),
        )
        .instrument("get_votable_transaction_id")
        .fetch_optional(self.storage)
        .await?;
        Ok(row.map(|r| r.id))
    }

    /// Inserts a new vote row in `via_votes`.
    pub async fn insert_vote(
        &mut self,
        votable_transaction_id: i64,
        verifier_address: &str,
        vote: bool,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_votes (votable_transaction_id, verifier_address, vote)
            VALUES
                ($1, $2, $3)
            ON CONFLICT (votable_transaction_id, verifier_address) DO NOTHING
            "#,
            votable_transaction_id,
            verifier_address,
            vote
        )
        .instrument("insert_vote")
        .with_arg("votable_transaction_id", &votable_transaction_id)
        .with_arg("verifier_address", &verifier_address)
        .with_arg("vote", &vote)
        .fetch_optional(self.storage)
        .await?;

        Ok(())
    }

    /// Returns (not_ok_votes, ok_votes, total_votes) for the given `votable_transaction_id`.
    pub async fn get_vote_count(&mut self, id: i64) -> DalResult<(i64, i64, i64)> {
        let row = sqlx::query!(
            r#"
            SELECT
                COUNT(*) FILTER (
                    WHERE
                        vote = FALSE
                ) AS not_ok_votes,
                COUNT(*) FILTER (
                    WHERE
                        vote = TRUE
                ) AS ok_votes,
                COUNT(*) AS total_votes
            FROM
                via_votes
            WHERE
                votable_transaction_id = $1
            "#,
            id
        )
        .instrument("get_vote_count")
        .fetch_one(self.storage)
        .await?;

        let not_ok_votes = row.not_ok_votes.unwrap_or(0);
        let ok_votes = row.ok_votes.unwrap_or(0);
        let total_votes = row.total_votes.unwrap_or(0);
        Ok((not_ok_votes, ok_votes, total_votes))
    }

    /// Marks the transaction as finalized if #ok_votes / #total_votes >= threshold.
    pub async fn finalize_transaction_if_needed(
        &mut self,
        id: i64,
        threshold: f64,
        number_of_verifiers: usize,
    ) -> DalResult<bool> {
        let row = sqlx::query!(
            r#"
            SELECT
                is_finalized,
                l1_batch_status
            FROM
                via_votable_transactions
            WHERE
                id = $1
            "#,
            id
        )
        .instrument("check_if_already_finalized")
        .fetch_one(self.storage)
        .await?;

        if row.is_finalized.is_some() {
            return Ok(false);
        }

        // The verifier cannot finalize the block unless the zk verification has been successfully completed.
        if row.l1_batch_status.is_none() {
            return Ok(false);
        }

        let (not_ok_votes, ok_votes, _total_votes) = self.get_vote_count(id).await?;

        let mut current_threshold = (ok_votes as f64) / (number_of_verifiers as f64);
        let mut is_finalized = true;
        if not_ok_votes > ok_votes {
            current_threshold = (not_ok_votes as f64) / (number_of_verifiers as f64);
            is_finalized = false;
        }
        let is_threshold_reached = current_threshold >= threshold;
        if !is_threshold_reached {
            return Ok(false);
        }

        sqlx::query!(
            r#"
            UPDATE via_votable_transactions
            SET
                is_finalized = $2,
                updated_at = NOW()
            WHERE
                id = $1
            "#,
            id,
            is_finalized
        )
        .instrument("finalize_transaction_if_needed")
        .execute(self.storage)
        .await?;

        Ok(is_finalized)
    }

    pub async fn get_last_finalized_l1_batch(&mut self) -> DalResult<Option<u32>> {
        let row = sqlx::query!(
            r#"
            SELECT
                MAX(l1_batch_number) AS max_batch_number
            FROM
                via_votable_transactions
            WHERE
                is_finalized = TRUE
            "#
        )
        .instrument("get_last_inserted_block")
        .fetch_one(self.storage)
        .await?;

        Ok(row.max_batch_number.map(|n| n as u32))
    }

    pub async fn verify_votable_transaction(
        &mut self,
        l1_batch_number: i64,
        proof_reveal_tx_id: H256,
        l1_batch_status: bool,
    ) -> DalResult<i64> {
        let record = sqlx::query!(
            r#"
            UPDATE via_votable_transactions
            SET
                l1_batch_status = $3,
                updated_at = NOW()
            WHERE
                l1_batch_number = $1
                AND proof_reveal_tx_id = $2
            RETURNING
                id
            "#,
            l1_batch_number,
            proof_reveal_tx_id.as_bytes(),
            l1_batch_status
        )
        .instrument("verify_votable_transaction")
        .fetch_one(self.storage)
        .await?;
        Ok(record.id)
    }

    pub async fn get_first_non_finalized_l1_batch_in_canonical_inscription_chain(
        &mut self,
    ) -> DalResult<Option<i64>> {
        let id = sqlx::query_scalar!(
            r#"
            SELECT
                v1.id as id
            FROM
                via_votable_transactions v1
            WHERE
                v1.is_finalized IS NULL
                AND (
                    v1.l1_batch_number = 1
                    OR EXISTS (
                        SELECT
                            1
                        FROM
                            via_votable_transactions v2
                        WHERE
                            v2.l1_batch_hash = v1.prev_l1_batch_hash
                            AND v2.l1_batch_number = v1.l1_batch_number - 1
                            AND v2.is_finalized = TRUE
                    )
                )
            ORDER BY
                v1.l1_batch_number ASC
            LIMIT
                1 
            "#,
        )
        .instrument("get_first_non_finalized_l1_batch_in_canonical_inscription_chain")
        .fetch_optional(self.storage)
        .await?;

        Ok(id)
    }

    pub async fn get_verifier_vote_status(
        &mut self,
        votable_transaction_id: i64,
    ) -> DalResult<Option<(bool, Vec<u8>)>> {
        let row = sqlx::query!(
            r#"
            SELECT
                l1_batch_status,
                proof_reveal_tx_id
            FROM
                via_votable_transactions
            WHERE
                id = $1
                AND l1_batch_status IS NOT NULL
            LIMIT
                1
            "#,
            votable_transaction_id
        )
        .instrument("get_verifier_vote_status")
        .fetch_optional(self.storage)
        .await?;

        let result = row.map(|r| {
            let l1_batch_status = r.l1_batch_status.unwrap_or_default();
            let proof_reveal_tx_id = r.proof_reveal_tx_id;
            (l1_batch_status, proof_reveal_tx_id)
        });

        Ok(result)
    }

    /// Retrieve the first not executed in the canonical inscription list.
    pub async fn get_first_not_verified_l1_batch_in_canonical_inscription_chain(
        &mut self,
    ) -> DalResult<Option<(i64, Vec<u8>)>> {
        let row = sqlx::query!(
            r#"
            SELECT
                v1.l1_batch_number AS l1_batch_number,
                v1.proof_reveal_tx_id AS proof_reveal_tx_id
            FROM
                via_votable_transactions v1
            WHERE
                l1_batch_status IS NULL
                AND v1.is_finalized IS NULL
                AND (
                    v1.l1_batch_number = 1
                    OR EXISTS (
                        SELECT
                            1
                        FROM
                            via_votable_transactions v2
                        WHERE
                            v2.l1_batch_hash = v1.prev_l1_batch_hash
                            AND v2.l1_batch_number = v1.l1_batch_number - 1
                            AND v2.l1_batch_status = TRUE
                    )
                )
            ORDER BY
                v1.l1_batch_number ASC
            LIMIT
                1
            "#,
        )
        .instrument("get_first_not_executed_block")
        .fetch_optional(self.storage)
        .await?;

        let result = row.map(|r| {
            let l1_batch_number = r.l1_batch_number;
            let proof_reveal_tx_id = r.proof_reveal_tx_id;
            (l1_batch_number, proof_reveal_tx_id)
        });

        Ok(result)
    }

    pub async fn get_finalized_block_and_non_processed_withdrawal(
        &mut self,
        l1_batch_number: i64,
    ) -> DalResult<Option<(String, Vec<u8>)>> {
        // Query the database to fetch the desired row
        let result = sqlx::query!(
            r#"
            SELECT
                pubdata_blob_id,
                proof_reveal_tx_id
            FROM
                via_votable_transactions
            WHERE
                is_finalized = TRUE
                AND l1_batch_status = TRUE
                AND bridge_tx_id IS NULL
                AND l1_batch_number = $1
            LIMIT
                1
            "#,
            l1_batch_number
        )
        .instrument("get_finalized_block_and_non_processed_withdrawal")
        .fetch_optional(self.storage) // Use fetch_optional to handle None results
        .await?;

        // Map the result into the desired output format
        let mapped_result = result.map(|row| (row.pubdata_blob_id, row.proof_reveal_tx_id));

        Ok(mapped_result)
    }

    pub async fn list_finalized_blocks_and_non_processed_withdrawals(
        &mut self,
    ) -> DalResult<Vec<(i64, String, Vec<u8>)>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                l1_batch_number,
                pubdata_blob_id,
                proof_reveal_tx_id
            FROM
                via_votable_transactions
            WHERE
                is_finalized = TRUE
                AND l1_batch_status = TRUE
                AND bridge_tx_id IS NULL
            ORDER BY
                l1_batch_number ASC
            "#,
        )
        .instrument("list_finalized_blocks_and_non_processed_withdrawals")
        .fetch_all(self.storage)
        .await?;

        // Map the rows into a Vec<(l1_batch_number, pubdata_blob_id, proof_reveal_tx_id)>
        let result: Vec<(i64, String, Vec<u8>)> = rows
            .into_iter()
            .map(|r| (r.l1_batch_number, r.pubdata_blob_id, r.proof_reveal_tx_id))
            .collect();

        Ok(result)
    }

    pub async fn mark_vote_transaction_as_processed(
        &mut self,
        bridge_tx_id: H256,
        proof_reveal_tx_id: &[u8],
        l1_batch_number: i64,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            UPDATE via_votable_transactions
            SET
                bridge_tx_id = $1
            WHERE
                bridge_tx_id IS NULL
                AND l1_batch_number = $2
                AND proof_reveal_tx_id = $3
            "#,
            bridge_tx_id.as_bytes(),
            l1_batch_number,
            proof_reveal_tx_id,
        )
        .instrument("mark_vote_transaction_as_processed")
        .execute(self.storage)
        .await?;

        Ok(())
    }

    pub async fn get_vote_transaction_bridge_tx_id(
        &mut self,
        l1_batch_number: i64,
    ) -> DalResult<Option<Vec<u8>>> {
        let bridge_tx_id = sqlx::query_scalar!(
            r#"
            SELECT
                bridge_tx_id
            FROM via_votable_transactions
            WHERE
                l1_batch_number = $1
            "#,
            l1_batch_number
        )
        .instrument("get_vote_transaction")
        .fetch_optional(self.storage)
        .await?
        .flatten();

        Ok(bridge_tx_id)
    }

    /// Delete all the votable_transactions that are invalid and behind the last finilized valid l1_batch
    pub async fn delete_invalid_votable_transactions(
        &mut self,
        l1_batch_number: i64,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            DELETE FROM via_votable_transactions
            WHERE
                l1_batch_number < $1
                AND (
                    is_finalized = FALSE
                    OR is_finalized IS NULL
                )
            "#,
            l1_batch_number
        )
        .instrument("delete_invalid_votable_transactions")
        .fetch_optional(self.storage)
        .await?;
        Ok(())
    }

    /// Get the first rejected l1 batch (failed zk proof).
    pub async fn get_first_rejected_l1_batch(&mut self) -> DalResult<Option<(i64, Vec<u8>)>> {
        let row = sqlx::query!(
            r#"
            SELECT
                v1.l1_batch_number,
                v1.l1_batch_hash
            FROM
                via_votable_transactions v1
            WHERE
                v1.is_finalized = FALSE
                AND v1.l1_batch_status = FALSE
                AND v1.bridge_tx_id IS NULL
                AND (
                    EXISTS (
                        SELECT
                            1
                        FROM
                            via_votable_transactions v2
                        WHERE
                            v1.prev_l1_batch_hash = v2.l1_batch_hash
                            AND v2.is_finalized = TRUE
                            AND v2.l1_batch_status = TRUE
                            AND v2.bridge_tx_id IS NOT NULL
                    )
                )
            LIMIT
                1
            "#
        )
        .instrument("get_first_rejected_l1_batch")
        .fetch_optional(self.storage)
        .await?;

        let result = row.map(|r| (r.l1_batch_number, r.l1_batch_hash));

        Ok(result)
    }
}
