use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::H256;

use crate::Verifier;

#[derive(Debug)]
pub struct ViaTransactionsDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Verifier>,
}

impl ViaTransactionsDal<'_, '_> {
    pub async fn insert_transaction(
        &mut self,
        priority_id: i64,
        tx_id: H256,
        receiver: String,
        value: i64,
        calldata: Vec<u8>,
        canonical_tx_hash: H256,
        l1_block_number: i64,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_transactions (
                    priority_id,
                    tx_id,
                    receiver,
                    value,
                    calldata,
                    canonical_tx_hash,
                    l1_block_number
                )
            VALUES
                ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (tx_id) DO NOTHING
            "#,
            priority_id,
            tx_id.as_bytes(),
            receiver,
            value,
            calldata,
            canonical_tx_hash.as_bytes(),
            l1_block_number,
        )
        .instrument("insert_transaction")
        .fetch_optional(self.storage)
        .await?;

        Ok(())
    }

    pub async fn get_last_priority_id(&mut self) -> DalResult<i64> {
        let priority_id = sqlx::query_scalar!(
            r#"
            SELECT COUNT(priority_id) as priority_id FROM via_transactions;
            "#
        )
        .instrument("get_last_priority_id")
        .fetch_one(self.storage)
        .await?;

        Ok(priority_id.unwrap_or(0))
    }

    pub async fn list_transactions_not_processed(&mut self, limit: i64) -> DalResult<Vec<Vec<u8>>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                canonical_tx_hash
            FROM
                via_transactions
            WHERE
                status IS NULL
            ORDER BY
                priority_id ASC
            LIMIT
                $1
            "#,
            limit
        )
        .instrument("list_transactions")
        .fetch_all(self.storage)
        .await?;

        let canonical_tx_hashs: Vec<Vec<u8>> =
            rows.into_iter().map(|row| row.canonical_tx_hash).collect();
        Ok(canonical_tx_hashs)
    }

    pub async fn update_transaction(
        &mut self,
        canonical_tx_hash: &H256,
        status: bool,
        l1_batch_number: i64,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            UPDATE via_transactions
            SET
                status = $2,
                l1_batch_number = $3
            WHERE
                canonical_tx_hash = $1
            "#,
            canonical_tx_hash.as_bytes(),
            status,
            l1_batch_number,
        )
        .instrument("update_transaction")
        .fetch_optional(self.storage)
        .await?;

        Ok(())
    }

    pub async fn transaction_exists_with_txid(&mut self, tx_id: &H256) -> DalResult<bool> {
        let exists = sqlx::query!(
            r#"
            SELECT
                1 AS cnt
            FROM
                via_transactions
            WHERE
                tx_id = $1
            LIMIT
                1
            "#,
            tx_id.as_bytes(),
        )
        .instrument("transaction_exists_with_txid")
        .fetch_optional(self.storage)
        .await?;

        Ok(exists.is_some())
    }
}
