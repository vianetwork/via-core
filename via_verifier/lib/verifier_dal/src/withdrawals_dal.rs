use std::str::FromStr;

use bitcoin::{Address as BitcoinAddress, Amount};
use via_verifier_types::withdrawal::WithdrawalRequest;
use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::Address;

use crate::Verifier;

pub struct ViaWithdrawalDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Verifier>,
}

impl ViaWithdrawalDal<'_, '_> {
    pub async fn insert_l1_batch_bridge_withdrawals(
        &mut self,
        proof_reveal_tx_id: &[u8],
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
            via_l1_batch_bridge_withdrawals (proof_reveal_tx_id)
            VALUES
            ($1)
            ON CONFLICT (proof_reveal_tx_id) DO NOTHING
            "#,
            &proof_reveal_tx_id,
        )
        .instrument("insert_l1_batch_bridge_withdrawals")
        .execute(&mut self.storage)
        .await?;

        Ok(())
    }

    pub async fn insert_bridge_withdrawal_tx(&mut self, tx_id: &[u8]) -> DalResult<i64> {
        let row = sqlx::query!(
            r#"
            INSERT INTO via_bridge_withdrawals (tx_id)
            VALUES ($1)
            ON CONFLICT (tx_id) DO NOTHING
            RETURNING id
            "#,
            tx_id,
        )
        .instrument("insert_bridge_withdrawal_tx")
        .fetch_one(&mut self.storage)
        .await?;

        Ok(row.id)
    }

    pub async fn mark_bridge_withdrawal_tx_as_processed(&mut self, tx_id: &[u8]) -> DalResult<()> {
        sqlx::query!(
            r#"
            UPDATE
            via_bridge_withdrawals
            SET
                executed = TRUE,
                updated_at = NOW()
            WHERE
                tx_id = $1
            "#,
            tx_id,
        )
        .instrument("mark_bridge_withdrawal_tx_as_processed")
        .execute(&mut self.storage)
        .await?;

        Ok(())
    }

    pub async fn bridge_withdrawal_exists(&mut self, tx_id: &[u8]) -> DalResult<bool> {
        let row = sqlx::query!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM via_bridge_withdrawals
                WHERE tx_id = $1
            ) AS "exists!"
            "#,
            tx_id,
        )
        .instrument("bridge_withdrawal_exists")
        .fetch_one(&mut self.storage)
        .await?;

        Ok(row.exists)
    }

    pub async fn get_bridge_withdrawal_id(&mut self, tx_id: &[u8]) -> DalResult<Option<i64>> {
        let row = sqlx::query!(
            r#"
            SELECT
                id
            FROM
                via_bridge_withdrawals
            WHERE
                tx_id = $1
            "#,
            tx_id,
        )
        .instrument("get_bridge_withdrawal_id")
        .fetch_optional(&mut self.storage)
        .await?;

        Ok(row.map(|r| r.id))
    }

    /// Inserts a new withdrawals.
    pub async fn insert_withdrawals(
        &mut self,
        withdrawals: &[WithdrawalRequest],
    ) -> DalResult<Vec<bool>> {
        let mut results = Vec::with_capacity(withdrawals.len());

        for withdrawal in withdrawals {
            let res = sqlx::query!(
                r#"
                INSERT INTO
                via_withdrawals (id, l2_tx_hash, l2_tx_log_index, receiver, value)
                VALUES
                ($1, $2, $3, $4, $5)
                ON CONFLICT (id)
                DO UPDATE SET
                value = excluded.value,
                l2_tx_hash = CASE
                    WHEN excluded.l2_tx_hash <> '' THEN excluded.l2_tx_hash
                    ELSE via_withdrawals.l2_tx_hash
                END,
                updated_at = NOW();
                "#,
                withdrawal.id,
                withdrawal.l2_tx_hash,
                withdrawal.l2_tx_log_index as i64,
                withdrawal.receiver.to_string(),
                withdrawal.amount.to_sat() as i64,
            )
            .instrument("insert_withdrawals")
            .report_latency()
            .execute(&mut self.storage)
            .await?;

            results.push(res.rows_affected() > 0);
        }

        Ok(results)
    }

    pub async fn mark_withdrawals_as_processed(
        &mut self,
        bridge_withdrawal_id: i64,
        withdrawals: &[WithdrawalRequest],
    ) -> DalResult<()> {
        for w in withdrawals {
            self.mark_withdrawal_as_processed(bridge_withdrawal_id, &w)
                .await?;
        }
        Ok(())
    }

    pub async fn mark_withdrawal_as_processed(
        &mut self,
        bridge_withdrawal_id: i64,
        withdrawal: &WithdrawalRequest,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            UPDATE via_withdrawals SET
                bridge_withdrawal_id = $4,
                updated_at = NOW()
            WHERE
                id = $1
                AND l2_tx_log_index = $2
                AND receiver = $3
            "#,
            withdrawal.id,
            withdrawal.l2_tx_log_index as i64,
            withdrawal.receiver.to_string(),
            bridge_withdrawal_id
        )
        .instrument("mark_withdrawal_as_processed")
        .execute(&mut self.storage)
        .await?;

        Ok(())
    }

    pub async fn check_if_withdrawal_exists_unprocessed(
        &mut self,
        withdrawal: &WithdrawalRequest,
    ) -> DalResult<bool> {
        let row = sqlx::query!(
            r#"
            SELECT
                id
            FROM
                via_withdrawals
            WHERE
                id = $1
                AND l2_tx_log_index = $2
                AND receiver = $3
                AND bridge_withdrawal_id IS NULL
            "#,
            withdrawal.id,
            withdrawal.l2_tx_log_index as i64,
            withdrawal.receiver.to_string(),
        )
        .instrument("check_if_withdrawal_exists_unprocessed")
        .fetch_optional(&mut self.storage)
        .await?;

        Ok(row.is_some())
    }

    pub async fn check_if_withdrawal_exists(
        &mut self,
        withdrawal: &WithdrawalRequest,
    ) -> DalResult<bool> {
        let row = sqlx::query!(
            r#"
            SELECT EXISTS(
                SELECT
                    1
                FROM
                    via_withdrawals
                WHERE
                    id = $1
                    AND l2_tx_log_index = $2
                    AND receiver = $3
            ) AS "exists!"
            "#,
            withdrawal.id,
            withdrawal.l2_tx_log_index as i64,
            withdrawal.receiver.to_string(),
        )
        .instrument("check_if_withdrawal_exists")
        .fetch_one(&mut self.storage)
        .await?;

        Ok(row.exists)
    }

    pub async fn list_no_processed_withdrawals(
        &mut self,
        min_value: i64,
        limit: u32,
    ) -> anyhow::Result<Vec<WithdrawalRequest>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                id,
                l2_tx_hash,
                l2_tx_log_index,
                receiver,
                value
            FROM
                via_withdrawals
            WHERE
                value >= $1 AND bridge_withdrawal_id IS NULL
            LIMIT $2
            "#,
            min_value,
            limit as i64,
        )
        .instrument("list_no_processed_withdrawals")
        .fetch_all(&mut self.storage)
        .await?;

        let withdrawals = rows
            .into_iter()
            .map(|row| {
                Ok(WithdrawalRequest {
                    id: row.id,
                    receiver: BitcoinAddress::from_str(&row.receiver)?.assume_checked(),
                    amount: Amount::from_sat(row.value as u64),
                    l2_sender: Address::zero(),
                    l2_tx_hash: row.l2_tx_hash,
                    l2_tx_log_index: row.l2_tx_log_index as u16,
                })
            })
            .collect::<Result<Vec<_>, anyhow::Error>>();

        withdrawals
    }

    pub async fn get_finalized_block_and_non_processed_withdrawal(
        &mut self,
        l1_batch_number: i64,
    ) -> DalResult<Option<(String, Vec<u8>)>> {
        let result = sqlx::query!(
            r#"
            SELECT
                v.pubdata_blob_id,
                v.proof_reveal_tx_id
            FROM
                via_votable_transactions v
            LEFT JOIN
                via_l1_batch_bridge_withdrawals b
                ON b.proof_reveal_tx_id = v.proof_reveal_tx_id
            WHERE
                v.is_finalized = TRUE
                AND v.l1_batch_status = TRUE
                AND v.l1_batch_number = $1
                AND b.proof_reveal_tx_id IS NULL
            ORDER BY
                v.l1_batch_number ASC
            LIMIT
                1
            "#,
            l1_batch_number
        )
        .instrument("get_finalized_block_and_non_processed_withdrawal")
        .fetch_optional(self.storage)
        .await?;

        let mapped_result = result.map(|row| (row.pubdata_blob_id, row.proof_reveal_tx_id));
        Ok(mapped_result)
    }

    pub async fn list_finalized_blocks_with_no_bridge_withdrawal(
        &mut self,
    ) -> DalResult<Vec<(i64, String, Vec<u8>)>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                v.l1_batch_number,
                v.pubdata_blob_id,
                v.proof_reveal_tx_id
            FROM
                via_votable_transactions v
            LEFT JOIN
                via_l1_batch_bridge_withdrawals b
                ON b.proof_reveal_tx_id = v.proof_reveal_tx_id
            WHERE
                v.is_finalized = TRUE
                AND v.l1_batch_status = TRUE
                AND b.proof_reveal_tx_id IS NULL
            ORDER BY
                v.l1_batch_number ASC
            "#
        )
        .instrument("list_finalized_blocks_with_no_bridge_withdrawal")
        .fetch_all(self.storage)
        .await?;

        let result: Vec<(i64, String, Vec<u8>)> = rows
            .into_iter()
            .map(|r| (r.l1_batch_number, r.pubdata_blob_id, r.proof_reveal_tx_id))
            .collect();

        Ok(result)
    }
}
