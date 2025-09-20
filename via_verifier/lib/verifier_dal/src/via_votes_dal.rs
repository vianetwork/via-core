use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::H256;

use crate::{models::storage_vote::CanonicalChainStatus, Verifier};

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
        proof_reveal_tx_id: &[u8],
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
            proof_reveal_tx_id,
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

    pub async fn get_last_voted_l1_batch(&mut self) -> DalResult<u32> {
        let row = sqlx::query!(
            r#"
            SELECT
                MAX(l1_batch_number) AS max_batch_number
            FROM
                via_votable_transactions
            WHERE
                l1_batch_status IS NOT NULL
            "#
        )
        .instrument("get_last_voted_l1_batch")
        .fetch_one(self.storage)
        .await?;
        Ok(row.max_batch_number.unwrap_or(0) as u32)
    }

    pub async fn get_last_votable_l1_batch(&mut self) -> DalResult<u32> {
        let row = sqlx::query!(
            r#"
            SELECT
                MAX(l1_batch_number) AS max_batch_number
            FROM
                via_votable_transactions
            "#
        )
        .instrument("get_last_votable_l1_batch")
        .fetch_one(self.storage)
        .await?;
        Ok(row.max_batch_number.unwrap_or(0) as u32)
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

        // Invalidate all subsequent batches if an invalid L1 batch is detected.
        if !l1_batch_status {
            sqlx::query!(
                r#"
                UPDATE via_votable_transactions
                SET
                    l1_batch_status = FALSE,
                    is_finalized = FALSE,
                    updated_at = NOW()
                WHERE
                    l1_batch_number > $1
                "#,
                l1_batch_number
            )
            .instrument("verify_votable_transaction")
            .execute(self.storage)
            .await?;
        }

        Ok(record.id)
    }

    pub async fn get_first_non_finalized_l1_batch_in_canonical_inscription_chain(
        &mut self,
    ) -> DalResult<Option<i64>> {
        let id = sqlx::query_scalar!(
            r#"
            WITH last_finalized AS (
                SELECT
                    l1_batch_number,
                    l1_batch_hash
                FROM via_votable_transactions
                WHERE is_finalized = TRUE
                ORDER BY l1_batch_number DESC
                LIMIT 1
            )
            SELECT v.id
            FROM via_votable_transactions v
            LEFT JOIN last_finalized lf ON v.prev_l1_batch_hash = lf.l1_batch_hash
            WHERE v.is_finalized IS NULL
            AND (
                (v.l1_batch_number = 1 AND NOT EXISTS(SELECT 1 FROM last_finalized))
                OR 
                (lf.l1_batch_hash IS NOT NULL AND v.l1_batch_number = lf.l1_batch_number + 1)
            )
            ORDER BY v.l1_batch_number ASC
            LIMIT 1;
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

    pub async fn get_first_not_verified_l1_batch_in_canonical_inscription_chain(
        &mut self,
    ) -> DalResult<Option<(i64, Vec<u8>)>> {
        let row = sqlx::query!(
            r#"
            WITH
            last_verified AS (
                SELECT
                    l1_batch_number,
                    l1_batch_hash
                FROM
                    via_votable_transactions
                WHERE
                    l1_batch_status = TRUE
                ORDER BY
                    l1_batch_number DESC
                LIMIT
                    1
            )
            
            SELECT
                v.l1_batch_number,
                v.proof_reveal_tx_id
            FROM
                via_votable_transactions v
            LEFT JOIN last_verified lv ON v.prev_l1_batch_hash = lv.l1_batch_hash
            WHERE
                v.l1_batch_status IS NULL
                AND (
                    v.l1_batch_number = 1
                    OR (
                        lv.l1_batch_hash IS NOT NULL
                        AND v.l1_batch_number = lv.l1_batch_number + 1
                    )
                )
            ORDER BY
                v.l1_batch_number ASC
            LIMIT
                1
            "#,
        )
        .instrument("get_first_not_verified_l1_batch_in_canonical_inscription_chain")
        .fetch_optional(&mut self.storage)
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
        let result = sqlx::query!(
            r#"
            SELECT
                v.pubdata_blob_id,
                v.proof_reveal_tx_id
            FROM
                via_votable_transactions v
            LEFT JOIN via_bridge_tx b ON b.votable_tx_id = v.id
            WHERE
                v.is_finalized = TRUE
                AND v.l1_batch_status = TRUE
                AND v.l1_batch_number = $1
                AND (
                    b.hash IS NULL
                    OR b.id IS NULL
                )
            ORDER BY
                v.l1_batch_number ASC
            LIMIT
                1
            "#,
            l1_batch_number
        )
        .instrument("get_finalized_block_and_non_processed_withdrawal")
        .fetch_optional(self.storage)
        .await?;

        let mapped_result = result.map(|row| (row.pubdata_blob_id, row.proof_reveal_tx_id));
        Ok(mapped_result)
    }

    pub async fn list_finalized_blocks_and_non_processed_withdrawals(
        &mut self,
    ) -> DalResult<Vec<(i64, String, Vec<u8>)>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                v.l1_batch_number,
                v.pubdata_blob_id,
                v.proof_reveal_tx_id
            FROM
                via_votable_transactions v
            LEFT JOIN via_bridge_tx b ON b.votable_tx_id = v.id
            WHERE
                v.is_finalized = TRUE
                AND v.l1_batch_status = TRUE
                AND (
                    b.hash IS NULL
                    OR b.id IS NULL
                )
            ORDER BY
                v.l1_batch_number ASC
            "#
        )
        .instrument("list_finalized_blocks_and_non_processed_withdrawals")
        .fetch_all(self.storage)
        .await?;

        let result: Vec<(i64, String, Vec<u8>)> = rows
            .into_iter()
            .map(|r| (r.l1_batch_number, r.pubdata_blob_id, r.proof_reveal_tx_id))
            .collect();

        Ok(result)
    }

    pub async fn get_vote_transaction_bridge_tx(
        &mut self,
        l1_batch_number: i64,
        index: usize,
    ) -> DalResult<Vec<u8>> {
        let row = sqlx::query!(
            r#"
            SELECT
                b.hash
            FROM
                via_bridge_tx b
            JOIN via_votable_transactions v ON b.votable_tx_id = v.id
            WHERE
                v.l1_batch_number = $1
                AND b.index = $2
                AND b.hash IS NOT NULL
            ORDER BY
                b.id ASC
            LIMIT
                1
            "#,
            l1_batch_number,
            index as i64
        )
        .instrument("get_vote_transaction_bridge_tx")
        .fetch_optional(self.storage)
        .await?;

        match row {
            Some(r) => Ok(r.hash.expect("hash is not null by query")),
            None => Ok(Vec::new()),
        }
    }

    /// Delete all the votable_transactions that are invalid and behind the last finalized valid l1_batch
    pub async fn delete_invalid_votable_transactions_if_exists(&mut self) -> DalResult<()> {
        sqlx::query!(
            r#"
            DELETE FROM via_votable_transactions
            WHERE
                l1_batch_status = FALSE
                AND is_finalized = FALSE
            "#
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
                AND NOT EXISTS (
                    SELECT
                        1
                    FROM
                        via_bridge_tx b
                    WHERE
                        b.votable_tx_id = v1.id
                        AND b.hash IS NOT NULL
                )
                AND EXISTS (
                    SELECT
                        1
                    FROM
                        via_votable_transactions v2
                    JOIN via_bridge_tx b2 ON b2.votable_tx_id = v2.id
                    WHERE
                        v1.prev_l1_batch_hash = v2.l1_batch_hash
                        AND v2.is_finalized = TRUE
                        AND v2.l1_batch_status = TRUE
                        AND b2.hash IS NOT NULL
                )
            ORDER BY
                v1.l1_batch_number ASC
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

    pub async fn get_vote_transaction_info(
        &mut self,
        proof_reveal_tx_id: H256,
        index: i64,
    ) -> DalResult<Option<(i64, i64, Option<Vec<u8>>)>> {
        let res = sqlx::query!(
            r#"
            SELECT
                v.id,
                v.l1_batch_number,
                b.hash
            FROM
                via_votable_transactions v
            LEFT JOIN via_bridge_tx b
                ON
                    b.votable_tx_id = v.id
                    AND b.index = $2
            WHERE
                v.proof_reveal_tx_id = $1
            "#,
            proof_reveal_tx_id.as_bytes(),
            index
        )
        .instrument("get_vote_transaction_info")
        .fetch_optional(self.storage)
        .await?;

        Ok(res.map(|row| (row.id, row.l1_batch_number, row.hash)))
    }

    pub async fn get_last_batch_in_canonical_chain(&mut self) -> DalResult<Option<(u32, Vec<u8>)>> {
        let row = sqlx::query!(
            r#"
            WITH RECURSIVE
            canonical_chain AS (
                (
                    SELECT
                        *
                    FROM
                        via_votable_transactions
                    WHERE
                        is_finalized = TRUE
                        OR is_finalized IS NULL
                    ORDER BY
                        l1_batch_number DESC
                    LIMIT
                        1
                )
                UNION ALL
                SELECT
                    vt.*
                FROM
                    via_votable_transactions vt
                JOIN canonical_chain cc ON
                    vt.prev_l1_batch_hash = cc.l1_batch_hash
                    AND vt.l1_batch_number = cc.l1_batch_number + 1
                    AND (
                        vt.l1_batch_status IS NULL
                        OR vt.l1_batch_status = TRUE
                    )
            )
            
            SELECT
                l1_batch_number,
                l1_batch_hash
            FROM
                canonical_chain
            ORDER BY
                l1_batch_number DESC
            LIMIT
                1
            "#
        )
        .instrument("get_last_batch_in_canonical_chain")
        .fetch_optional(&mut self.storage)
        .await?;

        Ok(row.and_then(|r| Some((r.l1_batch_number? as u32, r.l1_batch_hash?))))
    }

    /// Verify that the canonical chain is valid and continuous
    pub async fn verify_canonical_chain(&mut self) -> DalResult<CanonicalChainStatus> {
        let result = sqlx::query!(
            r#"
            WITH RECURSIVE
            canonical_chain AS (
                (
                    SELECT
                        *,
                        1::BIGINT AS chain_position,
                        TRUE AS is_valid_link
                    FROM
                        via_votable_transactions
                    WHERE
                        l1_batch_number = 1
                    ORDER BY
                        created_at ASC -- Take the first one if multiple exist
                    LIMIT
                        1
                )
                UNION ALL
                SELECT
                    vt.*,
                    cc.chain_position + 1 AS chain_position,
                    (
                        vt.prev_l1_batch_hash = cc.l1_batch_hash
                        AND vt.l1_batch_number = cc.l1_batch_number + 1
                    ) AS is_valid_link
                FROM
                    via_votable_transactions vt
                JOIN canonical_chain cc
                    ON
                        vt.prev_l1_batch_hash = cc.l1_batch_hash
                        AND vt.l1_batch_number = cc.l1_batch_number + 1
                        AND (
                            vt.l1_batch_status IS NULL
                            OR vt.l1_batch_status = TRUE
                        )
                WHERE
                    cc.is_valid_link = TRUE -- Only continue if previous link was valid
            ),
            
            chain_stats AS (
                SELECT
                    COUNT(*) AS total_batches,
                    MAX(l1_batch_number) AS max_batch_number,
                    MIN(l1_batch_number) AS min_batch_number,
                    BOOL_AND(is_valid_link) AS all_links_valid,
                    ARRAY_AGG(
                        l1_batch_number
                        ORDER BY
                            l1_batch_number
                    ) AS batch_numbers
                FROM
                    canonical_chain
            ),
            
            expected_sequence AS (
                SELECT
                    GENERATE_SERIES(
                        (
                            SELECT
                                min_batch_number
                            FROM
                                chain_stats
                        ),
                        (
                            SELECT
                                max_batch_number
                            FROM
                                chain_stats
                        )
                    ) AS expected_batch
            ),
            
            missing_batches AS (
                SELECT
                    ARRAY_AGG(expected_batch) AS missing
                FROM
                    expected_sequence es
                WHERE
                    NOT EXISTS (
                        SELECT
                            1
                        FROM
                            canonical_chain cc
                        WHERE
                            cc.l1_batch_number = es.expected_batch
                    )
            )
            
            SELECT
                cs.total_batches,
                cs.max_batch_number,
                cs.min_batch_number,
                cs.all_links_valid,
                cs.batch_numbers,
                mb.missing,
                (
                    cs.total_batches = cs.max_batch_number - cs.min_batch_number + 1
                ) AS is_continuous,
                (
                    SELECT
                        COUNT(*)
                    FROM
                        via_votable_transactions
                ) AS total_transactions_in_db
            FROM
                chain_stats cs,
                missing_batches mb
            "#
        )
        .instrument("verify_canonical_chain")
        .fetch_optional(&mut self.storage)
        .await?;

        match result {
            Some(row) => {
                let status = CanonicalChainStatus {
                    is_valid: row.all_links_valid.unwrap_or(false)
                        && row.is_continuous.unwrap_or(false)
                        && row.missing.is_none(),
                    total_canonical_batches: row.total_batches.unwrap_or(0),
                    max_batch_number: row.max_batch_number.map(|n| n as u32),
                    min_batch_number: row.min_batch_number.map(|n| n as u32),
                    missing_batches: row
                        .missing
                        .unwrap_or_default()
                        .into_iter()
                        .map(|n| n as u32)
                        .collect(),
                    batch_sequence: row
                        .batch_numbers
                        .unwrap_or_default()
                        .into_iter()
                        .map(|n| n as u32)
                        .collect(),
                    total_transactions_in_db: row.total_transactions_in_db.unwrap_or(0),
                    has_genesis: row.min_batch_number == Some(1),
                };
                Ok(status)
            }
            None => {
                // No canonical chain found (no batch 1)
                Ok(CanonicalChainStatus {
                    is_valid: false,
                    total_canonical_batches: 0,
                    max_batch_number: None,
                    min_batch_number: None,
                    missing_batches: vec![],
                    batch_sequence: vec![],
                    total_transactions_in_db: 0,
                    has_genesis: false,
                })
            }
        }
    }

    /// Check if a batch with the given number already exists
    pub async fn batch_exists(&mut self, l1_batch_number: u32) -> DalResult<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1 
                FROM via_votable_transactions 
                WHERE l1_batch_number = $1
            )
            "#,
            l1_batch_number as i64
        )
        .instrument("batch_exists")
        .fetch_one(&mut self.storage)
        .await?;

        Ok(exists.unwrap_or(false))
    }

    pub async fn proof_reveal_tx_exists(&mut self, proof_reveal_tx_id: &[u8]) -> DalResult<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1 
                FROM via_votable_transactions 
                WHERE proof_reveal_tx_id = $1
            )
            "#,
            proof_reveal_tx_id
        )
        .instrument("proof_reveal_tx_exists")
        .fetch_one(&mut self.storage)
        .await?;

        Ok(exists.unwrap_or(false))
    }
}
