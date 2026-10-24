//! Role adapters own the outer transaction, owner/source locks and dependent row deletion.

use anyhow::{ensure, Context};
use bitcoin::BlockHash;
use via_reorg::chain::{BlockRef, Header};
use zksync_db_connection::{
    connection::{Connection, DbMarker},
    instrument::InstrumentExt,
};

pub const FORMAT_VERSION: i32 = 1;

#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Bitcoin reader head changed during preparation")]
    HeadChanged,
    #[error("Bitcoin reader integrity failure: {0}")]
    Integrity(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredHeader {
    pub id: i64,
    pub header: Header<BlockHash>,
}

struct HeaderRow {
    id: i64,
    number: i64,
    hash: String,
    parent_hash: String,
}
impl TryFrom<HeaderRow> for StoredHeader {
    type Error = anyhow::Error;
    fn try_from(row: HeaderRow) -> anyhow::Result<Self> {
        let id = block_ref(row.number, &row.hash)?;
        Ok(Self {
            id: row.id,
            header: Header {
                height: id.height,
                block_hash: id.block_hash,
                parent_hash: row
                    .parent_hash
                    .parse()
                    .context(StoreError::Integrity("invalid parent hash"))?,
            },
        })
    }
}

fn block_ref(number: i64, hash: &str) -> anyhow::Result<BlockRef<BlockHash>> {
    Ok(BlockRef {
        height: number.try_into().context(StoreError::Integrity("invalid height"))?,
        block_hash: hash.parse().context(StoreError::Integrity("invalid hash"))?,
    })
}

#[derive(Debug, Clone)]
pub struct ReaderState {
    pub format_version: i32,
    pub network: String,
    pub genesis: BlockHash,
    pub bootstrap: BlockRef<BlockHash>,
    pub cutoff: BlockRef<BlockHash>,
    pub confirmed: BlockRef<BlockHash>,
    pub replay_ready: bool,
    pub stopped_reason: Option<String>,
}

pub struct ReaderDal<'c, 'a, DB: DbMarker> {
    storage: &'c mut Connection<'a, DB>,
}
impl<'c, 'a, DB: DbMarker> ReaderDal<'c, 'a, DB> {
    pub fn new(storage: &'c mut Connection<'a, DB>) -> Self {
        Self { storage }
    }

    /// check_reset_schema rejects reset when any foreign key into reset tables is missing from known list.
    /// Unknown dependent table then blocks deletion instead of cascading or failing mid-reset.
    /// Idea only from Midnight migration compatibility checks, which refuse unknown schema state: https://github.com/midnightntwrk/midnight-indexer/blob/8b6ea73ea310d60519b3deb17b9f4d1c2342d9c9/indexer-common/src/infra/migrations/postgres.rs
    pub async fn check_reset_schema(
        &mut self,
        tables: &[&str],
        known: &[&str],
    ) -> anyhow::Result<()> {
        ensure!(self.storage.in_transaction(), StoreError::Integrity("reset outside transaction"));
        let unknown = sqlx::query_scalar::<_, String>(
            "SELECT conrelid::regclass::TEXT||'.'||conname FROM pg_constraint WHERE contype='f'
             AND confrelid IN (SELECT unnest($1::TEXT[])::regclass)
             AND NOT (conrelid::regclass::TEXT||'.'||conname)=ANY($2::TEXT[]) LIMIT 1",
        )
        .bind(tables)
        .bind(known)
        .instrument("bitcoin_reader_reset_schema")
        .fetch_optional(self.storage)
        .await?;
        ensure!(
            unknown.is_none(),
            "Bitcoin reset has an unknown dependent constraint: {unknown:?}"
        );
        Ok(())
    }

    /// History without a reconciled origin stays in place and cannot establish readiness.
    pub async fn delete_common_suffix(&mut self, height: u32) -> anyhow::Result<()> {
        ensure!(self.storage.in_transaction(), StoreError::Integrity("delete outside transaction"));
        for query in [
            "DELETE FROM via_btc_configuration WHERE l1_block_number>$1",
            "DELETE FROM via_wallets WHERE l1_block_number>$1 AND block_hash IS NOT NULL",
            "DELETE FROM via_l1_blocks WHERE number>$1",
        ] {
            sqlx::query(query)
                .bind(i64::from(height))
                .instrument("bitcoin_reader_delete_common")
                .execute(self.storage)
                .await?;
        }
        Ok(())
    }

    /// complete_consumer advances one named consumer to header only from previous header or bootstrap.
    /// Gap or stale header changes no row, so consumer never skips block or passes reader head.
    /// Idea only from PostgreSQL replication slots, where each consumer acknowledges its own confirmed position: https://github.com/postgres/postgres/blob/0019a1ecf4c7615a2570d21eeb4b32ae3b371f18/src/backend/replication/slot.c
    pub async fn complete_consumer(
        &mut self,
        name: &str,
        header: &StoredHeader,
    ) -> anyhow::Result<()> {
        ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("consumer outside transaction")
        );
        let result = sqlx::query(
            "INSERT INTO via_btc_consumers (name,header_id)
             SELECT $1,h.id FROM via_btc_headers h WHERE h.id=$2 AND h.number=$3 AND h.hash=$4
             AND ((h.number=(SELECT bootstrap_number FROM via_btc_reader)
                   AND NOT EXISTS(SELECT 1 FROM via_btc_consumers WHERE name=$1))
                  OR EXISTS(SELECT 1 FROM via_btc_consumers c JOIN via_btc_headers previous ON previous.id=c.header_id
                      WHERE c.name=$1 AND (previous.number=h.number-1 OR previous.id=h.id)))
             ON CONFLICT(name) DO UPDATE SET header_id=excluded.header_id",
        ).bind(name).bind(header.id).bind(i64::from(header.header.height))
            .bind(header.header.block_hash.to_string()).instrument("bitcoin_reader_consumer")
            .execute(self.storage).await?;
        ensure!(result.rows_affected() == 1, StoreError::HeadChanged);
        Ok(())
    }

    /// complete_replay flips replay_ready once cutoff and frontier headers exist,
    /// every required consumer reached cutoff and no configuration before cutoff is undecided.
    /// Role checks retained sources first, and flag is durable, so restart never repeats catch-up checks.
    /// Idea only from PostgreSQL table synchronization, which moves SYNCDONE to READY only when apply reaches sync position: https://github.com/postgres/postgres/blob/0019a1ecf4c7615a2570d21eeb4b32ae3b371f18/src/backend/replication/logical/tablesync.c
    pub async fn complete_replay(
        &mut self,
        consumers: &[&str],
        unreconciled_query: &str,
    ) -> anyhow::Result<bool> {
        ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("readiness outside transaction")
        );
        let unknown = sqlx::query_scalar::<_, bool>(unreconciled_query)
            .instrument("bitcoin_reader_retained_reconciliation")
            .fetch_one(self.storage)
            .await?;
        if unknown {
            return Ok(false);
        }
        let result = sqlx::query(
            "UPDATE via_btc_reader r SET replay_ready=TRUE WHERE stopped_reason IS NULL
             AND EXISTS(SELECT 1 FROM via_btc_headers h WHERE h.number=r.replay_cutoff_number AND h.hash=r.replay_cutoff_hash)
             AND EXISTS(SELECT 1 FROM via_btc_headers h WHERE h.number=r.confirmed_number AND h.hash=r.confirmed_hash)
             AND NOT EXISTS(SELECT 1 FROM unnest($1::TEXT[]) required(name)
                 WHERE NOT EXISTS(SELECT 1 FROM via_btc_consumers c JOIN via_btc_headers h ON h.id=c.header_id
                     WHERE c.name=required.name AND h.number>=r.replay_cutoff_number))
             AND NOT EXISTS(SELECT 1 FROM via_btc_configuration WHERE l1_block_number<=r.replay_cutoff_number AND accepted IS NULL)",
        ).bind(consumers).instrument("bitcoin_reader_complete_replay").execute(self.storage).await?;
        Ok(result.rows_affected() == 1)
    }

    /// save_block appends header only while expected head is still highest and parent matches.
    /// Concurrent append or reorg then changes nothing and returns HeadChanged, so role aborts its transaction.
    /// Idea only from Midnight save_block, which writes block and children in caller's outer transaction: https://github.com/midnightntwrk/midnight-indexer/blob/8b6ea73ea310d60519b3deb17b9f4d1c2342d9c9/chain-indexer/src/infra/storage.rs
    pub async fn save_block(
        &mut self,
        block: Header<BlockHash>,
        expected: &StoredHeader,
    ) -> anyhow::Result<i64> {
        ensure!(self.storage.in_transaction(), StoreError::Integrity("append outside transaction"));
        let hash = block.block_hash.to_string();
        let parent = block.parent_hash.to_string();
        let row = sqlx::query!(
            "INSERT INTO via_btc_headers (number, hash, parent_hash)
             SELECT $1::BIGINT, $2, $3 FROM via_btc_headers
             WHERE id=$4 AND number=$1::BIGINT-1 AND hash=$3
               AND id=(SELECT id FROM via_btc_headers ORDER BY number DESC LIMIT 1)
             RETURNING id",
            i64::from(block.height),
            hash,
            parent,
            expected.id,
        )
        .instrument("bitcoin_reader_save_block")
        .fetch_optional(self.storage)
        .await?;
        row.map(|row| row.id).context(StoreError::HeadChanged)
    }

    /// get_highest_block returns stored head by highest number, decoding hashes fallibly.
    /// Corrupt row then surfaces as integrity error instead of panic.
    /// Adapted from Midnight get_highest_block, Bitcoin fields replace ledger fields: https://github.com/midnightntwrk/midnight-indexer/blob/8b6ea73ea310d60519b3deb17b9f4d1c2342d9c9/chain-indexer/src/infra/storage.rs
    pub async fn get_highest_block(&mut self) -> anyhow::Result<Option<StoredHeader>> {
        sqlx::query_as!(HeaderRow,
            "SELECT id, number, hash, parent_hash FROM via_btc_headers ORDER BY number DESC LIMIT 1"
        )
        .instrument("bitcoin_reader_highest_block")
        .fetch_optional(self.storage)
        .await?
        .map(TryInto::try_into)
        .transpose()
    }

    pub async fn header_at(&mut self, height: u32) -> anyhow::Result<Option<StoredHeader>> {
        sqlx::query_as!(
            HeaderRow,
            "SELECT id, number, hash, parent_hash FROM via_btc_headers WHERE number=$1",
            i64::from(height)
        )
        .instrument("bitcoin_reader_header_at")
        .fetch_optional(self.storage)
        .await?
        .map(TryInto::try_into)
        .transpose()
    }

    pub async fn state(&mut self) -> anyhow::Result<Option<ReaderState>> {
        let Some(row) = sqlx::query!("SELECT * FROM via_btc_reader WHERE singleton")
            .instrument("bitcoin_reader_state")
            .fetch_optional(self.storage)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(ReaderState {
            format_version: row.format_version,
            network: row.bitcoin_network,
            genesis: row.genesis_hash.parse().context(StoreError::Integrity("invalid genesis"))?,
            bootstrap: block_ref(row.bootstrap_number, &row.bootstrap_hash)?,
            cutoff: block_ref(row.replay_cutoff_number, &row.replay_cutoff_hash)?,
            confirmed: block_ref(row.confirmed_number, &row.confirmed_hash)?,
            replay_ready: row.replay_ready,
            stopped_reason: row.stopped_reason,
        }))
    }

    /// initialize returns existing state after format, network, genesis and bootstrap checks, else anchors new state at bootstrap. The caller resets its allowlisted rows in this transaction, only when state is absent.
    /// Mismatch fails startup instead of silently reading another chain or format.
    /// Idea only from Sui indexer store, which inserts-or-returns watermark and checks chain id on start: https://github.com/MystenLabs/sui/blob/6de525b6b70277c845f4f5751a9e87fc7130dd48/crates/sui-pg-db/src/store.rs
    pub async fn initialize(
        &mut self,
        network: &str,
        genesis: BlockHash,
        bootstrap: Header<BlockHash>,
        cutoff: BlockRef<BlockHash>,
    ) -> anyhow::Result<ReaderState> {
        ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("initialize outside transaction")
        );
        if let Some(state) = self.state().await? {
            ensure!(
                state.format_version == FORMAT_VERSION,
                StoreError::Integrity("unsupported format")
            );
            ensure!(
                state.network == network
                    && state.genesis == genesis
                    && state.bootstrap == bootstrap.into(),
                StoreError::Integrity("network or bootstrap changed")
            );
            return Ok(state);
        }
        let hash = bootstrap.block_hash.to_string();
        let parent = bootstrap.parent_hash.to_string();
        sqlx::query!(
            "INSERT INTO via_btc_headers (number, hash, parent_hash) VALUES ($1,$2,$3)",
            i64::from(bootstrap.height),
            hash,
            parent,
        )
        .instrument("bitcoin_reader_bootstrap")
        .execute(self.storage)
        .await?;
        sqlx::query!(
            "INSERT INTO via_btc_reader (singleton, format_version, bitcoin_network, genesis_hash,
            bootstrap_number, bootstrap_hash, replay_cutoff_number, replay_cutoff_hash,
            confirmed_number, confirmed_hash) VALUES (TRUE,$1,$2,$3,$4,$5,$6,$7,$4,$5)",
            FORMAT_VERSION,
            network,
            genesis.to_string(),
            i64::from(bootstrap.height),
            hash,
            i64::from(cutoff.height),
            cutoff.block_hash.to_string(),
        )
        .instrument("bitcoin_reader_initialize")
        .execute(self.storage)
        .await?;
        self.state().await?.context(StoreError::Integrity("missing initialized state"))
    }

    /// rewind_headers moves consumers back to ancestor, reconciles unfinished replay cutoff, then deletes headers above ancestor.
    /// Expected head, stopped flag and confirmed frontier are checked first, and role deletes dependent rows earlier in same transaction, so rewind across confirmed history fails as whole.
    /// Adapted from Hyperlane near-head observe and delete_suffix, SQLx replaces SeaORM and leases are dropped: https://github.com/hyperlane-xyz/hyperlane-monorepo/blob/693d2d45df75135504e424c5a4d16761d0016f99/rust/main/agents/scraper/src/near_head/store.rs
    pub async fn rewind_headers(
        &mut self,
        expected: &StoredHeader,
        ancestor: &StoredHeader,
        selected: BlockRef<BlockHash>,
    ) -> anyhow::Result<()> {
        ensure!(self.storage.in_transaction(), StoreError::Integrity("rewind outside transaction"));
        ensure!(
            self.get_highest_block().await?.as_ref() == Some(expected),
            StoreError::HeadChanged
        );
        let state = self.state().await?.context(StoreError::Integrity("missing reader state"))?;
        ensure!(
            state.stopped_reason.is_none()
                && ancestor.header.height >= state.confirmed.height
                && (ancestor.header.height != state.confirmed.height
                    || ancestor.header.block_hash == state.confirmed.block_hash),
            StoreError::Integrity("rewind crosses stopped or confirmed history")
        );
        ensure!(
            self.header_at(ancestor.header.height).await?.as_ref() == Some(ancestor),
            StoreError::Integrity("missing common ancestor")
        );
        sqlx::query!(
            "UPDATE via_btc_consumers c SET header_id=$1 FROM via_btc_headers h
            WHERE c.header_id=h.id AND h.number>$2",
            ancestor.id,
            i64::from(ancestor.header.height)
        )
        .instrument("bitcoin_reader_rewind_consumers")
        .execute(self.storage)
        .await?;
        sqlx::query!(
            "UPDATE via_btc_reader SET replay_cutoff_number=$1, replay_cutoff_hash=$2
            WHERE NOT replay_ready AND replay_cutoff_number>$3",
            i64::from(selected.height),
            selected.block_hash.to_string(),
            i64::from(ancestor.header.height)
        )
        .instrument("bitcoin_reader_reconcile_cutoff")
        .execute(self.storage)
        .await?;
        sqlx::query!(
            "DELETE FROM via_btc_headers WHERE number>$1",
            i64::from(ancestor.header.height)
        )
        .instrument("bitcoin_reader_rewind_headers")
        .execute(self.storage)
        .await?;
        Ok(())
    }

    /// advance_frontier moves confirmed boundary forward only, to header that exists and sits under current head.
    /// Reader computes N-inclusive boundary from checked Core evidence before SQL, so frontier never names unverified block.
    /// Adapted from Hyperlane near-head confirm, without leases or publications: https://github.com/hyperlane-xyz/hyperlane-monorepo/blob/693d2d45df75135504e424c5a4d16761d0016f99/rust/main/agents/scraper/src/near_head/store.rs
    pub async fn advance_frontier(
        &mut self,
        expected: &StoredHeader,
        boundary: &StoredHeader,
    ) -> anyhow::Result<()> {
        ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("frontier outside transaction")
        );
        let result = sqlx::query!(
            "UPDATE via_btc_reader SET confirmed_number=$1, confirmed_hash=$2
            WHERE stopped_reason IS NULL AND confirmed_number<=$1
              AND (confirmed_number<>$1 OR confirmed_hash=$2)
              AND $1<=(SELECT number FROM via_btc_headers WHERE id=$3)
              AND $3=(SELECT id FROM via_btc_headers ORDER BY number DESC LIMIT 1)
              AND EXISTS(SELECT 1 FROM via_btc_headers WHERE id=$4 AND number=$1 AND hash=$2)",
            i64::from(boundary.header.height),
            boundary.header.block_hash.to_string(),
            expected.id,
            boundary.id
        )
        .instrument("bitcoin_reader_confirm")
        .execute(self.storage)
        .await?;
        ensure!(result.rows_affected() == 1, StoreError::HeadChanged);
        Ok(())
    }

    /// stop records first failure reason durably and clears replay_ready.
    /// Later reasons never overwrite first, so operator sees original cause after restarts.
    /// Idea only from Hyperlane near-head pause, which persists sticky halted flag that later cycles refuse: https://github.com/hyperlane-xyz/hyperlane-monorepo/blob/693d2d45df75135504e424c5a4d16761d0016f99/rust/main/agents/scraper/src/near_head/store.rs
    pub async fn stop(&mut self, reason: &str) -> anyhow::Result<()> {
        ensure!(self.storage.in_transaction(), StoreError::Integrity("stop outside transaction"));
        sqlx::query!("UPDATE via_btc_reader SET stopped_reason=COALESCE(stopped_reason,$1), replay_ready=FALSE", reason)
            .instrument("bitcoin_reader_stop").execute(self.storage).await?;
        Ok(())
    }
}
