use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};

use crate::Core;

pub const BTC_WATCH_OWNER_LOCK_NAMESPACE: i32 = 1464095563;
const BTC_WATCH_OWNER_LOCK_RESOURCE: i32 = 0;

#[derive(Debug)]
pub struct ViaIndexerDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Core>,
}

impl ViaIndexerDal<'_, '_> {
    /// Serializes main-node Bitcoin scans, header updates, initialization, and recovery.
    /// Acquire inside the work's transaction, before reading progress, fetching blocks, or taking other locks.
    /// The two-integer namespace is separate from verifier bigint locks and its wallet session namespace.
    // Graph Node commits indexed data and its block pointer under one deployment owner.
    // Via retains separate tasks, so they share this transaction owner through Bitcoin fetches.
    // https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs#L1330-L1368
    pub async fn lock_btc_watch(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.storage.in_transaction(),
            "Bitcoin watch ownership requires a transaction"
        );
        sqlx::query("SELECT pg_advisory_xact_lock($1::integer, $2::integer)")
            .bind(BTC_WATCH_OWNER_LOCK_NAMESPACE)
            .bind(BTC_WATCH_OWNER_LOCK_RESOURCE)
            .instrument("btc_watch_owner")
            .report_latency()
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
