use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::H256;

use crate::Core;

pub struct ViaVotesDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Core>,
}

impl ViaVotesDal<'_, '_> {
    pub async fn insert_votable_transaction(
        &mut self,
        tx_id: H256,
        tx_type: &str,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_votable_transactions (tx_id, transaction_type)
            VALUES
                ($1, $2)
            ON CONFLICT (tx_id) DO NOTHING
            "#,
            tx_id.as_bytes(),
            tx_type
        )
        .instrument("insert_votable_transaction")
        .fetch_optional(self.storage)
        .await?;
        Ok(())
    }

    pub async fn insert_vote(
        &mut self,
        tx_id: H256,
        verifier_address: &str,
        vote: bool,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_votes (tx_id, verifier_address, vote)
            VALUES
                ($1, $2, $3)
            ON CONFLICT (tx_id, verifier_address) DO NOTHING
            "#,
            tx_id.as_bytes(),
            verifier_address,
            vote
        )
        .instrument("insert_vote")
        .fetch_optional(self.storage)
        .await?;
        Ok(())
    }

    pub async fn get_vote_count(&mut self, tx_id: H256) -> DalResult<(i64, i64)> {
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
                tx_id = $1
            "#,
            tx_id.as_bytes()
        )
        .instrument("get_vote_count")
        .fetch_one(self.storage)
        .await?;

        let ok_votes = row.ok_votes.unwrap_or(0);
        let total_votes = row.total_votes.unwrap_or(0);
        Ok((ok_votes, total_votes))
    }

    pub async fn finalize_transaction_if_needed(
        &mut self,
        tx_id: H256,
        threshold: f64,
    ) -> DalResult<bool> {
        let (ok_votes, total_votes) = self.get_vote_count(tx_id).await?;
        let finalized = total_votes > 0 && (ok_votes as f64) / (total_votes as f64) > threshold;
        if finalized {
            sqlx::query!(
                r#"
                UPDATE via_votable_transactions
                SET
                    is_finalized = TRUE,
                    updated_at = NOW()
                WHERE
                    tx_id = $1
                "#,
                tx_id.as_bytes()
            )
            .instrument("finalize_transaction_if_needed")
            .execute(self.storage)
            .await?;
        }
        Ok(finalized)
    }

    pub async fn is_transaction_finalized(&mut self, tx_id: H256) -> DalResult<bool> {
        let row = sqlx::query!(
            r#"
            SELECT
                is_finalized
            FROM
                via_votable_transactions
            WHERE
                tx_id = $1
            "#,
            tx_id.as_bytes()
        )
        .instrument("is_transaction_finalized")
        .fetch_optional(self.storage)
        .await?;
        Ok(row.map(|r| r.is_finalized).unwrap_or(false))
    }
}
