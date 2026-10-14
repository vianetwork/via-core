use anyhow::Context;
use sqlx::Row;
use zksync_db_connection::{
    connection::{Connection, DbMarker},
    error::DalResult,
    instrument::InstrumentExt,
};
use zksync_types::{
    via_btc_sender::{
        ViaBtcInscriptionRequest, ViaBtcInscriptionRequestHistory, ViaBtcInscriptionStatus,
    },
    L1BatchNumber,
};

use crate::{
    models::storage_btc_inscription_request::{
        ViaStorageBtcInscriptionRequest, ViaStorageBtcInscriptionRequestHistory,
    },
    Core,
};

/// Selects the sender's request-to-batch association.
#[derive(Debug, Clone, Copy)]
pub enum InscriptionSender {
    Main,
    Verifier,
}

/// Reads pending requests and their latest attempts in one database snapshot.
/// Unobserved requests require investigation before declaring the sender healthy.
pub async fn inscription_status<DB: DbMarker>(
    connection: &mut Connection<'_, DB>,
    sender: InscriptionSender,
    current_height: u64,
    threshold: u32,
) -> anyhow::Result<ViaBtcInscriptionStatus> {
    let (batch_column, batch_join) = match sender {
        InscriptionSender::Main => ("r.l1_batch_number", ""),
        InscriptionSender::Verifier => (
            "v.l1_batch_number",
            "LEFT JOIN via_l1_batch_vote_inscription_request m ON m.vote_l1_batch_inscription_id = r.id
             LEFT JOIN via_votable_transactions v ON v.id = m.votable_transaction_id",
        ),
    };
    let query = format!(
        r#"
        WITH pending AS (
            SELECT r.id, {batch_column} AS l1_batch_number
            FROM via_btc_inscriptions_request r
            {batch_join}
            WHERE r.confirmed_inscriptions_request_history_id IS NULL
        ), latest AS (
            SELECT DISTINCT ON (h.inscription_request_id)
                h.inscription_request_id, h.sent_at_block
            FROM via_btc_inscriptions_request_history h
            JOIN pending p ON p.id = h.inscription_request_id
            ORDER BY h.inscription_request_id, h.id DESC
        ), status AS (
            SELECT p.l1_batch_number, h.sent_at_block,
                h.sent_at_block BETWEEN 0 AND $1
                    AND h.sent_at_block <= $1 - $2 AS overdue
            FROM pending p
            LEFT JOIN latest h ON h.inscription_request_id = p.id
        )
        SELECT COUNT(*) AS pending,
            COUNT(*) FILTER (WHERE overdue) AS overdue,
            COUNT(*) FILTER (
                WHERE sent_at_block IS NULL OR sent_at_block NOT BETWEEN 0 AND $1
                    OR l1_batch_number IS NULL OR l1_batch_number < 0
            ) AS unobserved,
            COALESCE(MIN(l1_batch_number) FILTER (
                WHERE overdue AND l1_batch_number >= 0
            ), 0) AS first_overdue_batch
        FROM status
        "#
    );
    sqlx::query::<sqlx::Postgres>(&query)
        .bind(i64::try_from(current_height).context("Bitcoin height exceeds database range")?)
        .bind(i64::from(threshold))
        .try_map(|row| {
            Ok(ViaBtcInscriptionStatus {
                pending: row.try_get("pending")?,
                overdue: row.try_get("overdue")?,
                unobserved: row.try_get("unobserved")?,
                first_overdue_batch: row.try_get("first_overdue_batch")?,
            })
        })
        .instrument("inscription_status")
        .report_latency()
        .fetch_one(connection)
        .await
        .context("fetch current Bitcoin inscription status")
}

#[derive(Debug)]
pub struct ViaBtcSenderDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Core>,
}

impl ViaBtcSenderDal<'_, '_> {
    /// Insert a btc inscription request.
    pub async fn via_save_btc_inscriptions_request(
        &mut self,
        l1_batch_number: L1BatchNumber,
        inscription_request_type: String,
        inscription_message: Vec<u8>,
        predicted_fee: u64,
    ) -> DalResult<i64> {
        let record = sqlx::query!(
            r#"
            INSERT INTO
            via_btc_inscriptions_request (
                l1_batch_number,
                request_type,
                inscription_message,
                predicted_fee,
                created_at,
                updated_at
            )
            VALUES
            ($1, $2, $3, $4, NOW(), NOW())
            RETURNING
            id
            "#,
            i64::from(l1_batch_number.0),
            inscription_request_type,
            inscription_message,
            predicted_fee as i64,
        )
        .instrument("via_save_btc_inscriptions_request")
        .report_latency()
        .fetch_one(self.storage)
        .await?;

        Ok(record.id)
    }

