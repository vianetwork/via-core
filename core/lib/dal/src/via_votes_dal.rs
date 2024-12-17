use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::H256;

use crate::Core;

pub struct ViaVotesDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Core>,
}

impl ViaVotesDal<'_, '_> {
    /// Inserts a new row in `via_votable_transactions`.
    /// Notice we haven’t changed this since the PK is still (l1_batch_number, tx_id).
    pub async fn insert_votable_transaction(
        &mut self,
        l1_batch_number: u32,
        tx_id: H256,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_votable_transactions (l1_batch_number, tx_id)
            VALUES
                ($1, $2)
            ON CONFLICT (l1_batch_number, tx_id) DO NOTHING
            "#,
            l1_batch_number as i64,
            tx_id.as_bytes()
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

    /// Marks the transaction as finalized if #ok_votes / #total_votes > threshold.
    /// Must use `(l1_batch_number, tx_id)` in both vote counting and the UPDATE statement.
    pub async fn finalize_transaction_if_needed(
        &mut self,
        l1_batch_number: u32,
        tx_id: H256,
        threshold: f64,
    ) -> DalResult<bool> {
        let (ok_votes, total_votes) = self.get_vote_count(l1_batch_number, tx_id).await?;
        let is_above_threshold =
            total_votes > 0 && (ok_votes as f64) / (total_votes as f64) > threshold;

        if is_above_threshold {
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
                l1_batch_number as i64,
                tx_id.as_bytes()
            )
            .instrument("finalize_transaction_if_needed")
            .execute(self.storage)
            .await?;
        }

        Ok(is_above_threshold)
    }
}
