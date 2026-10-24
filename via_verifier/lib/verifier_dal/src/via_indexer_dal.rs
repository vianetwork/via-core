use anyhow::Context;
use zksync_db_connection::{
    advisory_lock::{AdvisoryLockKey, TransactionAdvisoryLock},
    connection::Connection,
    error::DalResult,
    instrument::InstrumentExt,
};

use crate::{Verifier, VerifierDal, BTC_WATCH_OWNER_LOCK_KEY, WITHDRAWAL_INVALIDATION_LOCK_KEY};

pub const BTC_WATCH_OWNER: TransactionAdvisoryLock = TransactionAdvisoryLock::new(
    AdvisoryLockKey::BigInt(BTC_WATCH_OWNER_LOCK_KEY),
    "btc_watch_owner",
);

pub const UNRECONCILED_READER_SOURCES: &str = "SELECT EXISTS(SELECT 1 FROM via_transactions t WHERE t.bitcoin_payload IS NULL
                OR NOT EXISTS(SELECT 1 FROM via_btc_headers h WHERE h.number=t.bitcoin_height AND h.hash=t.bitcoin_hash))
             OR EXISTS(SELECT 1 FROM via_votable_transactions p WHERE p.bitcoin_payload IS NULL
                OR NOT EXISTS(SELECT 1 FROM via_btc_headers h WHERE h.id=p.proof_header_id AND h.hash=p.proof_l1_block_hash))
             OR EXISTS(SELECT 1 FROM protocol_versions WHERE executed AND bitcoin_payload IS NULL)";

#[derive(Debug)]
pub struct ViaIndexerDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Verifier>,
}

impl ViaIndexerDal<'_, '_> {
    // Era rollback's child-before-parent transaction; completed/local proof parents are retained.
    // https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/block_reverter/src/lib.rs
    pub async fn delete_reader_suffix(&mut self, height: u32) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.storage.in_transaction(),
            "Bitcoin suffix deletion requires a transaction"
        );
        self.lock_btc_watch().await?;
        self.lock_withdrawal_invalidation().await?;
        via_btc_storage::ReaderDal::new(self.storage)
            .check_reset_schema(
                &[
                    "via_transactions",
                    "protocol_versions",
                    "protocol_patches",
                    "via_votable_transactions",
                    "via_wallets",
                    "via_l1_blocks",
                    "via_btc_configuration",
                    "via_btc_headers",
                ],
                &[
                    "protocol_patches.protocol_patches_minor_fkey",
                    "via_l1_batch_vote_inscription_request.via_l1_batch_vote_inscription_reque_votable_transaction_id_fkey",
                    "via_votes.via_votes_votable_transaction_id_fkey",
                    "via_btc_consumers.via_btc_consumers_header_id_fkey",
                ],
            )
            .await?;
        self.storage
            .via_withdrawal_dal()
            .invalidate_withdrawals_from_l1_height(
                height.checked_add(1).context("Bitcoin suffix height overflow")?,
            )
            .await?;
        for query in [
            "DELETE FROM via_votes v USING via_votable_transactions p WHERE v.votable_transaction_id=p.id
                AND (v.source_l1_block_number>$1 OR v.source_l1_block_number IS NULL)
                AND p.l1_batch_status IS NULL
                AND NOT EXISTS(SELECT 1 FROM via_l1_batch_vote_inscription_request r WHERE r.votable_transaction_id=p.id)",
            "DELETE FROM via_votable_transactions p WHERE (p.proof_l1_block_number>$1 OR p.proof_l1_block_number IS NULL)
                AND p.l1_batch_status IS NULL
                AND NOT EXISTS(SELECT 1 FROM via_votes v WHERE v.votable_transaction_id=p.id)
                AND NOT EXISTS(SELECT 1 FROM via_l1_batch_vote_inscription_request r WHERE r.votable_transaction_id=p.id)
                AND NOT EXISTS(SELECT 1 FROM via_l1_batch_bridge_withdrawals b WHERE b.proof_reveal_tx_id=p.proof_reveal_tx_id)
                AND NOT EXISTS(SELECT 1 FROM via_withdrawal_batches b WHERE b.proof_txid=p.proof_reveal_tx_id)",
            "DELETE FROM protocol_patches p USING protocol_versions v WHERE p.minor=v.id AND NOT v.executed
                AND (p.bitcoin_height>$1 OR p.bitcoin_height IS NULL)
                AND NOT EXISTS(SELECT 1 FROM via_votable_transactions)",
            "DELETE FROM protocol_versions v WHERE NOT v.executed AND (v.bitcoin_height>$1 OR v.bitcoin_height IS NULL)
                AND NOT EXISTS(SELECT 1 FROM protocol_patches p WHERE p.minor=v.id)
                AND NOT EXISTS(SELECT 1 FROM via_votable_transactions)",
            "DELETE FROM via_transactions WHERE COALESCE(bitcoin_height,l1_block_number)>$1
                AND status IS NULL AND l1_batch_number IS NULL",
        ] {
            sqlx::query(query).bind(i64::from(height)).instrument("verifier_bitcoin_delete_suffix")
                .execute(self.storage).await?;
        }
        via_btc_storage::ReaderDal::new(self.storage).delete_common_suffix(height).await
    }


    /// Lets only one verifier scan, header update, initialization or recovery run at a time.
    /// Call it inside the transaction that commits the work, before reading progress or fetching blocks.
    /// Work that can invalidate withdrawals must then call `lock_withdrawal_invalidation` before any wallet lock.
    // Graph-node's `rewind_or_truncate_with_conn` saves block pointer and indexed data in one transaction.
    // Via does same and holds lock while fetching too, because several tasks share one checkpoint:
    // https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs
    pub async fn lock_btc_watch(&mut self) -> DalResult<()> {
        BTC_WATCH_OWNER.acquire(self.storage).await
    }

    /// Blocks withdrawal work and proof or vote writes while this transaction may invalidate withdrawals.
    /// Take the scan lock first, then this lock, then any wallet lock.
    /// In any other order, two operations can wait for each other forever.
    pub async fn lock_withdrawal_invalidation(&mut self) -> DalResult<()> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(WITHDRAWAL_INVALIDATION_LOCK_KEY)
            .instrument("btc_watch_invalidation_gate")
            .execute(&mut *self.storage)
            .await?;
        Ok(())
    }

    pub async fn init_indexer_metadata(&mut self, module: &str, l1_block: u32) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
            via_indexer_metadata (last_indexer_l1_block, module, updated_at)
            VALUES
            ($1, $2, NOW())
            ON CONFLICT DO NOTHING
            "#,
            i64::from(l1_block),
            module,
        )
        .instrument("init_indexer_metadata")
        .report_latency()
        .execute(self.storage)
        .await?;

        Ok(())
    }

    pub async fn update_last_processed_l1_block(
        &mut self,
        module: &str,
        l1_block: u32,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            UPDATE via_indexer_metadata
            SET
                last_indexer_l1_block = $1,
                updated_at = NOW()
            WHERE
                module = $2
            "#,
            i64::from(l1_block),
            module,
        )
        .instrument("update_last_processed_l1_block")
        .report_latency()
        .execute(self.storage)
        .await?;

        Ok(())
    }

    pub async fn get_last_processed_l1_block(&mut self, module: &str) -> DalResult<u64> {
        let record = sqlx::query!(
            r#"
            SELECT
                last_indexer_l1_block
            FROM
                via_indexer_metadata
            WHERE
                module = $1
            "#,
            module,
        )
        .instrument("get_last_processed_l1_block")
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(record.map(|r| r.last_indexer_l1_block as u64).unwrap_or(0))
    }
}
