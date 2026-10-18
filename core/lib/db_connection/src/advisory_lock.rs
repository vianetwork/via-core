use crate::{
    connection::{Connection, DbMarker},
    error::DalResult,
    instrument::InstrumentExt,
};

/// PostgreSQL advisory-lock identities in its two independent key spaces.
// PostgreSQL's `pg_advisory_xact_lock_int8` and `_int4` use separate key spaces, so owners can share a database.
// Via preserves that distinction rather than converting either node's owner into the other form.
// https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/backend/utils/adt/lockfuncs.c
#[derive(Debug, Clone, Copy)]
pub enum AdvisoryLockKey {
    BigInt(i64),
    Pair(i32, i32),
}

/// A PostgreSQL advisory lock that lasts until the end of the caller's transaction.
/// Take it before reading the state it protects. The caller decides when to commit, which releases it.
#[derive(Debug, Clone, Copy)]
pub struct TransactionAdvisoryLock {
    key: AdvisoryLockKey,
    query_name: &'static str,
    report_latency: bool,
}

impl TransactionAdvisoryLock {
    pub const fn new(key: AdvisoryLockKey, query_name: &'static str) -> Self {
        Self {
            key,
            query_name,
            report_latency: false,
        }
    }

    pub const fn report_latency(mut self) -> Self {
        self.report_latency = true;
        self
    }

    /// Waits for the lock on `storage`. The caller must pass a transaction.
    /// Outside a transaction, PostgreSQL releases the lock when this statement ends.
    pub async fn acquire<DB: DbMarker>(&self, storage: &mut Connection<'_, DB>) -> DalResult<()> {
        let query = match self.key {
            AdvisoryLockKey::BigInt(key) => {
                sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(key)
            }
            AdvisoryLockKey::Pair(namespace, resource) => {
                sqlx::query("SELECT pg_advisory_xact_lock($1::integer, $2::integer)")
                    .bind(namespace)
                    .bind(resource)
            }
        }
        .instrument(self.query_name);
        let query = if self.report_latency {
            query.report_latency()
        } else {
            query
        };
        query.execute(storage).await?;
        Ok(())
    }

    /// Begins a transaction or savepoint and acquires ownership before returning it to the caller.
    pub async fn begin<'c, 'a, DB: DbMarker>(
        &self,
        storage: &'c mut Connection<'a, DB>,
    ) -> DalResult<Connection<'c, DB>> {
        let mut transaction = storage.start_transaction().await?;
        self.acquire(&mut transaction).await?;
        Ok(transaction)
    }
}
