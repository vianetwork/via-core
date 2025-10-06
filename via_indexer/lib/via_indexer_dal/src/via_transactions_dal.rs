use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};

use crate::{
    models::{deposit::Deposit, withdraw::Withdrawal},
    Indexer,
};

#[derive(Debug)]
pub struct ViaTransactionsDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Indexer>,
}

impl ViaTransactionsDal<'_, '_> {
    pub async fn insert_deposit_many(&mut self, deposits: Vec<Deposit>) -> DalResult<()> {
        if deposits.is_empty() {
            return Ok(());
        }
        let mut transaction = self.storage.start_transaction().await?;

        for deposit in deposits {
            sqlx::query!(
                r#"
                INSERT INTO
                deposits (
                    priority_id,
                    tx_id,
                    block_number,
                    sender,
                    receiver,
                    value,
                    calldata,
                    canonical_tx_hash,
                    created_at
                )
                VALUES
                ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT (tx_id) DO NOTHING
                "#,
                deposit.priority_id,
                deposit.tx_id,
                i64::from(deposit.block_number),
                deposit.sender,
                deposit.receiver,
                deposit.value,
                deposit.calldata,
                deposit.canonical_tx_hash,
                deposit.block_timestamp as i64,
            )
            .instrument("insert_deposit")
            .execute(&mut transaction)
            .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    pub async fn deposit_exists(&mut self, tx_id: &[u8]) -> DalResult<bool> {
        let exists = sqlx::query_scalar!(
            r#"
                SELECT EXISTS(
                    SELECT 1 FROM deposits WHERE tx_id = $1
                )
                "#,
            tx_id,
        )
        .instrument("deposit_exists")
        .fetch_one(self.storage)
        .await?
        .unwrap_or(false);

        Ok(exists)
    }

    async fn delete_all_deposits(&mut self, block_number: i64) -> DalResult<()> {
        sqlx::query!(
            r#"
            DELETE FROM deposits WHERE block_number >= $1
            "#,
            block_number
        )
        .instrument("delete_deposits")
        .execute(self.storage)
        .await?;

        Ok(())
    }

    pub async fn get_last_priority_id(&mut self) -> DalResult<i64> {
        let priority_id = sqlx::query_scalar!(
            r#"
            SELECT COUNT(priority_id) as priority_id FROM deposits;
            "#
        )
        .instrument("get_last_priority_id")
        .fetch_one(self.storage)
        .await?;

        Ok(priority_id.unwrap_or(0))
    }

    pub async fn insert_withdraw(&mut self, withdrawal: Withdrawal) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO withdrawals (
                id,
                tx_id,
                l2_tx_log_index,
                receiver,
                value,
                timestamp,
                block_number
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (id) DO NOTHING
            "#,
            withdrawal.id,
            withdrawal.tx_id,
            withdrawal.l2_tx_log_index,
            withdrawal.receiver,
            withdrawal.value,
            withdrawal.timestamp,
            withdrawal.block_number
        )
        .instrument("insert_withdrawal")
        .execute(&mut self.storage)
        .await?;

        Ok(())
    }

    async fn delete_all_withdrawals(&mut self, block_number: i64) -> DalResult<()> {
        sqlx::query!(
            r#"
            DELETE FROM withdrawals WHERE block_number >= $1
            "#,
            block_number
        )
        .instrument("delete_withdrawals")
        .execute(self.storage)
        .await?;

        Ok(())
    }

    pub async fn delete_transactions(&mut self, block_number: i64) -> DalResult<()> {
        self.delete_all_deposits(block_number).await?;
        self.delete_all_withdrawals(block_number).await?;
        Ok(())
    }
}
