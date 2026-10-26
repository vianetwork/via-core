//! Role adapters own the outer transaction, owner/source locks and dependent row deletion.

use std::future::Future;

use anyhow::{ensure, Context};
use bitcoin::BlockHash;
use via_reorg::chain::{BlockRef, Header};
use zksync_db_connection::{
    connection::{Connection, DbMarker},
    instrument::InstrumentExt,
};

pub const FORMAT_VERSION: i32 = 1;

/// PROTOCOL_CONSUMER names consumer that completes block h's protocol inputs even while h's rotation is NotReady.
/// Block h+1 then waits for that decision.
pub const PROTOCOL_CONSUMER: &str = "protocol";

#[derive(Clone, Copy)]
pub enum ProofScope {
    Interpretation,
    Finalization,
    Publication(bitcoin::Txid),
}

/// ReaderRole owns schema-specific SQL and returns typed facts to the common reader.
/// Each operation uses the caller's connection, keeping native effects in the same transaction.
/// Adapted from Sui's store traits, role schemas replace store backends: https://github.com/MystenLabs/sui/blob/6de525b6b70277c845f4f5751a9e87fc7130dd48/crates/sui-indexer-alt-framework-store-traits/src/lib.rs
pub trait ReaderRole: DbMarker + Sized {
    const NAME: &'static str;
    const OWNER: zksync_db_connection::advisory_lock::TransactionAdvisoryLock;
    const REQUIRED_CONSUMERS: &'static [&'static str];
    fn authorize_publication(
        storage: &mut Connection<'_, Self>,
        message: &via_btc_client::types::InscriptionMessage,
        voter: &bitcoin::Address,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn save_publication(
        storage: &mut Connection<'_, Self>,
        request_id: i64,
        publication: publication::Publication,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn save_proof(
        storage: &mut Connection<'_, Self>,
        proof: &via_btc_client::indexer::replay::Included<
            via_btc_client::indexer::replay::ProofInput,
        >,
        header_id: i64,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn lock_source(
        storage: &mut Connection<'_, Self>,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn lock_authority_source(
        storage: &mut Connection<'_, Self>,
    ) -> impl Future<Output = anyhow::Result<()>> + Send {
        Self::lock_source(storage)
    }
    fn delete_suffix(
        storage: &mut Connection<'_, Self>,
        height: u32,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn materialize_protocol(
        storage: &mut Connection<'_, Self>,
        change: &StoredConfiguration,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn apply_protocol_block(
        storage: &mut Connection<'_, Self>,
        block: &protocol::ProtocolBlock,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn stamp_legacy_cutoff(
        storage: &mut Connection<'_, Self>,
        cutoff: u32,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn initialize_bootstrap(
        storage: &mut Connection<'_, Self>,
        bootstrap: &via_btc_client::bootstrap::CheckedBootstrap,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn unreconciled_source(
        storage: &mut Connection<'_, Self>,
    ) -> impl Future<Output = anyhow::Result<Option<String>>> + Send;
    /// retained_boundary returns the greatest reconciled source needed by already-kept effects.
    /// Replay protects old work admitted at weaker depth. Higher frontier grants no new maturity.
    /// Idea only from Hyperlane's monotone confirmed frontier, which refuses deletion below protected history: https://github.com/hyperlane-xyz/hyperlane-monorepo/blob/693d2d45df75135504e424c5a4d16761d0016f99/rust/main/agents/scraper/src/near_head/store.rs
    fn retained_boundary(
        storage: &mut Connection<'_, Self>,
    ) -> impl Future<Output = anyhow::Result<Option<u32>>> + Send;
    fn retained_proof_txids(
        storage: &mut Connection<'_, Self>,
    ) -> impl Future<Output = anyhow::Result<Vec<bitcoin::Txid>>> + Send;
    fn canonical_proofs(
        storage: &mut Connection<'_, Self>,
        through: u32,
        scope: ProofScope,
    ) -> impl Future<Output = anyhow::Result<Vec<CanonicalProof>>> + Send;
    fn finalize_proof(
        storage: &mut Connection<'_, Self>,
        work: &ProofWork,
        approved: bool,
    ) -> impl Future<Output = anyhow::Result<u64>> + Send;
}

pub async fn initialize_reader<DB: ReaderRole>(
    pool: &zksync_db_connection::connection_pool::ConnectionPool<DB>,
    client: std::sync::Arc<via_btc_client::client::BitcoinClient>,
    genesis: zksync_config::configs::via_consensus::ViaGenesisConfig,
) -> anyhow::Result<()> {
    let bootstrap = via_btc_client::bootstrap::ViaBootstrap::new(client.clone(), genesis)
        .checked_reader_bootstrap()
        .await?;
    bootstrap.revalidate(&client).await?;
    let mut connection = pool.connection().await?;
    let mut tx = DB::OWNER.begin(&mut connection).await?;
    if ReaderDal::new(&mut tx).prepare_reset(bootstrap.selected.height).await? {
        DB::delete_suffix(&mut tx, bootstrap.starting.height.saturating_sub(1)).await?;
    }
    ReaderDal::new(&mut tx)
        .initialize_defaults(
            &bootstrap,
            via_btc_client::traits::BitcoinOps::get_network(client.as_ref()),
        )
        .await?;
    tx.commit().await?;
    Ok(())
}

mod configuration;
mod proof;
pub use proof::{CanonicalProof, CanonicalVote, ProofQuorum, ProofWork};
pub mod metrics;
pub mod protocol;
pub mod publication;
pub mod transition;
pub use configuration::{StoredConfiguration, WalletSource};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Bitcoin authority held pending canonical mature replay inputs")]
    AuthorityHeld,
    #[error("Bitcoin reader stopped for diagnosis: {0}")]
    Stopped(String),
    #[error("Bitcoin source authority held: {0}")]
    SourceHeld(&'static str),
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

/// ConsumerPosition separates committed effects from the oldest history a consumer still needs.
/// Both positions commit with effects, so a crash cannot release history needed on restart.
/// Idea only from PostgreSQL replication slots, which retain the durable restart position separately from confirmed progress: https://github.com/postgres/postgres/blob/0019a1ecf4c7615a2570d21eeb4b32ae3b371f18/src/backend/replication/slot.c
pub struct ConsumerPosition {
    pub applied: StoredHeader,
    pub oldest_needed: StoredHeader,
}

#[derive(sqlx::FromRow)]
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

    pub async fn prepare_reset(&mut self, cutoff: u32) -> anyhow::Result<bool>
    where
        DB: ReaderRole,
    {
        ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("reset preflight outside transaction")
        );
        let held = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM via_l1_reorg)")
            .instrument("bitcoin_reset_barrier")
            .fetch_one(self.storage)
            .await?;
        ensure!(!held, "Existing Bitcoin recovery barrier; stopped for diagnosis");
        let reset = self.state().await?.is_none();
        if reset {
            self.stamp_legacy_cutoff(cutoff).await?;
        }
        Ok(reset)
    }

    /// Q26 captures only already-finalized rows before the first reset, never a moving replay cutoff.
    pub async fn stamp_legacy_cutoff(&mut self, cutoff: u32) -> anyhow::Result<()>
    where
        DB: ReaderRole,
    {
        ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("legacy cutoff outside transaction")
        );
        ensure!(
            self.state().await?.is_none(),
            StoreError::Integrity("legacy cutoff already established")
        );
        DB::stamp_legacy_cutoff(self.storage, cutoff).await
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
            "DELETE FROM via_votes WHERE source_l1_block_number>$1 OR source_l1_block_number IS NULL",
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
        oldest_needed: &StoredHeader,
    ) -> anyhow::Result<()> {
        ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("consumer outside transaction")
        );
        let result = sqlx::query(
            "INSERT INTO via_btc_consumers (name,header_id,oldest_needed_id)
             SELECT $1,h.id,n.id FROM via_btc_headers h,via_btc_headers n WHERE h.id=$2 AND h.number=$3 AND h.hash=$4
             AND n.id=$5 AND n.number=$6 AND n.hash=$7 AND n.number<=h.number
             AND ((h.number=(SELECT bootstrap_number FROM via_btc_reader)
                   AND NOT EXISTS(SELECT 1 FROM via_btc_consumers WHERE name=$1))
                  OR EXISTS(SELECT 1 FROM via_btc_consumers c JOIN via_btc_headers previous ON previous.id=c.header_id
                      WHERE c.name=$1 AND (previous.number=h.number-1 OR previous.id=h.id)))
             ON CONFLICT(name) DO UPDATE SET header_id=excluded.header_id,oldest_needed_id=excluded.oldest_needed_id",
        ).bind(name).bind(header.id).bind(i64::from(header.header.height))
            .bind(header.header.block_hash.to_string()).bind(oldest_needed.id)
            .bind(i64::from(oldest_needed.header.height)).bind(oldest_needed.header.block_hash.to_string()).instrument("bitcoin_reader_consumer")
            .execute(self.storage).await?;
        ensure!(result.rows_affected() == 1, StoreError::HeadChanged);
        Ok(())
    }

    pub async fn consumer(&mut self, name: &str) -> anyhow::Result<Option<ConsumerPosition>> {
        type Row = (i64, i64, String, String, i64, i64, String, String);
        let row = sqlx::query(
            "SELECT h.id,h.number,h.hash,h.parent_hash,n.id,n.number,n.hash,n.parent_hash FROM via_btc_consumers c
             JOIN via_btc_headers h ON h.id=c.header_id JOIN via_btc_headers n ON n.id=c.oldest_needed_id WHERE c.name=$1",
        )
        .bind(name)
        .try_map(|row: sqlx::postgres::PgRow| <Row as sqlx::FromRow<'_, _>>::from_row(&row))
        .instrument("bitcoin_reader_consumer_position")
        .fetch_optional(self.storage)
        .await?;
        row.map(
            |(
                id,
                number,
                hash,
                parent_hash,
                needed_id,
                needed_number,
                needed_hash,
                needed_parent,
            )| {
                Ok(ConsumerPosition {
                    applied: HeaderRow { id, number, hash, parent_hash }.try_into()?,
                    oldest_needed: HeaderRow {
                        id: needed_id,
                        number: needed_number,
                        hash: needed_hash,
                        parent_hash: needed_parent,
                    }
                    .try_into()?,
                })
            },
        )
        .transpose()
    }

    /// complete_replay flips replay_ready once cutoff and frontier headers exist,
    /// every required consumer reached cutoff and no configuration before cutoff is undecided.
    /// Role checks retained sources first, and flag is durable, so restart never repeats catch-up checks.
    /// Idea only from PostgreSQL table synchronization, which moves SYNCDONE to READY only when apply reaches sync position: https://github.com/postgres/postgres/blob/0019a1ecf4c7615a2570d21eeb4b32ae3b371f18/src/backend/replication/logical/tablesync.c
    pub async fn complete_replay(&mut self) -> anyhow::Result<bool>
    where
        DB: ReaderRole,
    {
        ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("readiness outside transaction")
        );
        if let Some(source) = DB::unreconciled_source(self.storage).await? {
            crate::metrics::METRICS.replay_unreconciled[&DB::NAME].set(1);
            tracing::warn!(role = DB::NAME, %source, "Bitcoin replay waits for retained source reconciliation");
            return Ok(false);
        }
        let unbound = DB::retained_proof_txids(self.storage).await?;
        if let Some(txid) = unbound.first() {
            crate::metrics::METRICS.replay_unreconciled[&DB::NAME].set(1);
            tracing::warn!(%txid, remaining=unbound.len(), "Bitcoin replay waits for retained proof source binding");
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
        ).bind(DB::REQUIRED_CONSUMERS).instrument("bitcoin_reader_complete_replay").execute(self.storage).await?;
        crate::metrics::METRICS.replay_unreconciled[&DB::NAME].set(0);
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
            "UPDATE via_btc_consumers c SET header_id=CASE WHEN h.number>$2 THEN $1 ELSE c.header_id END,
             oldest_needed_id=CASE WHEN n.number>$2 THEN $1 ELSE c.oldest_needed_id END FROM via_btc_headers h,via_btc_headers n
             WHERE c.header_id=h.id AND c.oldest_needed_id=n.id AND (h.number>$2 OR n.number>$2)",
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
