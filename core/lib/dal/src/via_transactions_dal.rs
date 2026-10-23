use sqlx::types::chrono::NaiveDateTime;
use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::{l1::L1Tx, u256_to_big_decimal, Address, L1BlockNumber, PriorityOpId, H256};

use crate::{Core,CoreDal};

#[derive(Debug)]
pub struct ViaTransactionsDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Core>,
}

impl ViaTransactionsDal<'_, '_> {
    /// Inserts an L1 transaction. If a transaction with the same hash already exists, the insert is skipped.
    pub async fn insert_transaction_l1(
        &mut self,
        tx: &L1Tx,
        l1_block_number: L1BlockNumber,
        tx_id: H256,
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_btc_watch().await.map_err(|error| zksync_db_connection::instrument::Instrumented::new("insert_transaction_l1").constraint_error(error))?;
        let contract_address = tx.execute.contract_address.unwrap_or_default().0.to_vec();

        let tx_hash = tx.hash();
        let tx_hash_bytes = tx_hash.as_bytes();
        let json_data = serde_json::to_value(&tx.execute)
            .unwrap_or_else(|_| panic!("cannot serialize tx {:?} to json", tx.hash()));
        let gas_limit = u256_to_big_decimal(tx.common_data.gas_limit);
        let max_fee_per_gas = u256_to_big_decimal(tx.common_data.max_fee_per_gas);
        let full_fee = u256_to_big_decimal(tx.common_data.full_fee);
        let layer_2_tip_fee = u256_to_big_decimal(tx.common_data.layer_2_tip_fee);
        let sender = tx.common_data.sender.as_bytes();
        let serial_id = tx.serial_id().0 as i64;
        let gas_per_pubdata_limit = u256_to_big_decimal(tx.common_data.gas_per_pubdata_limit);
        let value = u256_to_big_decimal(tx.execute.value);
        let tx_format = tx.common_data.tx_format() as i32;
        let empty_address = Address::default();

        let to_mint = u256_to_big_decimal(tx.common_data.to_mint);
        let refund_recipient = tx.common_data.refund_recipient.as_bytes();

        let secs = (tx.received_timestamp_ms / 1000) as i64;
        let nanosecs = ((tx.received_timestamp_ms % 1000) * 1_000_000) as u32;
        #[allow(deprecated)]
        let received_at = NaiveDateTime::from_timestamp_opt(secs, nanosecs).unwrap();

        // we keep the signature in the database as a bitcoin tx_id
        let signature = tx_id.as_bytes();

        let written=sqlx::query(r#"
            INSERT INTO
            transactions (
                hash,
                is_priority,
                initiator_address,
                gas_limit,
                max_fee_per_gas,
                gas_per_pubdata_limit,
                data,
                priority_op_id,
                full_fee,
                layer_2_tip_fee,
                contract_address,
                l1_block_number,
                value,
                paymaster,
                paymaster_input,
                tx_format,
                l1_tx_mint,
                l1_tx_refund_recipient,
                received_at,
                signature,
                created_at,
                updated_at
            )
            SELECT
                $1,
                TRUE,
                $2,
                $3,
                $4,
                $5,
                $6,
                $7,
                $8,
                $9,
                $10,
                $11,
                $12,
                $13,
                $14,
                $15,
                $16,
                $17,
                $18,
                $19,
                NOW(),
                NOW()
            WHERE (SELECT COUNT(*) FROM via_deposit_inclusions inclusion
                JOIN via_l1_blocks canonical ON canonical.number=inclusion.block_height AND canonical.hash=inclusion.block_hash
                WHERE inclusion.operation_hash=$1 AND inclusion.revoked_at IS NULL)=1
            ON CONFLICT (hash) DO UPDATE SET data = via_btc_immutable_payload(jsonb_build_object('is_priority',transactions.is_priority,'initiator_address',transactions.initiator_address,'gas_limit',transactions.gas_limit,'max_fee_per_gas',transactions.max_fee_per_gas,'gas_per_pubdata_limit',transactions.gas_per_pubdata_limit,'data',transactions.data,'priority_op_id',transactions.priority_op_id,'full_fee',transactions.full_fee,'layer_2_tip_fee',transactions.layer_2_tip_fee,'contract_address',transactions.contract_address,'l1_block_number',transactions.l1_block_number,'value',transactions.value,'paymaster',transactions.paymaster,'paymaster_input',transactions.paymaster_input,'tx_format',transactions.tx_format,'l1_tx_mint',transactions.l1_tx_mint,'l1_tx_refund_recipient',transactions.l1_tx_refund_recipient,'signature',transactions.signature),jsonb_build_object('is_priority',EXCLUDED.is_priority,'initiator_address',EXCLUDED.initiator_address,'gas_limit',EXCLUDED.gas_limit,'max_fee_per_gas',EXCLUDED.max_fee_per_gas,'gas_per_pubdata_limit',EXCLUDED.gas_per_pubdata_limit,'data',EXCLUDED.data,'priority_op_id',EXCLUDED.priority_op_id,'full_fee',EXCLUDED.full_fee,'layer_2_tip_fee',EXCLUDED.layer_2_tip_fee,'contract_address',EXCLUDED.contract_address,'l1_block_number',EXCLUDED.l1_block_number,'value',EXCLUDED.value,'paymaster',EXCLUDED.paymaster,'paymaster_input',EXCLUDED.paymaster_input,'tx_format',EXCLUDED.tx_format,'l1_tx_mint',EXCLUDED.l1_tx_mint,'l1_tx_refund_recipient',EXCLUDED.l1_tx_refund_recipient,'signature',EXCLUDED.signature)) -> 'data'
            "#)
        .bind(tx_hash_bytes)
        .bind(sender)
        .bind(gas_limit)
        .bind(max_fee_per_gas)
        .bind(gas_per_pubdata_limit)
        .bind(json_data)
        .bind(serial_id)
        .bind(full_fee)
        .bind(layer_2_tip_fee)
        .bind(contract_address)
        .bind(l1_block_number.0 as i32)
        .bind(value)
        .bind(empty_address.as_bytes())
        .bind(&[] as &[u8])
        .bind(tx_format)
        .bind(to_mint)
        .bind(refund_recipient)
        .bind(received_at)
        .bind(signature)
        .instrument("insert_transaction_l1")
        .report_latency()
        .with_arg("tx_hash", &tx_hash)
        .execute(self.storage)
        .await?;
        if written.rows_affected()!=1 {
            return Err(zksync_db_connection::instrument::Instrumented::new("insert_deposit_operation")
                .constraint_error(anyhow::anyhow!("deposit operation has no unique canonical inclusion")));
        }
        Ok(())
    }

    /// Retrieves the L1 block number of the most recently processed priority transaction.
    /// Queries transactions table ordered by priority operation ID in descending order.
    pub async fn get_last_processed_l1_block(&mut self) -> DalResult<Option<L1BlockNumber>> {
        let maybe_row = sqlx::query!(
            r#"
            SELECT
                l1_block_number
            FROM
                transactions
            WHERE
                priority_op_id IS NOT NULL
            ORDER BY
                priority_op_id DESC
            LIMIT
                1
            "#
        )
        .instrument("get_last_processed_l1_block")
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(maybe_row
            .and_then(|row| row.l1_block_number)
            .map(|number| L1BlockNumber(number as u32)))
    }

    /// Returns the highest priority operation ID from all priority transactions in the database.
    pub async fn last_priority_id(&mut self) -> DalResult<Option<PriorityOpId>> {
        let maybe_row = sqlx::query!(
            r#"
            SELECT
                MAX(priority_op_id) AS "op_id"
            FROM
                transactions
            WHERE
                is_priority = TRUE
            "#
        )
        .instrument("last_priority_id")
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(maybe_row
            .and_then(|row| row.op_id)
            .map(|op_id| PriorityOpId(op_id as u64)))
    }

    /// Checks if a transaction with the given transaction ID exists in the database.
    pub async fn transaction_exists_with_txid(&mut self, tx_id: &H256) -> DalResult<bool> {
        let maybe_row = sqlx::query!(
            r#"
            SELECT
                1 AS cnt
            FROM
                transactions
            WHERE
                signature = $1
            LIMIT
                1
            "#,
            tx_id.as_bytes(),
        )
        .instrument("transaction_exists_with_txid")
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(maybe_row.is_some())
    }

    // Removes transactions that are affected by a detected L1 reorg event.
    pub async fn delete_priority_txs(&mut self, l1_block_number: i32) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_btc_watch().await.map_err(|error| zksync_db_connection::instrument::Instrumented::new("delete_priority_txs").constraint_error(error))?;
        let protected:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM transactions WHERE is_priority
            AND l1_block_number>$1 AND (l1_batch_number IS NOT NULL OR miniblock_number IS NOT NULL))")
            .bind(l1_block_number).instrument("protected_deposit_queue").fetch_one(&mut *self.storage).await?;
        if protected {
            return Err(zksync_db_connection::instrument::Instrumented::new("remove_txs_affected_by_reorg")
                .constraint_error(anyhow::anyhow!("affected deposit is already assigned to L2; protected recovery is required")));
        }
        sqlx::query("UPDATE via_deposit_inclusions SET revoked_at=NOW() WHERE block_height>$1 AND revoked_at IS NULL")
            .bind(i64::from(l1_block_number)).instrument("revoke_deposit_inclusions").execute(&mut *self.storage).await?;
        sqlx::query("DELETE FROM transactions WHERE is_priority AND l1_block_number>$1
            AND l1_batch_number IS NULL AND miniblock_number IS NULL")
            .bind(l1_block_number).instrument("remove_txs_affected_by_reorg").execute(self.storage).await?;

        Ok(())
    }
}


// jscpd:ignore-start Independent database queue owners retain the same immutable outpoint / inclusion contract.
impl ViaTransactionsDal<'_, '_> {
    pub async fn record_deposit_evidence(
        &mut self, txid: bitcoin::Txid, vout: u32, payload: &serde_json::Value,
        inclusion: via_reorg::chain::MessageRef<bitcoin::BlockHash,bitcoin::Txid>, operation_hash: H256,
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_btc_watch().await.map_err(|error| zksync_db_connection::instrument::Instrumented::new("record_deposit_evidence").constraint_error(error))?;
        use bitcoin::hashes::Hash;
        use zksync_db_connection::instrument::Instrumented;
        if !self.storage.in_transaction() {
            return Err(Instrumented::new("record_deposit_evidence").constraint_error(anyhow::anyhow!("deposit requires an effects transaction")));
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
