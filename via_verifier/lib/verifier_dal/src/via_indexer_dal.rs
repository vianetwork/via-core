use zksync_db_connection::{
    advisory_lock::{AdvisoryLockKey, TransactionAdvisoryLock},
    connection::Connection,
    error::DalResult,
    instrument::InstrumentExt,
};

use crate::{Verifier, BTC_WATCH_OWNER_LOCK_KEY, WITHDRAWAL_INVALIDATION_LOCK_KEY};

pub const BTC_WATCH_OWNER: TransactionAdvisoryLock = TransactionAdvisoryLock::new(
    AdvisoryLockKey::BigInt(BTC_WATCH_OWNER_LOCK_KEY),
    "btc_watch_owner",
);

#[derive(Debug)]
pub struct ViaIndexerDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Verifier>,
}

impl ViaIndexerDal<'_, '_> {
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

// jscpd:ignore-start Independent database adapters retain concrete SQL transaction ownership.
impl ViaIndexerDal<'_, '_> {
    /// An unknown group position is not inferred from legacy scalar metadata.
    pub async fn scan_position(
        &mut self,
        group: via_reorg::reader::ScanGroup,
    ) -> anyhow::Result<Option<via_reorg::chain::BlockRef<bitcoin::BlockHash>>> {
        use bitcoin::hashes::Hash;
        use sqlx::Row;
        let row: Option<(i64, Vec<u8>)> =
            sqlx::query("SELECT height,hash FROM via_btc_scan_positions WHERE group_name=$1")
                .bind(group.as_str())
                .try_map(|row: sqlx::postgres::PgRow| {
                    Ok((row.try_get("height")?, row.try_get("hash")?))
                })
                .instrument("bitcoin_scan_position")
                .fetch_optional(self.storage)
                .await?;
        row.map(|(height, hash)| {
            Ok(via_reorg::chain::BlockRef {
                height: u32::try_from(height)?,
                block_hash: bitcoin::BlockHash::from_slice(&hash)?,
            })
        })
        .transpose()
    }

    /// The SQL exception aborts the effects transaction on a stale height or hash.
    // Graph applies effects and progress together; SQL enforces the compare rather than a later Rust-only check.
    // https://github.com/GraphProtocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs
    pub async fn compare_and_swap_scan_position(
        &mut self,
        group: via_reorg::reader::ScanGroup,
        expected: via_reorg::chain::BlockRef<bitcoin::BlockHash>,
        next: via_reorg::chain::BlockRef<bitcoin::BlockHash>,
    ) -> anyhow::Result<()> {
        use bitcoin::hashes::Hash;
        anyhow::ensure!(
            self.storage.in_transaction(),
            "position changes require an effects transaction"
        );
        sqlx::query("SELECT via_btc_compare_and_swap_position($1,$2,$3,$4,$5)")
            .bind(group.as_str())
            .bind(i64::from(expected.height))
            .bind(expected.block_hash.as_byte_array().as_slice())
            .bind(i64::from(next.height))
            .bind(next.block_hash.as_byte_array().as_slice())
            .instrument("compare_and_swap_bitcoin_position")
            .execute(self.storage)
            .await?;
        Ok(())
    }
}
// jscpd:ignore-end

// jscpd:ignore-start The same observable contract runs against independent Core and Verifier SQL transaction owners.
#[cfg(test)]
mod reader_tests {
    use bitcoin::{hashes::Hash, BlockHash};
    use via_reorg::{
        chain::{BlockRef, Header},
        reader::ScanGroup,
    };
    use crate::{ConnectionPool, Verifier, VerifierDal};

