use bitcoin::hash_types::Txid;

use zksync_types::{
    btc_inscription_operations::ViaBtcInscriptionRequestType, btc_sender::ViaBtcInscriptionRequest,
};

use zksync_db_connection::connection::Connection;

use crate::{models::storage_btc_inscription_request::ViaStorageBtcInscriptionRequest, Core};

#[derive(Debug)]
pub struct ViaBtcSenderDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Core>,
}

impl ViaBtcSenderDal<'_, '_> {
    #[allow(clippy::too_many_arguments)]
    pub async fn via_save_btc_inscriptions_request(
        &mut self,
        inscription_request_type: ViaBtcInscriptionRequestType,
        inscription_message: Vec<u8>,
        predicted_fee: u32,
    ) -> sqlx::Result<ViaBtcInscriptionRequest> {
        let inscription_request = sqlx::query_as!(
            ViaBtcInscriptionRequest,
            r#"
            INSERT INTO
                via_btc_inscriptions_request (
                    request_type,
                    inscription_message,
                    predicted_fee,
                    created_at,
                    updated_at
                )
            VALUES
                ($1, $2, $3, NOW(), NOW())
            RETURNING
                *
            "#,
            inscription_request_type.to_string(),
            inscription_message,
            i64::from(predicted_fee),
        )
        .fetch_one(self.storage.conn())
        .await?;
        Ok(inscription_request.into())
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
            LEFT JOIN via_btc_inscriptions_request_history
                ON via_btc_inscriptions_request.id = via_btc_inscriptions_request_history.inscription_request_id
            WHERE
                via_btc_inscriptions_request_history.sent_at_block IS NOT NULL
                AND
                via_btc_inscriptions_request_history.confirmed_at IS NULL
            ORDER BY
                via_btc_inscriptions_request.id
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
                LEFT JOIN via_btc_inscriptions_request_history
                ON via_btc_inscriptions_request.id = via_btc_inscriptions_request_history.inscription_request_id
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
        inscription_request_context_id: i64,
        signed_raw_tx: Vec<u8>,
        actual_fees: i64,
        sent_at_block: i64,
    ) -> anyhow::Result<Option<u32>> {
        Ok(sqlx::query!(
            r#"
            INSERT INTO
                via_btc_inscriptions_request_history (
                    commit_tx_id,
                    reveal_tx_id,
                    inscription_request_id,
                    inscription_request_context_id,
                    signed_raw_tx,
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
            inscription_request_context_id,
            signed_raw_tx,
            actual_fees,
            sent_at_block as i32
        )
        .fetch_optional(self.storage.conn())
        .await?
        .map(|row| row.id as u32))
    }
}
