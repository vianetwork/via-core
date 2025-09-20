use anyhow::Context;
use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::{
    via_btc_sender::{ViaBtcInscriptionRequest, ViaBtcInscriptionRequestHistory},
    L1BatchNumber,
};

use crate::{
    models::storage_btc_inscription_request::{
        ViaStorageBtcInscriptionRequest, ViaStorageBtcInscriptionRequestHistory,
    },
    Core,
};

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
