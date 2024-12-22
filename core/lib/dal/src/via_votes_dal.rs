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
            i64::from(l1_batch_number),
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
}
