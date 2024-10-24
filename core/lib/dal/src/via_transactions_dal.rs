use sqlx::types::chrono::NaiveDateTime;
use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::{l1::L1Tx, Address, L1BlockNumber, PriorityOpId, H256};
use zksync_utils::u256_to_big_decimal;

use crate::Core;

#[derive(Debug)]
pub struct ViaTransactionsDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Core>,
}

impl ViaTransactionsDal<'_, '_> {
    pub async fn insert_transaction_l1(
        &mut self,
        tx: &L1Tx,
        l1_block_number: L1BlockNumber,
        tx_id: H256,
    ) -> DalResult<()> {
        let contract_address = tx.execute.contract_address.as_bytes();
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

        sqlx::query!(
            r#"
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
            VALUES
                (
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
                )
            ON CONFLICT (hash) DO NOTHING
            "#,
            tx_hash_bytes,
            sender,
            gas_limit,
            max_fee_per_gas,
            gas_per_pubdata_limit,
            json_data,
            serial_id,
            full_fee,
            layer_2_tip_fee,
            contract_address,
            l1_block_number.0 as i32,
            value,
            empty_address.as_bytes(),
            &[] as &[u8],
            tx_format,
            to_mint,
            refund_recipient,
            received_at,
            signature,
        )
        .instrument("insert_transaction_l1")
        .with_arg("tx_hash", &tx_hash)
        .fetch_optional(self.storage)
        .await?;
        Ok(())
    }

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
        .fetch_optional(self.storage)
        .await?;

        Ok(maybe_row
            .and_then(|row| row.l1_block_number)
            .map(|number| L1BlockNumber(number as u32)))
    }

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
        .fetch_optional(self.storage)
        .await?;

        Ok(maybe_row
            .and_then(|row| row.op_id)
            .map(|op_id| PriorityOpId(op_id as u64)))
    }

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
        .fetch_optional(self.storage)
        .await?;

        Ok(maybe_row.is_some())
    }
}
