use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};

use crate::{models::withdrawal::Withdrawal, Verifier};

pub struct ViaWithdrawalDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Verifier>,
}

impl ViaWithdrawalDal<'_, '_> {
    /// Inserts a new withdrawals.
    pub async fn insert_withdrawals(
        &mut self,
        bridge_tx_id: Option<i64>,
        withdrawals: Vec<Withdrawal>,
    ) -> DalResult<Vec<bool>> {
        let mut results = Vec::with_capacity(withdrawals.len());

        for withdrawal in withdrawals {
            let res = sqlx::query!(
                r#"
                INSERT INTO
                    via_withdrawals (bridge_tx_id, l2_tx_hash, l2_tx_index, receiver, value)
                VALUES
                    ($1, $2, $3, $4, $5)
                ON CONFLICT (l2_tx_hash, l2_tx_index) DO NOTHING
                "#,
                bridge_tx_id,
                withdrawal.l2_tx_hash,
                withdrawal.l2_tx_index,
                withdrawal.receiver,
                withdrawal.value,
            )
            .instrument("insert_withdrawals")
            .report_latency()
            .execute(&mut self.storage)
            .await?;

            // Check if the row was actually inserted
            results.push(res.rows_affected() > 0);
        }

        Ok(results)
    }

    pub async fn check_if_withdrawals_are_processed(
        &mut self,
        withdrawals: Vec<Withdrawal>,
    ) -> DalResult<Vec<bool>> {
        let mut results = Vec::with_capacity(withdrawals.len());

        for withdrawal in withdrawals {
            // query! returns a struct per row, even for a single column
            let row = sqlx::query!(
                r#"
                SELECT
                    bridge_tx_id
                FROM
                    via_withdrawals
                WHERE
                    l2_tx_hash = $1
                    AND l2_tx_index = $2
                "#,
                withdrawal.l2_tx_hash,
                withdrawal.l2_tx_index
            )
            .instrument("check_if_withdrawals_are_processed")
            .fetch_optional(&mut self.storage)
            .await?;

            // row is Option<struct>, row.bridge_tx_id is Option<i64> if nullable
            results.push(row.map(|r| r.bridge_tx_id.is_some()).unwrap_or(false));
        }

        Ok(results)
    }

    /// Update the withdrawals as processed by setting the bridge_tx_id and updated_at
    pub async fn update_withdrawals_to_processed(
        &mut self,
        bridge_tx_id: i64,
        withdrawals: Vec<Withdrawal>,
    ) -> DalResult<()> {
        for withdrawal in withdrawals {
            sqlx::query!(
                r#"
                UPDATE via_withdrawals
                SET
                    bridge_tx_id = $1,
                    updated_at = NOW()
                WHERE
                    l2_tx_hash = $2
                    AND l2_tx_index = $3
                "#,
                bridge_tx_id,
                withdrawal.l2_tx_hash,
                withdrawal.l2_tx_index
            )
            .instrument("update_withdrawals_to_processed")
            .report_latency()
            .execute(&mut self.storage)
            .await?;
        }

        Ok(())
    }
}
