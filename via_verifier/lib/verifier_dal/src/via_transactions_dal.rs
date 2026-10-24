use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::{H256, Address, PriorityOpId};
use via_btc_client::indexer::replay::{
    DepositOrigin, DepositPayload, DepositReplay, ReplayConflict, compare_deposit_replay,
    retained_deposit_operation,
};

use crate::Verifier;

#[derive(Debug)]
pub struct L1ToL2Transaction {
    pub priority_id: i64,
    pub l1_block_number: i64,
    pub tx_id: H256,
    pub receiver: String,
    pub value: i64,
    pub calldata: Vec<u8>,
    pub canonical_tx_hash: H256,
}

#[derive(Debug)]
pub struct ViaTransactionsDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Verifier>,
}

impl ViaTransactionsDal<'_, '_> {
    pub async fn insert_transaction(
        &mut self,
        tx: L1ToL2Transaction,
        origin: &DepositOrigin,
        payload: &DepositPayload,
    ) -> anyhow::Result<bool> {
        let source_hash = origin.hash.to_string();
        let old = sqlx::query!(
            "SELECT * FROM via_transactions WHERE tx_id=$1 AND COALESCE(bitcoin_vout, priority_id & 65535)=$2",
            tx.tx_id.as_bytes(),
            i64::from(origin.vout)
        )
        .instrument("verifier_deposit_replay")
        .fetch_optional(self.storage)
        .await?;
        let json_payload = serde_json::to_value(payload)?;
        if let Some(mut old) = old {
            let attached = old.status.is_some() || old.l1_batch_number.is_some();
            if let DepositReplay::NeedsRetainedCheck = compare_deposit_replay(
                payload,
                &source_hash,
                old.bitcoin_hash.as_deref(),
                old.bitcoin_payload.take(),
                attached,
            )? {
                anyhow::ensure!(
                    old.value >= 0 && old.value as u64 == payload.input.1,
                    ReplayConflict { field: "amount" }
                );
                let receiver = match old.receiver.parse::<Address>() {
                    Ok(receiver) => receiver,
                    Err(_)
                        if old.receiver.contains('…')
                            && old.receiver == payload.input.0.receiver_l2_address.to_string() =>
                    {
                        payload.input.0.receiver_l2_address
                    }
                    Err(_) => return Err(ReplayConflict { field: "receiver" }.into()),
                };
                retained_deposit_operation(
                    PriorityOpId(old.priority_id as u64),
                    receiver,
                    payload.input.1,
                    H256::from_slice(&old.canonical_tx_hash),
                )?;
            }
            sqlx::query!(
                "UPDATE via_transactions SET bitcoin_height=$3, bitcoin_hash=$4,
                bitcoin_payload=$5, bitcoin_vout=$2 WHERE tx_id=$1 AND COALESCE(bitcoin_vout, priority_id & 65535)=$2",
                &old.tx_id,
                i64::from(origin.vout),
                i64::from(origin.height),
                source_hash,
                json_payload
            )
            .instrument("verifier_deposit_reconcile_origin")
            .execute(self.storage)
            .await?;
            return Ok(false);
        }

        let L1ToL2Transaction {
            priority_id,
            tx_id,
            receiver,
            value,
            calldata,
            canonical_tx_hash,
            l1_block_number,
        } = tx;
        let inserted = sqlx::query!(
            r#"
            INSERT INTO
            via_transactions (
                priority_id,
                tx_id,
                receiver,
                value,
                calldata,
                canonical_tx_hash,
                l1_block_number, bitcoin_height, bitcoin_hash, bitcoin_vout, bitcoin_payload
            )
            VALUES
            ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT DO NOTHING RETURNING tx_id
            "#,
            priority_id,
            tx_id.as_bytes(),
            receiver,
            value,
            calldata,
            canonical_tx_hash.as_bytes(),
            l1_block_number, i64::from(origin.height), source_hash, i64::from(origin.vout), json_payload,
        )
        .instrument("insert_transaction")
        .fetch_optional(self.storage)
        .await?;

        anyhow::ensure!(inserted.is_some(), ReplayConflict { field: "outpoint" });
        Ok(true)
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

    /// First batch containing a deposit / priority transaction removed by a reorg.
    /// `l1_block_number` is the last retained Bitcoin height, not the first removed height.
    pub async fn get_l1_batch_number_affected_by_reorg(
        &mut self,
        l1_block_number: i64,
    ) -> DalResult<Option<i64>> {
        sqlx::query_scalar(
            r#"
            SELECT MIN(l1_batch_number)
            FROM via_transactions
            WHERE l1_block_number > $1
            "#,
        )
        .bind(l1_block_number)
        .instrument("get_l1_batch_number_affected_by_reorg")
        .with_arg("last_retained_l1_block", &l1_block_number)
        .report_latency()
        .fetch_one(self.storage)
        .await
    }

    pub async fn get_not_finalized_transactions(&mut self, l1_block_number: i64) -> DalResult<i64> {
        let record = sqlx::query!(
            r#"
            SELECT
                COUNT(l1_block_number) AS "count!"
            FROM
                via_transactions
            WHERE
                l1_batch_number IS NULL
                AND l1_block_number > $1
            "#,
            l1_block_number,
        )
        .instrument("get_not_finalized_transactions")
        .report_latency()
        .fetch_one(self.storage)
        .await?;

        Ok(record.count)
    }

    pub async fn delete_transactions(&mut self, l1_block_number: i64) -> DalResult<()> {
        sqlx::query!(
            r#"
            DELETE FROM via_transactions
            WHERE
                l1_block_number > $1
            "#,
            l1_block_number,
        )
        .instrument("delete_transactions")
        .execute(self.storage)
        .await?;

        Ok(())
    }

    pub async fn reset_transactions(&mut self, l1_batch_number: i64) -> DalResult<()> {
        sqlx::query_scalar!(
            r#"
            UPDATE via_transactions SET
                l1_batch_number = NULL,
                status = NULL
            WHERE
                l1_batch_number > $1
            "#,
            l1_batch_number,
        )
        .instrument("reset_transactions")
        .execute(self.storage)
        .await?;

        Ok(())
    }
}
