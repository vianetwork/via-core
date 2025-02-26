use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::H256;

use crate::Core;

pub struct ViaVotesDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Core>,
}

impl ViaVotesDal<'_, '_> {
    /// Inserts a new vote row in `via_votes`.
    /// Now requires `l1_batch_number` as part of the primary key / FK.
    pub async fn insert_vote(
        &mut self,
        l1_batch_number: u32,
        proof_reveal_tx_id: H256,
        verifier_address: &str,
        vote: bool,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_votes (l1_batch_number, proof_reveal_tx_id, verifier_address, vote)
            VALUES
                ($1, $2, $3, $4)
            ON CONFLICT (l1_batch_number, proof_reveal_tx_id, verifier_address) DO NOTHING
            "#,
            l1_batch_number as i32,
            proof_reveal_tx_id.as_bytes(),
            verifier_address,
            vote
        )
        .instrument("insert_vote")
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(())
    }

    /// Returns (not_ok_votes, ok_votes, total_votes) for the given `(l1_batch_number)`.
    /// Must also filter on `l1_batch_number`.
    pub async fn get_vote_count(&mut self, l1_batch_number: u32) -> DalResult<(i64, i64, i64)> {
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
                l1_batch_number = $1
            "#,
            l1_batch_number as i32,
        )
        .instrument("get_vote_count")
        .report_latency()
        .fetch_one(self.storage)
        .await?;

        let not_ok_votes = row.not_ok_votes.unwrap_or(0);
        let ok_votes = row.ok_votes.unwrap_or(0);
        let total_votes = row.total_votes.unwrap_or(0);
        Ok((not_ok_votes, ok_votes, total_votes))
    }

    /// Marks the transaction as finalized if #ok_votes / #total_votes >= threshold.
    /// Must use `(l1_batch_number, tx_id)` in both vote counting and the UPDATE statement.
    pub async fn finalize_transaction_if_needed(
        &mut self,
        l1_batch_number: u32,
        threshold: f64,
        number_of_verifiers: usize,
    ) -> DalResult<bool> {
        let row = sqlx::query!(
            r#"
            SELECT
                EXISTS (
                    SELECT
                        1
                    FROM
                        via_l1_batch_inscription_request
                    WHERE
                        l1_batch_number = $1
                        AND is_finalized IS NOT NULL
                ) AS already_finalized
            "#,
            i64::from(l1_batch_number),
        )
        .instrument("check_if_already_finalized")
        .report_latency()
        .fetch_one(self.storage)
        .await?;

        if row.already_finalized.unwrap() {
            return Ok(false);
        }

        let (not_ok_votes, ok_votes, _total_votes) = self.get_vote_count(l1_batch_number).await?;

        let mut current_threshold = (ok_votes as f64) / (number_of_verifiers as f64);
        let mut is_finalized = true;
        if not_ok_votes > ok_votes {
            current_threshold = (not_ok_votes as f64) / (number_of_verifiers as f64);
            is_finalized = false;
        }
        let reached_threshold = current_threshold >= threshold;

        if !reached_threshold {
            return Ok(false);
        }

        sqlx::query!(
            r#"
            UPDATE via_l1_batch_inscription_request
            SET
                is_finalized = $2,
                updated_at = NOW()
            WHERE
                l1_batch_number = $1
            "#,
            i64::from(l1_batch_number),
            is_finalized
        )
        .instrument("finalize_transaction_if_needed")
        .report_latency()
        .execute(self.storage)
        .await?;

        Ok(reached_threshold)
    }
}
