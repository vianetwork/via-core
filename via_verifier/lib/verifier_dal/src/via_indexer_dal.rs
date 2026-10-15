use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};

use crate::{Verifier, BTC_WATCH_OWNER_LOCK_KEY, WITHDRAWAL_INVALIDATION_LOCK_KEY};

#[derive(Debug)]
pub struct ViaIndexerDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Verifier>,
}

impl ViaIndexerDal<'_, '_> {
    /// Serializes verifier scans, header updates, initialization, and recovery.
    /// Call inside the outer transaction, before reading progress or fetching blocks.
    /// Acquire the invalidation gate before wallet locks if the operation can invalidate authority.
    // Via adapts graph-node's `rewind_or_truncate_with_conn` pointer-plus-effects transaction.
    // Via also serializes fetching across separate tasks under one database owner.
    // https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs
    pub async fn lock_btc_watch(&mut self) -> DalResult<()> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(BTC_WATCH_OWNER_LOCK_KEY)
            .instrument("btc_watch_owner")
            .execute(&mut *self.storage)
            .await?;
        Ok(())
    }

    /// Excludes withdrawal and source writers before a scan can invalidate their authority.
    /// Call after the scan owner and before any withdrawal wallet lock to prevent lock upgrades.
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
