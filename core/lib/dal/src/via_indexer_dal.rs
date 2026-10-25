use zksync_db_connection::{
    advisory_lock::{AdvisoryLockKey, TransactionAdvisoryLock},
    connection::Connection,
    error::DalResult,
    instrument::InstrumentExt,
};

use crate::Core;

pub const BTC_WATCH_OWNER_LOCK_NAMESPACE: i32 = 1464095563;
const BTC_WATCH_OWNER_LOCK_RESOURCE: i32 = 0;
pub const BTC_WATCH_OWNER: TransactionAdvisoryLock = TransactionAdvisoryLock::new(
    AdvisoryLockKey::Pair(
        BTC_WATCH_OWNER_LOCK_NAMESPACE,
        BTC_WATCH_OWNER_LOCK_RESOURCE,
    ),
    "btc_watch_owner",
)
.report_latency();

pub const UNRECONCILED_READER_SOURCES: &str =
    "SELECT EXISTS(SELECT 1 FROM transactions t WHERE t.is_priority AND t.upgrade_id IS NULL
                AND (t.bitcoin_payload IS NULL OR NOT EXISTS(SELECT 1 FROM via_btc_headers h
                    WHERE h.number=t.bitcoin_height AND h.hash=t.bitcoin_hash)))";

#[derive(Debug)]
pub struct ViaIndexerDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Core>,
}

impl ViaIndexerDal<'_, '_> {
    // Era rollback's explicit child-before-parent deletion, limited to replayable Bitcoin rows.
    // Local L2, sender and DA parents remain in place.
    // https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/block_reverter/src/lib.rs
    pub async fn delete_reader_suffix(&mut self, height: u32) -> anyhow::Result<()> {
        self.lock_btc_watch().await?;
        via_btc_storage::ReaderDal::new(self.storage)
            .check_reset_schema(
                &[
                    "transactions",
                    "protocol_versions",
                    "protocol_patches",
                    "via_btc_proofs",
                    "via_btc_finalizations",
                    "via_votes",
                    "via_wallets",
                    "via_l1_blocks",
                    "via_btc_configuration",
                    "via_btc_headers",
                ],
                &[
                    "call_traces.call_traces_tx_hash_fkey",
                    "l1_batches.l1_batches_protocol_version_fkey",
                    "miniblocks.miniblocks_protocol_version_fkey",
                    "protocol_patches.protocol_patches_minor_fkey",
                    "via_btc_finalizations.via_btc_finalizations_proof_id_fkey",
                    "via_votes.via_votes_votable_transaction_id_fkey",
                    "via_btc_consumers.via_btc_consumers_header_id_fkey",
                ],
            )
            .await?;
        for query in [
            "DELETE FROM via_btc_finalizations f USING via_btc_proofs p WHERE f.proof_id=p.id AND p.l1_block_number>$1",
            "DELETE FROM via_votes WHERE source_l1_block_number>$1 OR source_l1_block_number IS NULL",
            "DELETE FROM via_btc_proofs p WHERE p.l1_block_number>$1
                AND NOT EXISTS(SELECT 1 FROM via_btc_inscriptions_request r WHERE r.l1_batch_number=p.l1_batch_number)
                AND NOT EXISTS(SELECT 1 FROM via_data_availability d WHERE d.l1_batch_number=p.l1_batch_number)",
            "DELETE FROM protocol_patches p WHERE (p.bitcoin_height>$1 OR p.bitcoin_height IS NULL)
                AND NOT EXISTS(SELECT 1 FROM l1_batches b WHERE b.protocol_version=p.minor)
                AND NOT EXISTS(SELECT 1 FROM miniblocks b WHERE b.protocol_version=p.minor)
                AND NOT EXISTS(SELECT 1 FROM protocol_versions v JOIN transactions t ON t.hash=v.upgrade_tx_hash
                    WHERE v.id=p.minor AND (t.l1_batch_number IS NOT NULL OR t.miniblock_number IS NOT NULL))",
            "DELETE FROM transactions t WHERE t.is_priority AND COALESCE(t.bitcoin_height,t.l1_block_number)>$1
                AND t.l1_batch_number IS NULL AND t.miniblock_number IS NULL
                AND NOT EXISTS(SELECT 1 FROM call_traces c WHERE c.tx_hash=t.hash)",
        ] {
            sqlx::query(query).bind(i64::from(height)).instrument("main_bitcoin_delete_suffix")
                .execute(self.storage).await?;
        }
        via_btc_storage::ReaderDal::new(self.storage)
            .delete_common_suffix(height)
            .await
    }


    /// Lets only one main-node Bitcoin scan, header update, initialization or recovery run at a time.
    /// Call it inside the transaction that commits the work, before reading progress, fetching blocks or taking other locks.
    /// The key is a two-integer pair with its own first number, so it collides neither with the verifier's
    /// bigint keys nor with its two-integer wallet-session keys.
    // Graph Node's `DeploymentStore::rewind_or_truncate_with_conn` saves indexed data and block pointer under one owner.
    // Via retains separate tasks, so they share this transaction owner through Bitcoin fetches.
    // https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs
    pub async fn lock_btc_watch(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.storage.in_transaction(),
            "Bitcoin watch ownership requires a transaction"
        );
        BTC_WATCH_OWNER
            .acquire(self.storage)
            .await
            .map_err(Into::into)
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
