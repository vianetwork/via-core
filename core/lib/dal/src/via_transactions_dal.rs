use sqlx::types::chrono::NaiveDateTime;
use via_btc_client::indexer::replay::{
    compare_deposit_replay, retained_deposit_operation, DepositOrigin, DepositPayload,
    DepositReplay, ReplayConflict,
};
use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::{l1::L1Tx, u256_to_big_decimal, Address, L1BlockNumber, PriorityOpId, H256};

use crate::{models::storage_transaction::StorageTransaction, Core};

#[derive(Debug)]
pub struct ViaTransactionsDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Core>,
}

impl ViaTransactionsDal<'_, '_> {
    pub async fn filter_mature_mempool(
        &mut self,
        transactions: &mut Vec<(
            zksync_types::Transaction,
            zksync_types::TransactionTimeRangeConstraint,
        )>,
    ) -> anyhow::Result<()> {
        let priorities: Vec<_> = transactions
            .iter()
            .filter(|(transaction, _)| transaction.is_l1())
            .map(|(transaction, _)| transaction.hash().as_bytes().to_vec())
            .collect();
        let eligible = sqlx::query_as::<_, (Vec<u8>,)>("SELECT t.hash FROM transactions t JOIN via_btc_headers h
            ON h.number=t.bitcoin_height AND h.hash=t.bitcoin_hash CROSS JOIN via_btc_reader r
            WHERE t.hash=ANY($1) AND t.bitcoin_payload IS NOT NULL AND r.replay_ready AND r.stopped_reason IS NULL
            AND h.number<=r.confirmed_number")
            .bind(&priorities).instrument("bitcoin_mature_mempool").fetch_all(self.storage).await?;
        let eligible: std::collections::HashSet<_> =
            eligible.into_iter().map(|(hash,)| hash).collect();
        let blocked: Vec<_> = priorities
            .into_iter()
            .filter(|hash| !eligible.contains(hash))
            .collect();
        sqlx::query("UPDATE transactions SET in_mempool=FALSE WHERE hash=ANY($1)")
            .bind(&blocked)
            .instrument("bitcoin_release_held_mempool")
            .execute(self.storage)
            .await?;
        transactions.retain(|(transaction, _)| {
            !transaction.is_l1() || eligible.contains(transaction.hash().as_bytes())
        });
        Ok(())
    }

    /// insert_transaction_l1 inserts new deposit, or on same outpoint compares replay and keeps operation already attached to L2.
    /// Returns true only when new row was inserted.
    pub async fn insert_transaction_l1(
        &mut self,
        tx: &L1Tx,
        l1_block_number: L1BlockNumber,
        tx_id: H256,
        origin: &DepositOrigin,
        payload: &DepositPayload,
    ) -> anyhow::Result<bool> {
        let mut existing = sqlx::query_as!(
            StorageTransaction,
            "SELECT * FROM transactions WHERE is_priority AND signature=$1
             AND COALESCE(bitcoin_vout, priority_op_id & 65535)=$2 LIMIT 2",
            tx_id.as_bytes(),
            i64::from(origin.vout)
        )
        .instrument("deposit_replay_lookup")
        .fetch_all(self.storage)
        .await?;
        anyhow::ensure!(
            existing.len() <= 1,
            ReplayConflict {
                field: "ambiguous_outpoint"
            }
        );
        let source_hash = origin.hash.to_string();
        let json_payload = serde_json::to_value(payload)?;
        if let Some(mut old) = existing.pop() {
            let attached = old.l1_batch_number.is_some() || old.miniblock_number.is_some();
            let old_hash = old.hash.clone();
            let saved_hash = old.bitcoin_hash.take();
            let saved_payload = old.bitcoin_payload.take();
            if let DepositReplay::NeedsRetainedCheck = compare_deposit_replay(
                payload,
                &source_hash,
                saved_hash.as_deref(),
                saved_payload,
                attached,
            )? {
                let expected = retained_deposit_operation(
                    PriorityOpId(old.priority_op_id.ok_or(ReplayConflict {
                        field: "priority_id",
                    })? as u64),
                    Address::from_slice(&old.initiator_address),
                    payload.input.1,
                    H256::from_slice(&old.hash),
                )?;
                let retained: zksync_types::Transaction = old.into();
                anyhow::ensure!(
                    retained.execute == expected.execute
                        && retained.common_data
                            == zksync_types::ExecuteTransactionCommon::L1(expected.common_data),
                    ReplayConflict {
                        field: "retained_execution"
                    }
                );
            }
            sqlx::query!(
                "UPDATE transactions SET bitcoin_height=$2, bitcoin_hash=$3,
                bitcoin_vout=$4, bitcoin_payload=$5 WHERE hash=$1",
                &old_hash,
                i64::from(origin.height),
                source_hash,
                i64::from(origin.vout),
                json_payload
            )
            .instrument("deposit_reconcile_origin")
            .execute(self.storage)
            .await?;
            return Ok(false);
        }

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

        let inserted = sqlx::query!(
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
                updated_at, bitcoin_height, bitcoin_hash, bitcoin_vout, bitcoin_payload
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
                NOW(), $20, $21, $22, $23
            )
            ON CONFLICT (hash) DO NOTHING RETURNING hash
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
            signature, i64::from(origin.height), source_hash, i64::from(origin.vout), json_payload,
        )
        .instrument("insert_transaction_l1")
        .report_latency()
        .with_arg("tx_hash", &tx_hash)
        .fetch_optional(self.storage)
        .await?;
        anyhow::ensure!(
            inserted.is_some(),
            ReplayConflict {
                field: "canonical_tx_hash"
            }
        );
        Ok(true)
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
        sqlx::query!(
            r#"
            DELETE FROM transactions
            WHERE
                is_priority = TRUE
                AND l1_block_number > $1
            "#,
            l1_block_number
        )
        .instrument("remove_txs_affected_by_reorg")
        .report_latency()
        .execute(self.storage)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::{hashes::Hash, BlockHash};
    use via_btc_client::{indexer::replay::Payload, types::L1ToL2MessageInput};
    use zksync_types::l1::via_l1::ViaL1Deposit;

    use super::*;
    use crate::{ConnectionPool, CoreDal};

    async fn deposit_fixture() -> anyhow::Result<(
        ConnectionPool<Core>,
        ViaL1Deposit,
        DepositOrigin,
        DepositPayload,
    )> {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let mut db = pool.connection().await?;
        let deposit = ViaL1Deposit {
            l2_receiver_address: Address::repeat_byte(7),
            amount: 50_000,
            calldata: vec![],
            l1_block_number: 10,
            tx_index: 1,
            output_vout: 0,
        };
        let original = deposit.l1_tx().unwrap();
        let origin = DepositOrigin {
            height: 10,
            hash: BlockHash::from_byte_array([10; 32]),
            vout: 0,
        };
        let payload = Payload {
            input: (
                L1ToL2MessageInput {
                    receiver_l2_address: deposit.l2_receiver_address,
                    l2_contract_address: Address::zero(),
                    call_data: vec![],
                },
                deposit.amount,
            ),
            public_key: vec![2; 33],
            signature: vec![3; 64],
            sender_script: None,
        };
        let txid = H256::repeat_byte(8);
        assert!(
            db.via_transactions_dal()
                .insert_transaction_l1(&original, original.eth_block(), txid, &origin, &payload)
                .await?
        );
        drop(db);
        Ok((pool, deposit, origin, payload))
    }

    async fn assert_deposit_refused(
        db: &mut Connection<'_, Core>,
        operation: &L1Tx,
        origin: &DepositOrigin,
        payload: &DepositPayload,
        field: &str,
    ) {
        let error = db
            .via_transactions_dal()
            .insert_transaction_l1(
                operation,
                operation.eth_block(),
                H256::repeat_byte(8),
                origin,
                payload,
            )
            .await
            .unwrap_err();
        assert_eq!(error.downcast_ref::<ReplayConflict>().unwrap().field, field);
    }

    #[tokio::test]
    async fn unattached_main_replay_requires_the_same_inclusion() -> anyhow::Result<()> {
        let (pool, deposit, mut origin, payload) = deposit_fixture().await?;
        let mut db = pool.connection().await?;
        let tx = deposit.l1_tx().unwrap();
        let txid = H256::repeat_byte(8);
        assert!(
            !db.via_transactions_dal()
                .insert_transaction_l1(&tx, tx.eth_block(), txid, &origin, &payload)
                .await?
        );
        origin.hash = BlockHash::from_byte_array([11; 32]);
        assert_deposit_refused(&mut db, &tx, &origin, &payload, "deposit_inclusion").await;
        let saved: String = sqlx::query_scalar("SELECT bitcoin_hash FROM transactions")
            .instrument("unchanged_pending_source")
            .fetch_one(&mut db)
            .await?;
        assert_eq!(saved, BlockHash::from_byte_array([10; 32]).to_string());
        Ok(())
    }

    #[tokio::test]
    async fn unattached_main_legacy_payload_is_not_rebound() -> anyhow::Result<()> {
        let (pool, deposit, origin, payload) = deposit_fixture().await?;
        let mut db = pool.connection().await?;
        sqlx::query("UPDATE transactions SET bitcoin_payload=NULL")
            .instrument("unbound_pending_payload")
            .execute(&mut db)
            .await?;
        let tx = deposit.l1_tx().unwrap();
        assert_deposit_refused(&mut db, &tx, &origin, &payload, "unreconciled_deposit").await;
        let saved: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT bitcoin_payload FROM transactions")
                .instrument("still_unbound_pending_payload")
                .fetch_one(&mut db)
                .await?;
        assert!(saved.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn ambiguous_main_outpoint_leaves_both_candidates_unchanged() -> anyhow::Result<()> {
        let (pool, deposit, origin, payload) = deposit_fixture().await?;
        let mut db = pool.connection().await?;
        let other = ViaL1Deposit {
            output_vout: 1,
            ..deposit.clone()
        }
        .l1_tx()
        .unwrap();
        let txid = H256::repeat_byte(8);
        let other_origin = DepositOrigin {
            vout: 1,
            height: origin.height,
            hash: origin.hash,
        };
        db.via_transactions_dal()
            .insert_transaction_l1(&other, other.eth_block(), txid, &other_origin, &payload)
            .await?;
        sqlx::query("UPDATE transactions SET bitcoin_vout=0")
            .instrument("ambiguous_outpoint_fixture")
            .execute(&mut db)
            .await?;
        let before: serde_json::Value =
            sqlx::query_scalar("SELECT jsonb_agg(t ORDER BY hash) FROM transactions t")
                .instrument("before_ambiguous_replay")
                .fetch_one(&mut db)
                .await?;
        let tx = deposit.l1_tx().unwrap();
        assert_deposit_refused(&mut db, &tx, &origin, &payload, "ambiguous_outpoint").await;
        let after: serde_json::Value =
            sqlx::query_scalar("SELECT jsonb_agg(t ORDER BY hash) FROM transactions t")
                .instrument("after_ambiguous_replay")
                .fetch_one(&mut db)
                .await?;
        assert_eq!(after, before);
        Ok(())
    }

    #[tokio::test]
    async fn main_remine_preserves_executed_operation() -> anyhow::Result<()> {
        let (pool, deposit, mut origin, mut payload) = deposit_fixture().await?;
        let mut db = pool.connection().await?;
        let original = deposit.l1_tx().unwrap();
        let txid = H256::repeat_byte(8);
        sqlx::query("UPDATE transactions SET miniblock_number=1,bitcoin_height=NULL,bitcoin_hash=NULL,bitcoin_vout=NULL,bitcoin_payload=NULL WHERE hash=$1")
            .bind(original.hash().as_bytes()).instrument("seed_legacy_execution").execute(&mut db).await?;
        origin.height = 11;
        origin.hash = BlockHash::from_byte_array([11; 32]);
        payload.input.0.receiver_l2_address = Address::repeat_byte(9);
        let remined = ViaL1Deposit {
            l1_block_number: 11,
            l2_receiver_address: payload.input.0.receiver_l2_address,
            ..deposit.clone()
        }
        .l1_tx()
        .unwrap();
        assert!(
            !db.via_transactions_dal()
                .insert_transaction_l1(&remined, remined.eth_block(), txid, &origin, &payload)
                .await?
        );
        let row = sqlx::query("SELECT hash,priority_op_id,initiator_address,miniblock_number,l1_block_number,bitcoin_height FROM transactions")
            .instrument("retained_execution_after_remine").fetch_optional(&mut db).await?.unwrap();
        use sqlx::Row;
        assert_eq!(row.get::<Vec<u8>, _>("hash"), original.hash().as_bytes());
        assert_eq!(
            row.get::<i64, _>("priority_op_id"),
            original.serial_id().0 as i64
        );
        assert_eq!(
            row.get::<Vec<u8>, _>("initiator_address"),
            deposit.l2_receiver_address.as_bytes()
        );
        assert_eq!(row.get::<i32, _>("l1_block_number"), 10);
        assert_eq!(row.get::<i64, _>("bitcoin_height"), 11);
        assert_eq!(row.get::<i64, _>("miniblock_number"), 1);
        payload.signature[0] ^= 1;
        let error = db
            .via_transactions_dal()
            .insert_transaction_l1(&remined, remined.eth_block(), txid, &origin, &payload)
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ReplayConflict>().unwrap().field,
            "signature"
        );
        Ok(())
    }

    #[tokio::test]
    async fn main_reset_preserves_retained_parents() -> anyhow::Result<()> {
        let (pool, deposit, mut origin, payload) = deposit_fixture().await?;
        let mut db = pool.connection().await?;
        let txid = H256::repeat_byte(8);
        sqlx::query("UPDATE transactions SET miniblock_number=1")
            .instrument("seed_retained_execution")
            .execute(&mut db)
            .await?;
        let pending = ViaL1Deposit {
            output_vout: 1,
            ..deposit
        }
        .l1_tx()
        .unwrap();
        origin.vout = 1;
        db.via_transactions_dal()
            .insert_transaction_l1(&pending, pending.eth_block(), txid, &origin, &payload)
            .await?;
        sqlx::query("INSERT INTO l1_batches (number,timestamp,l1_tx_count,l2_tx_count,bloom,priority_ops_onchain_data,created_at,updated_at,initial_bootloader_heap_content,used_contract_hashes) VALUES (1,1,1,0,'','{}',NOW(),NOW(),'[]','[]')")
            .instrument("seed_retained_batch").execute(&mut db).await?;
        sqlx::query("INSERT INTO via_btc_inscriptions_request (l1_batch_number,request_type,updated_at) VALUES (1,'proof',NOW())")
            .instrument("seed_retained_request").execute(&mut db).await?;
        sqlx::query("INSERT INTO via_data_availability(l1_batch_number,is_proof,blob_id,sent_at,created_at,updated_at,index) VALUES (1,FALSE,'retained-da',NOW(),NOW(),NOW(),0)")
            .instrument("seed_retained_da").execute(&mut db).await?;
        for commit in [false, true] {
            let mut reset = db.start_transaction().await?;
            reset.via_indexer_dal().delete_reader_suffix(0).await?;
            if commit {
                reset.commit().await?;
            } else {
                drop(reset);
            }
            let count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transactions")
                .instrument("retained_deposit_count")
                .fetch_one(&mut db)
                .await?;
            assert_eq!(count, if commit { 1 } else { 2 });
        }
        let roots=sqlx::query_scalar::<_,i64>("SELECT (SELECT count(*) FROM l1_batches)+(SELECT count(*) FROM via_btc_inscriptions_request)+(SELECT count(*) FROM via_data_availability WHERE blob_id='retained-da')")
            .instrument("retained_local_roots").fetch_one(&mut db).await?;
        assert_eq!(roots, 3);
        Ok(())
    }
    #[tokio::test]
    async fn main_reset_refuses_unknown_dependents() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let mut db = pool.connection().await?;
        for column in [
            "tx_hash BYTEA REFERENCES transactions(hash)",
            "vote_id BIGINT REFERENCES via_votes(id)",
            "proof_id BIGINT REFERENCES via_btc_finalizations(proof_id)",
        ] {
            sqlx::query(&format!("CREATE TABLE unreviewed_dependency ({column})"))
                .instrument("seed_unknown_dependency")
                .execute(&mut db)
                .await?;
            let mut reset = db.start_transaction().await?;
            assert!(reset
                .via_indexer_dal()
                .delete_reader_suffix(0)
                .await
                .unwrap_err()
                .to_string()
                .contains("unknown dependent constraint"));
            drop(reset);
            sqlx::query("DROP TABLE unreviewed_dependency")
                .instrument("remove_unknown_dependency")
                .execute(&mut db)
                .await?;
        }
        Ok(())
    }
}