    #[tokio::test]
    async fn reader_header_replay_preserves_payload_and_bitcoin_byte_order() {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        let mut tx = storage.start_transaction().await.unwrap();
        let hash: BlockHash = format!("{:064x}", 0x1234).parse().unwrap();
        let header = Header { height: 1, block_hash: hash, parent_hash: BlockHash::all_zeros() };
        tx.via_l1_block_dal().retain_header(header).await.unwrap();
        tx.via_l1_block_dal().retain_header(header).await.unwrap();
        let conflicting = Header { parent_hash: BlockHash::from_byte_array([7; 32]), ..header };
        assert!(tx.via_l1_block_dal().retain_header(conflicting).await.is_err());
        assert_eq!(tx.via_l1_block_dal().retained_header(hash).await.unwrap(), Some(header));
        let bytes: Vec<u8> = sqlx::query_scalar("SELECT hash FROM via_btc_headers WHERE height=1")
            .fetch_one(tx.conn())
            .await
            .unwrap();
        assert_eq!(&bytes[..2], &[0x34, 0x12]);
        assert_eq!(&bytes[2..], &[0; 30]);
        tx.via_l1_block_dal().insert_l1_block(1, hash.to_string()).await.unwrap();
        assert_eq!(tx.via_l1_block_dal().canonical_header(1).await.unwrap(), Some(header));
        tx.via_l1_block_dal()
            .insert_l1_block(2, BlockHash::from_byte_array([8; 32]).to_string())
            .await
            .unwrap();
        assert_eq!(tx.via_l1_block_dal().canonical_header(2).await.unwrap(), None);
        tx.via_l1_block_dal().insert_l1_block(3, "unproved legacy hash".into()).await.unwrap();
        assert!(tx.via_l1_block_dal().canonical_header(3).await.is_err());
        sqlx::query("UPDATE via_l1_blocks SET number=4 WHERE number=1")
            .execute(tx.conn())
            .await
            .unwrap();
        assert!(tx.via_l1_block_dal().canonical_header(4).await.is_err());
        assert_eq!(
            tx.via_indexer_dal().scan_position(ScanGroup::Configuration).await.unwrap(),
            None
        );
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn reader_position_cas_rolls_back_effects_even_when_the_error_is_caught() {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        let old = Header {
            height: 1,
            block_hash: BlockHash::from_byte_array([1; 32]),
            parent_hash: BlockHash::all_zeros(),
        };
        let next = Header {
            height: 2,
            block_hash: BlockHash::from_byte_array([2; 32]),
            parent_hash: old.block_hash,
        };
        assert!(storage
            .via_indexer_dal()
            .compare_and_swap_scan_position(ScanGroup::Deposits, old.into(), next.into())
            .await
            .is_err());
        let mut tx = storage.start_transaction().await.unwrap();
        for header in [old, next] {
            tx.via_l1_block_dal().retain_header(header).await.unwrap();
        }
        let certificate: i64 = sqlx::query_scalar("INSERT INTO via_btc_migration_certificates(format_version,network,genesis,scope,body) VALUES(1,'regtest',$1,'deposits','{}') RETURNING id")
            .bind([0u8;32].as_slice()).fetch_one(tx.conn()).await.unwrap();
        sqlx::query("INSERT INTO via_btc_scan_positions(group_name,height,hash,certificate_id) VALUES('deposits',1,$1,$2)")
            .bind(old.block_hash.as_byte_array().as_slice()).bind(certificate).execute(tx.conn()).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = storage.start_transaction().await.unwrap();
        tx.via_indexer_dal().init_indexer_metadata("reader_cas_effect", 2).await.unwrap();
        tx.via_indexer_dal()
            .compare_and_swap_scan_position(ScanGroup::Deposits, old.into(), next.into())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            storage.via_indexer_dal().scan_position(ScanGroup::Deposits).await.unwrap(),
            Some(next.into())
        );
        let mut tx = storage.start_transaction().await.unwrap();
        tx.via_indexer_dal().update_last_processed_l1_block("reader_cas_effect", 3).await.unwrap();
        let stale = BlockRef { height: 2, block_hash: old.block_hash };
        assert!(tx
            .via_indexer_dal()
            .compare_and_swap_scan_position(ScanGroup::Deposits, stale, old.into())
            .await
            .is_err());
        let _ = tx.commit().await;
        assert_eq!(
            storage
                .via_indexer_dal()
                .get_last_processed_l1_block("reader_cas_effect")
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            storage.via_indexer_dal().scan_position(ScanGroup::Deposits).await.unwrap(),
            Some(next.into())
        );
        let mut tx = storage.start_transaction().await.unwrap();
        tx.via_indexer_dal().update_last_processed_l1_block("reader_cas_effect", 4).await.unwrap();
        let unretained = BlockRef { height: 3, block_hash: BlockHash::from_byte_array([3; 32]) };
        assert!(tx
            .via_indexer_dal()
            .compare_and_swap_scan_position(ScanGroup::Deposits, next.into(), unretained)
            .await
            .is_err());
        let _ = tx.commit().await;
        assert_eq!(
            storage
                .via_indexer_dal()
                .get_last_processed_l1_block("reader_cas_effect")
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            storage.via_indexer_dal().scan_position(ScanGroup::Deposits).await.unwrap(),
            Some(next.into())
        );
    }
}
// jscpd:ignore-end