    /// List the inflight inscription request ids.
    pub async fn list_inflight_inscription_ids(&mut self) -> DalResult<Vec<i64>> {
        let records = sqlx::query!(
            r#"
            SELECT
                via_btc_inscriptions_request.id
            FROM
                via_btc_inscriptions_request
            JOIN via_btc_inscriptions_request_history
                ON
                    via_btc_inscriptions_request.id
                    = via_btc_inscriptions_request_history.inscription_request_id
                    AND via_btc_inscriptions_request_history.sent_at_block IS NOT NULL
                    AND via_btc_inscriptions_request.confirmed_inscriptions_request_history_id IS NULL
                    AND via_btc_inscriptions_request_history.id = (
                        SELECT
                            id
                        FROM
                            via_btc_inscriptions_request_history
                        WHERE
                            inscription_request_id = via_btc_inscriptions_request.id
                            AND via_btc_inscriptions_request_history.sent_at_block IS NOT NULL
                        ORDER BY
                            created_at DESC
                        LIMIT
                            1
                    )
            ORDER BY
                id
            "#
        )
        .instrument("list_inflight_inscription_ids")
        .report_latency()
        .fetch_all(self.storage)
        .await?;

        Ok(records.iter().map(|r| r.id).collect())
    }

    /// List new inscription requests not processed.
    pub async fn list_new_inscription_request(
        &mut self,
        limit: i64,
    ) -> DalResult<Vec<ViaBtcInscriptionRequest>> {
        let records = sqlx::query_as!(
            ViaStorageBtcInscriptionRequest,
            r#"
            SELECT
                via_btc_inscriptions_request.*
            FROM
                via_btc_inscriptions_request
            LEFT JOIN
                via_btc_inscriptions_request_history
                ON
                    via_btc_inscriptions_request.id
                    = via_btc_inscriptions_request_history.inscription_request_id
            WHERE
                via_btc_inscriptions_request_history.inscription_request_id IS NULL
            ORDER BY
                via_btc_inscriptions_request.id
            LIMIT
                $1
            "#,
            limit,
        )
        .instrument("list_new_inscription_request")
        .report_latency()
        .fetch_all(self.storage)
        .await?;

        Ok(records.into_iter().map(|r| r.into()).collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_inscription_request_history(
        &mut self,
        commit_tx_id: &[u8],
        reveal_tx_id: &[u8],
        inscription_request_id: i64,
        signed_commit_tx: &[u8],
        signed_reveal_tx: &[u8],
        actual_fees: i64,
        sent_at_block: i64,
    ) -> DalResult<i64> {
        let record = sqlx::query!(
            r#"
            INSERT INTO
            via_btc_inscriptions_request_history (
                commit_tx_id,
                reveal_tx_id,
                inscription_request_id,
                signed_commit_tx,
                signed_reveal_tx,
                actual_fees,
                sent_at_block,
                created_at,
                updated_at
            )
            VALUES
            ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())
            RETURNING
            id
            "#,
            commit_tx_id,
            reveal_tx_id,
            inscription_request_id,
            signed_commit_tx,
            signed_reveal_tx,
            actual_fees,
            sent_at_block as i32
        )
        .instrument("insert_inscription_request_history")
        .report_latency()
        .fetch_one(self.storage)
        .await?;

        Ok(record.id)
    }

    pub async fn get_last_inscription_request_history(
        &mut self,
        inscription_request_id: i64,
    ) -> DalResult<Option<ViaBtcInscriptionRequestHistory>> {
        let inscription_request_history = sqlx::query_as!(
            ViaStorageBtcInscriptionRequestHistory,
            r#"
            SELECT
                *
            FROM
                via_btc_inscriptions_request_history
            WHERE
                inscription_request_id = $1
            ORDER BY
                id DESC
            LIMIT
                1
            "#,
            inscription_request_id
        )
        .instrument("get_last_inscription_request_history")
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(inscription_request_history.map(ViaBtcInscriptionRequestHistory::from))
    }

    pub async fn get_inscription_request(
        &mut self,
        id: i64,
    ) -> DalResult<Option<ViaBtcInscriptionRequest>> {
        let inscription_request = sqlx::query_as!(
            ViaStorageBtcInscriptionRequest,
            r#"
            SELECT
                *
            FROM
                via_btc_inscriptions_request
            WHERE
                id = $1
            "#,
            id
        )
        .instrument("get_inscription_request")
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(inscription_request.map(ViaBtcInscriptionRequest::from))
    }

    pub async fn confirm_inscription(
        &mut self,
        inscriptions_request_id: i64,
        inscriptions_request_history_id: i64,
    ) -> anyhow::Result<ViaBtcInscriptionRequest> {
        let mut transaction = self
            .storage
            .start_transaction()
            .await
            .context("start_transaction_confirm_inscription")?;

        sqlx::query!(
            r#"
            UPDATE via_btc_inscriptions_request_history
            SET
                updated_at = NOW(),
                confirmed_at = NOW()
            WHERE
                id = $1
            "#,
            inscriptions_request_history_id
        )
        .execute(transaction.conn())
        .await?;

        let inscription = sqlx::query_as!(
            ViaStorageBtcInscriptionRequest,
            r#"
            UPDATE via_btc_inscriptions_request
            SET
                updated_at = NOW(),
                confirmed_inscriptions_request_history_id = $2
            WHERE
                id = $1
            RETURNING
            *
            "#,
            inscriptions_request_id,
            inscriptions_request_history_id
        )
        .fetch_one(transaction.conn())
        .await?;

        transaction
            .commit()
            .await
            .with_context(|| "Error commit transaction confirm inscription")?;

        Ok(inscription.into())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::Executor;

    use super::*;
    use crate::ConnectionPool;

    #[tokio::test]
    async fn inscription_status_uses_current_requests_and_latest_attempts() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let mut connection = pool.connection().await.unwrap();

        for sender in [InscriptionSender::Main, InscriptionSender::Verifier] {
            let mut storage = connection.start_transaction().await.unwrap();
            storage
                .conn()
                .execute(
                    "CREATE SCHEMA inscription_observability_fixture;
                     SET LOCAL search_path TO inscription_observability_fixture;",
                )
                .await
                .unwrap();
            let migrations = match sender {
                InscriptionSender::Main => [
                    "CREATE TABLE l1_batches (number BIGINT PRIMARY KEY);",
                    include_str!("../migrations/20240906134623_add_via_btc_inscription_requests.up.sql"),
                ],
                InscriptionSender::Verifier => [
                    include_str!("../../../../via_verifier/lib/verifier_dal/migrations/20240906134623_add_via_btc_inscription_requests.up.sql"),
                    include_str!("../../../../via_verifier/lib/verifier_dal/migrations/20250112053854_create_via_votes.up.up.sql"),
                ],
            };
            for migration in migrations {
                storage.conn().execute(migration).await.unwrap();
            }

            let status = inscription_status(&mut storage, sender, 113, 12)
                .await
                .unwrap();
            assert_eq!(status, ViaBtcInscriptionStatus::default());

            let requests = "(VALUES (1, 1), (2, 20), (3, 20), (4, 40), (5, 50),
                            (6, 60), (7, 70), (8, -1), (9, 90)) AS r(id, batch)";
            let (request_sql, txid) = match sender {
                InscriptionSender::Main => (
                    format!(
                        "INSERT INTO l1_batches SELECT DISTINCT batch FROM {requests};
                         INSERT INTO via_btc_inscriptions_request (id, l1_batch_number, request_type, updated_at)
                         SELECT id, batch, 'test', NOW() FROM {requests};"
                    ),
                    "decode(lpad(id::text, 64, '0'), 'hex')",
                ),
                InscriptionSender::Verifier => (
                    format!(
                        "INSERT INTO via_btc_inscriptions_request (id, request_type, updated_at)
                         SELECT id, 'test', NOW() FROM {requests};
                         INSERT INTO via_votable_transactions (
                            id, l1_batch_number, l1_batch_hash, prev_l1_batch_hash, proof_blob_id,
                            proof_reveal_tx_id, pubdata_blob_id, pubdata_reveal_tx_id, da_identifier
                         ) SELECT id + 1000, batch, decode(lpad(id::text, 64, '0'), 'hex'), ''::bytea,
                            'proof-' || id, decode(lpad(id::text, 64, '0'), 'hex'), 'pub-' || id,
                            'pubtx-' || id, 'test' FROM {requests};
                         INSERT INTO via_l1_batch_vote_inscription_request
                         SELECT id + 1000, id, NOW(), NOW() FROM via_btc_inscriptions_request;
                         DELETE FROM via_votable_transactions WHERE id = 1009;"
                    ),
                    "lpad(id::text, 64, '0')",
                ),
            };
            storage.conn().execute(request_sql.as_str()).await.unwrap();
            storage.conn().execute(format!(r#"
                INSERT INTO via_btc_inscriptions_request_history (
                    id, inscription_request_id, sent_at_block, created_at, commit_tx_id,
                    reveal_tx_id, signed_commit_tx, signed_reveal_tx, actual_fees, updated_at
                ) SELECT id, request_id, height, created_at::timestamp, {txid}, {txid},
                    ''::bytea, ''::bytea, 0, NOW() FROM (VALUES
                    (1, 1, 0, '2000-01-01'), (2, 2, 0, '2001-01-01'), (20, 2, 100, '2000-01-01'),
                    (3, 3, 0, '2000-01-01'), (30, 3, 100, '2000-01-01'),
                    (5, 5, -1, '2000-01-01'), (6, 6, 200, '2000-01-01'),
                    (7, 7, 0, '2000-01-01'), (8, 8, 100, '2000-01-01'),
                    (9, 9, 100, '2000-01-01')
                ) AS h(id, request_id, height, created_at);
                UPDATE via_btc_inscriptions_request SET confirmed_inscriptions_request_history_id = 1 WHERE id = 1;
            "#).as_str()).await.unwrap();

            for (height, overdue, first_batch) in [(111, 1, 70), (112, 5, 20), (113, 5, 20)] {
                let status = inscription_status(&mut storage, sender, height, 12)
                    .await
                    .unwrap();
                let unobserved = if matches!(sender, InscriptionSender::Verifier) {
                    5
                } else {
                    4
                };
                assert_eq!(
                    status,
                    ViaBtcInscriptionStatus {
                        pending: 8,
                        overdue,
                        unobserved,
                        first_overdue_batch: first_batch,
                    },
                    "{sender:?}, height {height}"
                );
            }
            let status = inscription_status(&mut storage, sender, 0, 0)
                .await
                .unwrap();
            assert_eq!((status.overdue, status.first_overdue_batch), (1, 70));

            storage.conn().execute("UPDATE via_btc_inscriptions_request r SET confirmed_inscriptions_request_history_id = (SELECT MAX(id) FROM via_btc_inscriptions_request_history WHERE inscription_request_id = r.id) WHERE r.id IN (2, 3, 7, 8, 9)").await.unwrap();
            let status = inscription_status(&mut storage, sender, 113, 12)
                .await
                .unwrap();
            assert_eq!(
                status,
                ViaBtcInscriptionStatus {
                    pending: 3,
                    unobserved: 3,
                    ..Default::default()
                }
            );

            storage.conn().execute(format!(
                "INSERT INTO via_btc_inscriptions_request_history (
                    id, inscription_request_id, sent_at_block, commit_tx_id, reveal_tx_id,
                    signed_commit_tx, signed_reveal_tx, actual_fees, updated_at
                 ) SELECT id, id, 100, {txid}, {txid}, ''::bytea, ''::bytea, 0, NOW() FROM (VALUES (4)) AS h(id);
                 UPDATE via_btc_inscriptions_request r SET confirmed_inscriptions_request_history_id =
                    (SELECT MAX(id) FROM via_btc_inscriptions_request_history WHERE inscription_request_id = r.id);"
            ).as_str()).await.unwrap();
            let status = inscription_status(&mut storage, sender, 113, 12)
                .await
                .unwrap();
            assert_eq!(status, ViaBtcInscriptionStatus::default());
            assert!(inscription_status(&mut storage, sender, u64::MAX, 12)
                .await
                .is_err());
            storage.conn().execute("ALTER TABLE via_btc_inscriptions_request_history RENAME COLUMN sent_at_block TO unavailable").await.unwrap();
            assert!(inscription_status(&mut storage, sender, 113, 12)
                .await
                .is_err());
        }
    }
}
