use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::H256;

use crate::{Verifier, VerifierDal};

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
    pub async fn insert_transaction(&mut self, tx: L1ToL2Transaction) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("insert_transaction")
                .constraint_error(error)
        })?;
        let L1ToL2Transaction {
            priority_id,
            tx_id,
            receiver,
            value,
            calldata,
            canonical_tx_hash,
            l1_block_number,
        } = tx;
        let written=sqlx::query(r#"
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
            SELECT $1,$2,$3,$4,$5,$6,$7
            WHERE (SELECT COUNT(*) FROM via_deposit_inclusions inclusion
                JOIN via_l1_blocks canonical ON canonical.number=inclusion.block_height AND canonical.hash=inclusion.block_hash
                WHERE inclusion.operation_hash=$6 AND inclusion.revoked_at IS NULL)=1
            ON CONFLICT (canonical_tx_hash) DO UPDATE SET calldata=CASE WHEN via_btc_immutable_payload(jsonb_build_object('priority_id',via_transactions.priority_id,'tx_id',via_transactions.tx_id,'receiver',via_transactions.receiver,'value',via_transactions.value,'calldata',via_transactions.calldata,'l1_block_number',via_transactions.l1_block_number),jsonb_build_object('priority_id',EXCLUDED.priority_id,'tx_id',EXCLUDED.tx_id,'receiver',EXCLUDED.receiver,'value',EXCLUDED.value,'calldata',EXCLUDED.calldata,'l1_block_number',EXCLUDED.l1_block_number)) IS NOT NULL THEN via_transactions.calldata END
            "#)
        .bind(priority_id)
        .bind(tx_id.as_bytes())
        .bind(receiver)
        .bind(value)
        .bind(calldata)
        .bind(canonical_tx_hash.as_bytes())
        .bind(l1_block_number)
        .instrument("insert_transaction")
        .execute(self.storage)
        .await?;

        if written.rows_affected() != 1 {
            return Err(zksync_db_connection::instrument::Instrumented::new(
                "insert_deposit_operation",
            )
            .constraint_error(anyhow::anyhow!(
                "deposit operation has no unique canonical inclusion"
            )));
        }
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

    pub async fn deposit_prefix_for_batch(
        &mut self,
        batch: i64,
        limit: i64,
    ) -> DalResult<Vec<Vec<u8>>> {
        use sqlx::Row;
        sqlx::query("SELECT canonical_tx_hash FROM via_transactions WHERE status IS NULL OR l1_batch_number=$1
            ORDER BY priority_id LIMIT $2")
            .bind(batch).bind(limit).try_map(|row:sqlx::postgres::PgRow| row.try_get("canonical_tx_hash"))
            .instrument("deposit_prefix_for_batch").fetch_all(self.storage).await
    }

    pub async fn update_transaction(
        &mut self,
        canonical_tx_hash: &H256,
        status: bool,
        l1_batch_number: i64,
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("update_deposit_status")
                .constraint_error(error)
        })?;
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
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("delete_transactions")
                .constraint_error(error)
        })?;
        sqlx::query("UPDATE via_deposit_inclusions SET revoked_at=NOW() WHERE block_height>$1 AND revoked_at IS NULL")
            .bind(l1_block_number).instrument("revoke_deposit_inclusions").execute(&mut *self.storage).await?;
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
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("reset_transactions")
                .constraint_error(error)
        })?;
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


// jscpd:ignore-start Independent database queue owners retain the same immutable outpoint / inclusion contract.
impl ViaTransactionsDal<'_, '_> {
    pub async fn record_deposit_evidence(
        &mut self,
        txid: bitcoin::Txid,
        vout: u32,
        payload: &serde_json::Value,
        inclusion: via_reorg::chain::MessageRef<bitcoin::BlockHash, bitcoin::Txid>,
        operation_hash: H256,
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("record_deposit_evidence")
                .constraint_error(error)
        })?;
        use bitcoin::hashes::Hash;
        use zksync_db_connection::instrument::Instrumented;
        if !self.storage.in_transaction() {
            return Err(Instrumented::new("record_deposit_evidence")
                .constraint_error(anyhow::anyhow!("deposit requires an effects transaction")));
        }
        sqlx::query("INSERT INTO via_deposit_evidence(txid,vout,immutable_payload) VALUES($1,$2,$3)
            ON CONFLICT(txid,vout) DO UPDATE
            SET immutable_payload=via_btc_immutable_payload(via_deposit_evidence.immutable_payload,EXCLUDED.immutable_payload)")
            .bind(txid.as_byte_array().as_slice()).bind(i64::from(vout)).bind(payload)
            .instrument("deposit_evidence").execute(&mut *self.storage).await?;
        sqlx::query("INSERT INTO via_deposit_inclusions(txid,vout,carrier_txid,block_hash,block_height,tx_index,message_index,operation_hash)
            VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(txid,vout,block_hash,tx_index,message_index) WHERE revoked_at IS NULL DO UPDATE
            SET operation_hash=CASE WHEN via_deposit_inclusions.operation_hash=EXCLUDED.operation_hash
                AND via_deposit_inclusions.carrier_txid=EXCLUDED.carrier_txid AND via_deposit_inclusions.block_height=EXCLUDED.block_height
                THEN via_deposit_inclusions.operation_hash ELSE NULL END")
            .bind(txid.as_byte_array().as_slice()).bind(i64::from(vout))
            .bind(inclusion.carrier_txid.as_byte_array().as_slice()).bind(inclusion.block.block_hash.as_byte_array().as_slice())
            .bind(i64::from(inclusion.block.height)).bind(i64::from(inclusion.tx_index)).bind(i64::from(inclusion.message_index))
            .bind(operation_hash.as_bytes()).instrument("deposit_inclusion").execute(self.storage).await?;
        Ok(())
    }
}
// jscpd:ignore-end
