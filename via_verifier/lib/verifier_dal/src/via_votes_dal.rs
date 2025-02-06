use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::H256;

use crate::Verifier;

pub struct ViaVotesDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Verifier>,
}

impl ViaVotesDal<'_, '_> {
    /// Inserts a new row in `via_votable_transactions`.
    /// Notice we haven’t changed this since the PK is still (l1_batch_number, tx_id).
    pub async fn insert_votable_transaction(
        &mut self,
        l1_batch_number: u32,
        tx_id: H256,
        da_identifier: String,
        blob_id: String,
        pubdata_reveal_tx_id: String,
        pubdata_blob_id: String,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_votable_transactions (
                    l1_batch_number,
                    tx_id,
                    da_identifier,
                    blob_id,
                    pubdata_reveal_tx_id,
                    pubdata_blob_id
                )
            VALUES
                ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (l1_batch_number, tx_id) DO NOTHING
            "#,
            i64::from(l1_batch_number),
            tx_id.as_bytes(),
            da_identifier,
            blob_id,
            pubdata_reveal_tx_id,
            pubdata_blob_id
        )
        .instrument("insert_votable_transaction")
        .fetch_optional(self.storage)
        .await?;

        Ok(())
    }

    /// Inserts a new vote row in `via_votes`.
    /// Now requires `l1_batch_number` as part of the primary key / FK.
    pub async fn insert_vote(
        &mut self,
        l1_batch_number: u32,
        tx_id: H256,
        verifier_address: &str,
        vote: bool,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_votes (l1_batch_number, tx_id, verifier_address, vote)
            VALUES
                ($1, $2, $3, $4)
            ON CONFLICT (l1_batch_number, tx_id, verifier_address) DO NOTHING
            "#,
            l1_batch_number as i32,
            tx_id.as_bytes(),
            verifier_address,
            vote
        )
        .instrument("insert_vote")
        .fetch_optional(self.storage)
        .await?;

        Ok(())
    }

    /// Returns (ok_votes, total_votes) for the given `(l1_batch_number, tx_id)`.
    /// Must also filter on `l1_batch_number`.
    pub async fn get_vote_count(
        &mut self,
        l1_batch_number: u32,
        tx_id: H256,
    ) -> DalResult<(i64, i64)> {
        let row = sqlx::query!(
            r#"
            SELECT
                COUNT(*) FILTER (
                    WHERE
                        vote = TRUE
                ) AS ok_votes,
                COUNT(*) AS total_votes
            FROM
                via_votes
            WHERE
                l1_batch_number = $1
                AND tx_id = $2
            "#,
            l1_batch_number as i32,
            tx_id.as_bytes()
        )
        .instrument("get_vote_count")
        .fetch_one(self.storage)
        .await?;

        let ok_votes = row.ok_votes.unwrap_or(0);
        let total_votes = row.total_votes.unwrap_or(0);
        Ok((ok_votes, total_votes))
    }

    /// Marks the transaction as finalized if #ok_votes / #total_votes >= threshold.
    /// Must use `(l1_batch_number, tx_id)` in both vote counting and the UPDATE statement.
    pub async fn finalize_transaction_if_needed(
        &mut self,
        l1_batch_number: u32,
        tx_id: H256,
        threshold: f64,
        number_of_verifiers: usize,
    ) -> DalResult<bool> {
        let row = sqlx::query!(
            r#"
            SELECT
                is_finalized
            FROM
                via_votable_transactions
            WHERE
                l1_batch_number = $1
                AND tx_id = $2
            "#,
            i64::from(l1_batch_number),
            tx_id.as_bytes()
        )
        .instrument("check_if_already_finalized")
        .fetch_one(self.storage)
        .await?;

        if row.is_finalized {
            return Ok(false);
        }

        let (ok_votes, _total_votes) = self.get_vote_count(l1_batch_number, tx_id).await?;
        let is_threshold_reached = (ok_votes as f64) / (number_of_verifiers as f64) >= threshold;

        if is_threshold_reached {
            sqlx::query!(
                r#"
                UPDATE via_votable_transactions
                SET
                    is_finalized = TRUE,
                    updated_at = NOW()
                WHERE
                    l1_batch_number = $1
                    AND tx_id = $2
                "#,
                i64::from(l1_batch_number),
                tx_id.as_bytes()
            )
            .instrument("finalize_transaction_if_needed")
            .execute(self.storage)
            .await?;
        }

        Ok(is_threshold_reached)
    }

    pub async fn get_last_inserted_block(&mut self) -> DalResult<Option<u32>> {
        let row = sqlx::query!(
            r#"
            SELECT
                MAX(l1_batch_number) AS max_batch_number
            FROM
                via_votable_transactions
            "#
        )
        .instrument("get_last_inserted_block")
        .fetch_one(self.storage)
        .await?;

        Ok(row.max_batch_number.map(|n| n as u32))
    }

    pub async fn verify_votable_transaction(
        &mut self,
        l1_batch_number: u32,
        tx_id: H256,
        l1_batch_status: bool,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            UPDATE via_votable_transactions
            SET
                is_verified = TRUE,
                l1_batch_status = $3,
                updated_at = NOW()
            WHERE
                l1_batch_number = $1
                AND tx_id = $2
            "#,
            i64::from(l1_batch_number),
            tx_id.as_bytes(),
            l1_batch_status
        )
        .instrument("verify_transaction")
        .execute(self.storage)
        .await?;
        Ok(())
    }

    pub async fn get_first_non_finalized_block(&mut self) -> DalResult<Option<i64>> {
        let l1_block_number = sqlx::query_scalar!(
            r#"
            SELECT
                MIN(l1_batch_number) as "l1_batch_number"
            FROM via_votable_transactions
            WHERE
                is_finalized = FALSE 
            "#,
        )
        .instrument("get_last_block_finilized")
        .fetch_optional(self.storage)
        .await?
        .flatten();

        Ok(l1_block_number)
    }

    pub async fn get_verifier_vote_status(
        &mut self,
        block_number: i64,
    ) -> DalResult<Option<(bool, Vec<u8>)>> {
        let row = sqlx::query!(
            r#"
            SELECT
                l1_batch_status,
                tx_id
            FROM
                via_votable_transactions
            WHERE
                l1_batch_number = $1
                AND is_verified = TRUE
            LIMIT
                1
            "#,
            block_number
        )
        .instrument("get_verifier_vote_status")
        .fetch_optional(self.storage)
        .await?;

        let result = row.map(|r| {
            let l1_batch_status = r.l1_batch_status;
            let tx_id = r.tx_id;
            (l1_batch_status, tx_id)
        });

        Ok(result)
    }

    /// Retrieve the first not executed block. (Similar to `get_first_not_finilized_block`, just with `is_verified = FALSE`).
    pub async fn get_first_not_verified_block(&mut self) -> DalResult<Option<(i64, Vec<u8>)>> {
        let row = sqlx::query!(
            r#"
            SELECT
                l1_batch_number,
                tx_id
            FROM
                via_votable_transactions
            WHERE
                is_verified = FALSE
            ORDER BY
                l1_batch_number ASC
            LIMIT
                1
            "#,
        )
        .instrument("get_first_not_executed_block")
        .fetch_optional(self.storage)
        .await?;

        let result = row.map(|r| {
            let l1_batch_number = r.l1_batch_number;
            let tx_id = r.tx_id;
            (l1_batch_number, tx_id)
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
                tx_id
            FROM
                via_votable_transactions
            WHERE
                is_finalized = TRUE
                AND is_verified = TRUE
                AND withdrawal_tx_id IS NULL
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
        let mapped_result = result.map(|row| (row.pubdata_blob_id, row.tx_id));

        Ok(mapped_result)
    }

    pub async fn get_finalized_blocks_and_non_processed_withdrawals(
        &mut self,
    ) -> DalResult<Vec<(i64, String, Vec<u8>)>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                l1_batch_number,
                pubdata_blob_id,
                tx_id
            FROM
                via_votable_transactions
            WHERE
                is_finalized = TRUE
                AND is_verified = TRUE
                AND withdrawal_tx_id IS NULL
            ORDER BY
                l1_batch_number ASC
            "#,
        )
        .instrument("get_finalized_blocks_and_non_processed_withdrawals")
        .fetch_all(self.storage)
        .await?;

        // Map the rows into a Vec<(l1_batch_number, pubdata_blob_id, tx_id)>
        let result: Vec<(i64, String, Vec<u8>)> = rows
            .into_iter()
            .map(|r| (r.l1_batch_number, r.pubdata_blob_id, r.tx_id))
            .collect();

        Ok(result)
    }

    pub async fn mark_vote_transaction_as_processed_withdrawals(
        &mut self,
        tx_id: H256,
        l1_batch_number: i64,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            UPDATE via_votable_transactions
            SET
                withdrawal_tx_id = $1
            WHERE
                is_finalized = TRUE
                AND is_verified = TRUE
                AND withdrawal_tx_id IS NULL
                AND l1_batch_number = $2
            "#,
            tx_id.as_bytes(),
            l1_batch_number
        )
        .instrument("mark_vote_transaction_as_processed_withdrawals")
        .execute(self.storage)
        .await?;

        Ok(())
    }

    pub async fn get_vote_transaction_withdrawal_tx(
        &mut self,
        l1_batch_number: i64,
    ) -> DalResult<Option<Vec<u8>>> {
        let withdrawal_tx_id = sqlx::query_scalar!(
            r#"
            SELECT
                withdrawal_tx_id
            FROM via_votable_transactions
            WHERE
                l1_batch_number = $1
            "#,
            l1_batch_number
        )
        .instrument("get_vote_transaction_withdrawal_tx")
        .fetch_optional(self.storage)
        .await?
        .flatten();

        Ok(withdrawal_tx_id)
    }
}
