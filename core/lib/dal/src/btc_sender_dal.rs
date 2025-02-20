use anyhow::Context;
use bitcoin::hash_types::Txid;
use zksync_db_connection::connection::Connection;
use zksync_types::btc_sender::{ViaBtcInscriptionRequest, ViaBtcInscriptionRequestHistory};

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
    pub async fn via_save_btc_inscriptions_request(
        &mut self,
        inscription_request_type: String,
        inscription_message: Vec<u8>,
        predicted_fee: u64,
    ) -> sqlx::Result<ViaBtcInscriptionRequest> {
        let inscription_request = sqlx::query_as!(
            ViaBtcInscriptionRequest,
            r#"
            INSERT INTO
                via_btc_inscriptions_request (request_type, inscription_message, predicted_fee, created_at, updated_at)
            VALUES
                ($1, $2, $3, NOW(), NOW())
            RETURNING
                *
            "#,
            inscription_request_type,
            inscription_message,
            predicted_fee as i64,
        )
        .fetch_one(self.storage.conn())
        .await?;
        Ok(inscription_request)
    }

    pub async fn get_inflight_inscriptions(
        &mut self,
    ) -> sqlx::Result<Vec<ViaBtcInscriptionRequest>> {
        let txs = sqlx::query_as!(
            ViaStorageBtcInscriptionRequest,
            r#"
            SELECT
                via_btc_inscriptions_request.*
            FROM
                via_btc_inscriptions_request
                JOIN via_btc_inscriptions_request_history ON via_btc_inscriptions_request.id = via_btc_inscriptions_request_history.inscription_request_id
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
        .fetch_all(self.storage.conn())
        .await?;
        Ok(txs.into_iter().map(|tx| tx.into()).collect())
    }

    pub async fn list_new_inscription_request(
        &mut self,
        limit: i64,
    ) -> sqlx::Result<Vec<ViaBtcInscriptionRequest>> {
        let txs = sqlx::query_as!(
            ViaStorageBtcInscriptionRequest,
            r#"
            SELECT
                via_btc_inscriptions_request.*
            FROM
                via_btc_inscriptions_request
                LEFT JOIN via_btc_inscriptions_request_history ON via_btc_inscriptions_request.id = via_btc_inscriptions_request_history.inscription_request_id
            WHERE
                via_btc_inscriptions_request_history.inscription_request_id IS NULL
            ORDER BY
                via_btc_inscriptions_request.id
            LIMIT
                $1
            "#,
            limit,
        )
        .fetch_all(self.storage.conn())
        .await?;
        Ok(txs.into_iter().map(|tx| tx.into()).collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_inscription_request_history(
        &mut self,
        commit_tx_id: Txid,
        reveal_tx_id: Txid,
        inscription_request_id: i64,
        signed_commit_tx: Vec<u8>,
        signed_reveal_tx: Vec<u8>,
        actual_fees: i64,
        sent_at_block: i64,
    ) -> sqlx::Result<Option<u32>> {
        Ok(sqlx::query!(
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
            commit_tx_id.to_string(),
            reveal_tx_id.to_string(),
            inscription_request_id,
            signed_commit_tx,
            signed_reveal_tx,
            actual_fees,
            sent_at_block as i32
        )
        .fetch_optional(self.storage.conn())
        .await?
        .map(|row| row.id as u32))
    }

    pub async fn get_last_inscription_request_history(
        &mut self,
        inscription_request_id: i64,
    ) -> sqlx::Result<Option<ViaBtcInscriptionRequestHistory>> {
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
        .fetch_optional(self.storage.conn())
        .await?;

        Ok(inscription_request_history.map(ViaBtcInscriptionRequestHistory::from))
    }

    pub async fn confirm_inscription(
        &mut self,
        inscriptions_request_id: i64,
        inscriptions_request_history_id: i64,
    ) -> anyhow::Result<()> {
        let mut transaction = self
            .storage
            .start_transaction()
            .await
            .context("start_transaction")?;

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

        sqlx::query!(
            r#"
            UPDATE via_btc_inscriptions_request
            SET
                updated_at = NOW(),
                confirmed_inscriptions_request_history_id = $2
            WHERE
                id = $1
            "#,
            inscriptions_request_id,
            inscriptions_request_history_id
        )
        .execute(transaction.conn())
        .await?;

        transaction.commit().await.context("commit()")
    }
}
