use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};

use crate::{
    models::{
        deposit::Deposit,
        withdraw::{BridgeWithdrawalParam, WithdrawalParam},
    },
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
                    deposits (priority_id, tx_id, block_number, receiver, value, calldata, canonical_tx_hash)
                VALUES
                    ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (tx_id) DO NOTHING
                "#,
                deposit.priority_id,
                deposit.tx_id,
                i64::from(deposit.block_number),
                deposit.receiver,
                deposit.value,
                deposit.calldata,
                deposit.canonical_tx_hash,
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

    pub async fn withdrawal_exists(&mut self, tx_id: &[u8]) -> DalResult<bool> {
        let exists = sqlx::query_scalar!(
            r#"
                SELECT EXISTS(
                    SELECT 1 FROM bridge_withdrawals WHERE tx_id = $1
                )
                "#,
            tx_id
        )
        .instrument("withdrawal_exists")
        .fetch_one(self.storage)
        .await?
        .unwrap_or(false);

        Ok(exists)
    }

    pub async fn insert_withdraw(
        &mut self,
        bridget_withdrawal_param: BridgeWithdrawalParam,
        withdrawals: Vec<WithdrawalParam>,
    ) -> DalResult<()> {
        if withdrawals.is_empty() {
            return Ok(());
        }

        let mut transaction = self.storage.start_transaction().await?;

        let id = sqlx::query!(
            r#"
                INSERT INTO bridge_withdrawals (tx_id, l1_batch_reveal_tx_id, block_number, fee, vsize, total_size, withdrawals_count)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (tx_id) DO NOTHING
                RETURNING id
                "#,
            bridget_withdrawal_param.tx_id,
            bridget_withdrawal_param.l1_batch_reveal_tx_id,
            bridget_withdrawal_param.block_number,
            bridget_withdrawal_param.vsize,
            bridget_withdrawal_param.total_size,
            bridget_withdrawal_param.fee,
            withdrawals.len() as i64
        )
        .instrument("insert_bridge_withdrawal")
        .fetch_one(&mut transaction)
        .await?
        .id;

        for withdrawal in withdrawals {
            sqlx::query!(
                r#"
                INSERT INTO
                    withdrawals (tx_index, bridge_withdrawal_id, receiver, value)
                VALUES
                    ($1, $2, $3, $4)
                "#,
                withdrawal.tx_index,
                i32::from(id),
                withdrawal.receiver,
                withdrawal.value,
            )
            .instrument("insert_withdrawal")
            .execute(&mut transaction)
            .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    async fn delete_all_withdrawals(&mut self, block_number: i64) -> DalResult<()> {
        sqlx::query!(
            r#"
            DELETE FROM bridge_withdrawals WHERE block_number >= $1
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
