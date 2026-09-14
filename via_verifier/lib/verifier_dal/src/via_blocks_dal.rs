use zksync_db_connection::{
    connection::Connection,
    error::DalResult,
    instrument::{InstrumentExt, Instrumented},
};
use zksync_types::via_verifier_btc_inscription_operations::ViaVerifierBtcInscriptionRequestType;

use crate::Verifier;

#[derive(Debug)]
pub struct ViaBlocksDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Verifier>,
}

impl ViaBlocksDal<'_, '_> {
    pub async fn insert_vote_l1_batch_inscription_request_id(
        &mut self,
        votable_transaction_id: i64,
        inscription_request_id: i64,
        inscription_request: ViaVerifierBtcInscriptionRequestType,
    ) -> DalResult<()> {
        match inscription_request {
            ViaVerifierBtcInscriptionRequestType::VoteOnchain => {
                let instrumentation = Instrumented::new("set_inscription_request_tx_id#commit")
                    .with_arg("votable_transaction_id", &votable_transaction_id)
                    .with_arg("inscription_request_id", &inscription_request_id);

                let query = sqlx::query!(
                    r#"
                    INSERT INTO
                    via_l1_batch_vote_inscription_request (
                        votable_transaction_id, vote_l1_batch_inscription_id, created_at, updated_at
                    )
                    VALUES
                    ($1, $2, NOW(), NOW())
                    ON CONFLICT DO NOTHING
                    "#,
                    votable_transaction_id,
                    inscription_request_id as i32,
                );
                let result = instrumentation
                    .clone()
                    .with(query)
                    .execute(self.storage)
                    .await?;

                if result.rows_affected() == 0 {
                    let err = instrumentation.constraint_error(anyhow::anyhow!(
                        "Failed to insert into 'via_l1_batch_vote_inscription_request': \
                        No rows were affected. This could be due to a conflict or invalid input values. \
                        votable_transaction_id: {:?}, inscription_request_id: {:?}",
                        votable_transaction_id,
                        inscription_request_id as i32
                    ));
                    return Err(err);
                }
                Ok(())
            }
        }
    }

    pub async fn check_vote_l1_batch_inscription_request_if_exists(
        &mut self,
        batch_number: i64,
    ) -> DalResult<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM via_l1_batch_vote_inscription_request
                WHERE votable_transaction_id = $1
            )
            "#,
            batch_number
        )
        .instrument("check_vote_l1_batch_inscription_request_id_exists")
        .fetch_one(self.storage)
        .await?;

        Ok(exists.unwrap_or(false))
    }

    /// Returns (batch, latest sent height) once per unconfirmed vote request, including unsent requests.
    /// Greatest history ID defines the latest attempt. Missing vote associations remain visible as NULL.
    pub async fn get_pending_inscription_attempts(
        &mut self,
    ) -> DalResult<Vec<(Option<i64>, Option<i64>)>> {
        sqlx::query_as(
            r#"
            WITH pending AS (
                SELECT r.id, MAX(h.id) AS history_id
                FROM via_btc_inscriptions_request r
                LEFT JOIN via_btc_inscriptions_request_history h ON h.inscription_request_id = r.id
                WHERE r.confirmed_inscriptions_request_history_id IS NULL
                GROUP BY r.id
            )
            SELECT v.l1_batch_number, h.sent_at_block
            FROM pending
            LEFT JOIN via_btc_inscriptions_request_history h ON h.id = pending.history_id
            LEFT JOIN via_l1_batch_vote_inscription_request a ON a.vote_l1_batch_inscription_id = pending.id
            LEFT JOIN via_votable_transactions v ON v.id = a.votable_transaction_id
            "#
        )
        .instrument("get_pending_inscription_attempts")
        .report_latency()
        .fetch_all(self.storage)
        .await
    }
}

#[cfg(test)]
mod inscription_tests {
    use crate::{ConnectionPool, Verifier, VerifierDal};

    #[tokio::test]
    async fn pending_inscription_attempts() {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        sqlx::raw_sql(r#"
            INSERT INTO via_btc_inscriptions_request (id, request_type, updated_at)
                SELECT n, 'VoteOnchain', NOW() FROM generate_series(1,5) n;
            INSERT INTO via_btc_inscriptions_request_history (id, inscription_request_id, sent_at_block,
                commit_tx_id, reveal_tx_id, signed_commit_tx, signed_reveal_tx, actual_fees, updated_at)
                SELECT n, r, h, n::text, n::text, ''::bytea, ''::bytea, 0, NOW()
                FROM (VALUES (1,1,0), (2,2,100), (3,2,111), (4,4,100), (5,5,101)) t(n,r,h);
            INSERT INTO via_votable_transactions (id, l1_batch_number, l1_batch_hash, prev_l1_batch_hash,
                proof_blob_id, proof_reveal_tx_id, pubdata_blob_id, pubdata_reveal_tx_id, da_identifier, is_finalized)
                SELECT n*10, b, decode(lpad(n::text,64,'0'),'hex'), ''::bytea, n::text,
                    decode(lpad(n::text,64,'0'),'hex'), n::text, n::text, 'test', CASE WHEN n=5 THEN FALSE ELSE NULL END
                FROM (VALUES (1,1), (2,7), (3,9), (5,7)) t(n,b);
            INSERT INTO via_l1_batch_vote_inscription_request (votable_transaction_id, vote_l1_batch_inscription_id, updated_at)
                SELECT n*10, n, NOW() FROM unnest(ARRAY[1,2,3,5]) n;
            UPDATE via_btc_inscriptions_request_history SET confirmed_at = NOW() WHERE id = 1;
            UPDATE via_btc_inscriptions_request SET confirmed_inscriptions_request_history_id = 1 WHERE id = 1;
        "#).execute(storage.conn()).await.unwrap();
        let mut attempts = storage
            .via_block_dal()
            .get_pending_inscription_attempts()
            .await
            .unwrap();
        attempts.sort();
        assert_eq!(
            attempts,
            vec![
                (None, Some(100)),
                (Some(7), Some(101)),
                (Some(7), Some(111)),
                (Some(9), None)
            ]
        );
        sqlx::query("UPDATE via_btc_inscriptions_request SET confirmed_inscriptions_request_history_id = 3 WHERE id = 2")
            .execute(storage.conn()).await.unwrap();
        let attempts = storage
            .via_block_dal()
            .get_pending_inscription_attempts()
            .await
            .unwrap();
        assert_eq!(attempts.len(), 3);
        assert!(!attempts.contains(&(Some(7), Some(111))));
    }
}
