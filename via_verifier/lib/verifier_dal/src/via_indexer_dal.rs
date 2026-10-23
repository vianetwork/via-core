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

// jscpd:ignore-start Legacy metadata and canonical SQL stay under each concrete role’s transaction / source owner.
impl ViaIndexerDal<'_, '_> {
    /// Source writers and final verdict saves use the same order and retain both locks through commit.
    pub async fn lock_source_writer(&mut self) -> anyhow::Result<()> {
        self.lock_btc_watch().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(WITHDRAWAL_INVALIDATION_LOCK_KEY)
            .instrument("bitcoin_source_writer")
            .execute(self.storage)
            .await?;
        Ok(())
    }

    /// Lets only one verifier scan, header update, initialization or recovery run at a time.
    /// Call it inside the transaction that commits the work, before reading progress or fetching blocks.
    /// Work that can invalidate withdrawals must then call `lock_withdrawal_invalidation` before any wallet lock.
    // Graph-node's `rewind_or_truncate_with_conn` saves block pointer and indexed data in one transaction.
    // Via does same and holds lock while fetching too, because several tasks share one checkpoint:
    // https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs
    pub async fn lock_btc_watch(&mut self) -> DalResult<()> {
        if !self.storage.in_transaction() {
            return Err(zksync_db_connection::instrument::Instrumented::new("btc_watch_owner")
                .constraint_error(anyhow::anyhow!(
                    "Bitcoin watch ownership requires a transaction"
                )));
        }
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
        self.lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("init_indexer_metadata")
                .constraint_error(error)
        })?;
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
        self.lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("update_last_processed_l1_block")
                .constraint_error(error)
        })?;
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
// jscpd:ignore-end

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
        tx.via_indexer_dal().lock_btc_watch().await.unwrap();
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
        let mut missing = tx.start_transaction().await.unwrap();
        assert!(missing
            .via_l1_block_dal()
            .insert_l1_block(2, BlockHash::from_byte_array([8; 32]).to_string())
            .await
            .is_err());
        missing.rollback().await.unwrap();
        assert!(tx
            .via_l1_block_dal()
            .insert_l1_block(3, "unproved legacy hash".into())
            .await
            .is_err());
        let mut wrong_height = tx.start_transaction().await.unwrap();
        assert!(sqlx::query("UPDATE via_l1_blocks SET number=4 WHERE number=1")
            .execute(wrong_height.conn())
            .await
            .is_err());
        wrong_height.rollback().await.unwrap();
        assert_eq!(tx.via_l1_block_dal().canonical_header(1).await.unwrap(), Some(header));
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
        let certificate: String = sqlx::query_scalar("INSERT INTO via_btc_migration_certificates(format_version,network,genesis,scope,body) VALUES(1,'regtest',$1,'deposits','{}') RETURNING id::TEXT")
            .bind([0u8;32].as_slice()).fetch_one(tx.conn()).await.unwrap();
        sqlx::query("INSERT INTO via_btc_scan_positions(group_name,height,hash,certificate_id) VALUES('deposits',1,$1,$2::TEXT::UUID)")
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

// jscpd:ignore-start Closed certificate inputs are captured independently under each concrete database owner.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticMigrationInput {
    pub database: String,
    pub database_oid: i64,
    pub system_identifier: String,
    pub schema: Vec<MigrationRevision>,
    pub progress: Vec<MigrationProgress>,
    pub canonical: Vec<MigrationCanonical>,
    pub wallets: Vec<MigrationWallet>,
    pub protocols: Vec<MigrationProtocol>,
    pub deposits: Vec<MigrationDeposit>,
    pub census: MigrationCensus,
    pub legacy_votes: i64,
    pub votes: Vec<MigrationVote>,
    pub proofs: Vec<MigrationProof>,
    pub reader_evidence: i64,
    pub reader_positions: i64,
    pub reader_certificates: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationVote {
    pub id: i64,
    pub proof_id: i64,
    pub batch: i64,
    pub proof: Vec<u8>,
    pub verifier: String,
    pub approved: bool,
    pub source_height: Option<i64>,
    pub source_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationProof {
    pub id: i64,
    pub batch: i64,
    pub batch_hash: Vec<u8>,
    pub parent_hash: Vec<u8>,
    pub proof_blob: String,
    pub proof: Vec<u8>,
    pub batch_blob: String,
    pub batch_txid: String,
    pub da_identifier: String,
    pub finalized: Option<bool>,
    pub status: Option<bool>,
    pub unverified_dev: bool,
    pub source_height: Option<i64>,
    pub source_hash: Option<String>,
    pub proof_height: Option<i64>,
    pub proof_hash: Option<String>,
    pub reader_inclusion: Option<i64>,
    pub completed_inputs: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationRevision {
    pub version: i64,
    pub checksum: Vec<u8>,
    pub success: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationProgress {
    pub module: String,
    pub height: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationCanonical {
    pub height: i64,
    pub hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationWallet {
    pub id: i64,
    pub role: String,
    pub address: String,
    pub tx_hash: String,
    pub baseline: i64,
    pub origin: String,
    pub message_index: i64,
    pub payload: Option<serde_json::Value>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationProtocol {
    pub minor: i32,
    pub patch: i32,
    pub timestamp: Option<i64>,
    pub bootloader: Vec<u8>,
    pub default_account: Vec<u8>,
    pub evm_emulator: Option<Vec<u8>>,
    pub verifier_key: Vec<u8>,
    pub upgrade_tx_hash: Option<Vec<u8>>,
    pub executed: Option<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationCensus {
    pub deposit_operations: i64,
    pub proof_results: i64,
    pub delivery_requests: i64,
    pub delivery_history: i64,
    pub vote_attachments: i64,
    pub withdrawal_batches: i64,
    pub withdrawal_observations: i64,
    pub withdrawal_holds: i64,
    pub withdrawal_effects: i64,
    pub recovery_barriers: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationDeposit {
    pub priority_id: i64,
    pub l1_block_number: i64,
    pub tx_id: Vec<u8>,
    pub receiver: String,
    pub value: i64,
    pub calldata: Vec<u8>,
    pub canonical_tx_hash: Vec<u8>,
    pub status: Option<bool>,
    pub l1_batch_number: Option<i64>,
}

impl MigrationDeposit {
    /// Compare the full deposit representation; legacy execution is separately reconciled.
    /// https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine.zig
    pub fn source_equivalent(&self, tx: &crate::via_transactions_dal::L1ToL2Transaction) -> bool {
        self.priority_id == tx.priority_id
            && self.l1_block_number == tx.l1_block_number
            && self.tx_id == tx.tx_id.as_bytes()
            && self.receiver == tx.receiver
            && self.value == tx.value
            && self.calldata == tx.calldata
            && self.canonical_tx_hash == tx.canonical_tx_hash.as_bytes()
    }
}

impl ViaIndexerDal<'_, '_> {
    /// Only SELECTs: callers can enforce REPEATABLE READ, READ ONLY without allocating IDs or locks.
    /// Like pg_rewind, application must compare the complete inspected identity before any mutation.
    /// https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/bin/pg_rewind/pg_rewind.c
    pub async fn semantic_migration_input(&mut self) -> anyhow::Result<SemanticMigrationInput> {
        use sqlx::Row;
        const LIMIT: i64 = 100_001;
        let identity=sqlx::query("SELECT current_database() AS name,(SELECT oid::BIGINT FROM pg_database WHERE datname=current_database()) AS oid,(SELECT system_identifier::TEXT FROM pg_control_system()) AS system_identifier")
            .fetch_one(self.storage.conn()).await?;
        let schema: Vec<MigrationRevision> = sqlx::query_as(
            "SELECT version,checksum,success FROM _sqlx_migrations ORDER BY version LIMIT $1",
        )
        .bind(LIMIT)
        .fetch_all(self.storage.conn())
        .await?;
        let reader_schema =
            schema.iter().any(|revision| revision.version == 20261023041443 && revision.success);
        let identity_schema =
            schema.iter().any(|revision| revision.version == 20261023030434 && revision.success);
        let progress:Vec<MigrationProgress>=sqlx::query_as("SELECT module,last_indexer_l1_block AS height FROM via_indexer_metadata ORDER BY module LIMIT $1")
            .bind(LIMIT).fetch_all(self.storage.conn()).await?;
        let canonical:Vec<MigrationCanonical>=sqlx::query_as("SELECT number AS height,hash::TEXT AS hash FROM via_l1_blocks ORDER BY number LIMIT $1")
            .bind(LIMIT).fetch_all(self.storage.conn()).await?;
        let wallet_query = if reader_schema {
            "SELECT id,role,address,tx_hash,l1_block_number AS baseline,origin_kind AS origin,message_index,immutable_payload AS payload FROM via_wallets ORDER BY id LIMIT $1"
        } else {
            "SELECT id,role,address,tx_hash,l1_block_number AS baseline,'legacy'::TEXT AS origin,0::BIGINT AS message_index,NULL::JSONB AS payload FROM via_wallets ORDER BY id LIMIT $1"
        };
        let wallets: Vec<MigrationWallet> =
            sqlx::query_as(wallet_query).bind(LIMIT).fetch_all(self.storage.conn()).await?;
        let protocol_query = if reader_schema {
            "SELECT p.id AS minor,pp.patch,NULL::BIGINT AS timestamp,p.bootloader_code_hash AS bootloader,p.default_account_code_hash AS default_account,NULL::BYTEA AS evm_emulator,p.recursion_scheduler_level_vk_hash AS verifier_key,pp.upgrade_tx_hash,pp.executed FROM protocol_versions p JOIN protocol_patches pp ON pp.minor=p.id ORDER BY p.id,pp.patch LIMIT $1"
        } else {
            "SELECT p.id AS minor,pp.patch,NULL::BIGINT AS timestamp,p.bootloader_code_hash AS bootloader,p.default_account_code_hash AS default_account,NULL::BYTEA AS evm_emulator,p.recursion_scheduler_level_vk_hash AS verifier_key,p.upgrade_tx_hash,p.executed FROM protocol_versions p JOIN protocol_patches pp ON pp.minor=p.id ORDER BY p.id,pp.patch LIMIT $1"
        };
        let protocols: Vec<MigrationProtocol> =
            sqlx::query_as(protocol_query).bind(LIMIT).fetch_all(self.storage.conn()).await?;
        let deposits:Vec<MigrationDeposit>=sqlx::query_as("SELECT priority_id,l1_block_number,tx_id,receiver,value,calldata,canonical_tx_hash,status,l1_batch_number FROM via_transactions ORDER BY priority_id,canonical_tx_hash LIMIT $1").bind(LIMIT).fetch_all(self.storage.conn()).await?;
        anyhow::ensure!([schema.len(),progress.len(),canonical.len(),wallets.len(),protocols.len(),deposits.len()].into_iter().all(|count|count<LIMIT as usize),"semantic certificate input exceeds its bounded census; split only at an explicitly proved baseline");
        let census=MigrationCensus {
            deposit_operations: sqlx::query_scalar("SELECT count(*) FROM via_transactions").fetch_one(self.storage.conn()).await?,
            proof_results: sqlx::query_scalar(if reader_schema { "SELECT count(*) FROM via_proof_verdicts" } else { "SELECT count(*) FROM via_votable_transactions" }).fetch_one(self.storage.conn()).await?,
            delivery_requests: sqlx::query_scalar("SELECT count(*) FROM via_btc_inscriptions_request").fetch_one(self.storage.conn()).await?,
            delivery_history: sqlx::query_scalar("SELECT count(*) FROM via_btc_inscriptions_request_history").fetch_one(self.storage.conn()).await?,
            vote_attachments: sqlx::query_scalar("SELECT count(*) FROM via_l1_batch_vote_inscription_request").fetch_one(self.storage.conn()).await?,
            withdrawal_batches: sqlx::query_scalar("SELECT count(*) FROM via_withdrawal_batches").fetch_one(self.storage.conn()).await?,
            withdrawal_observations: sqlx::query_scalar("SELECT count(*) FROM via_withdrawal_observations").fetch_one(self.storage.conn()).await?,
            withdrawal_holds: sqlx::query_scalar("SELECT (SELECT count(*) FROM via_withdrawal_holds)+(SELECT count(*) FROM via_withdrawal_legacy_holds)+(SELECT count(*) FROM via_withdrawal_conflicts)").fetch_one(self.storage.conn()).await?,
            withdrawal_effects: sqlx::query_scalar("SELECT (SELECT count(*) FROM via_withdrawal_attempts)+(SELECT count(*) FROM via_withdrawal_fulfillments)+(SELECT count(*) FROM via_withdrawal_expected)+(SELECT count(*) FROM via_l1_batch_bridge_withdrawals)+(SELECT count(*) FROM via_bridge_withdrawals)+(SELECT count(*) FROM via_withdrawals)").fetch_one(self.storage.conn()).await?,
            recovery_barriers: sqlx::query_scalar("SELECT count(*) FROM via_l1_reorg").fetch_one(self.storage.conn()).await?,
        };
        let legacy_votes = sqlx::query_scalar(if reader_schema {
            "SELECT count(*) FROM via_legacy_votes"
        } else {
            "SELECT count(*) FROM via_votes"
        })
        .fetch_one(self.storage.conn())
        .await?;
        let proof_relation =
            if reader_schema { "via_proof_verdicts" } else { "via_votable_transactions" };
        let vote_relation = if reader_schema { "via_legacy_votes" } else { "via_votes" };
        let proof_query=format!("SELECT id,l1_batch_number AS batch,l1_batch_hash AS batch_hash,prev_l1_batch_hash AS parent_hash,proof_blob_id AS proof_blob,proof_reveal_tx_id AS proof,pubdata_blob_id AS batch_blob,pubdata_reveal_tx_id AS batch_txid,da_identifier,is_finalized AS finalized,l1_batch_status AS status,unverified_dev,source_l1_block_number AS source_height,source_l1_block_hash::TEXT AS source_hash,proof_l1_block_number AS proof_height,proof_l1_block_hash::TEXT AS proof_hash,{} AS reader_inclusion,{} AS completed_inputs FROM {proof_relation} ORDER BY id LIMIT $1",
            if reader_schema { "reader_proof_inclusion_id" } else { "NULL::BIGINT" },
            if reader_schema { "completed_input_identity" } else { "NULL::JSONB" });
        let proofs: Vec<MigrationProof> =
            sqlx::query_as(&proof_query).bind(LIMIT).fetch_all(self.storage.conn()).await?;
        let vote_query=format!("SELECT v.id,v.votable_transaction_id AS proof_id,p.l1_batch_number AS batch,p.proof_reveal_tx_id AS proof,v.verifier_address AS verifier,v.vote AS approved,v.source_l1_block_number AS source_height,v.source_l1_block_hash::TEXT AS source_hash FROM {vote_relation} v JOIN {proof_relation} p ON p.id=v.votable_transaction_id ORDER BY v.id LIMIT $1");
        let votes: Vec<MigrationVote> =
            sqlx::query_as(&vote_query).bind(LIMIT).fetch_all(self.storage.conn()).await?;
        anyhow::ensure!(
            proofs.len() < LIMIT as usize && votes.len() < LIMIT as usize,
            "legacy proof/vote census exceeds its bound"
        );
        let reader_evidence = if reader_schema {
            sqlx::query_scalar("SELECT (SELECT count(*) FROM via_proof_evidence)+(SELECT count(*) FROM via_vote_evidence)+(SELECT count(*) FROM via_deposit_evidence)+(SELECT count(*) FROM via_upgrade_evidence)+(SELECT count(*) FROM via_wallet_inclusions)")
                .fetch_one(self.storage.conn()).await?
        } else {
            0
        };
        let reader_positions = if identity_schema {
            sqlx::query_scalar("SELECT count(*) FROM via_btc_scan_positions")
                .fetch_one(self.storage.conn())
                .await?
        } else {
            0
        };
        let reader_certificates = if identity_schema {
            sqlx::query_scalar("SELECT count(*) FROM via_btc_migration_certificates")
                .fetch_one(self.storage.conn())
                .await?
        } else {
            0
        };
        Ok(SemanticMigrationInput {
            database: identity.try_get("name")?,
            database_oid: identity.try_get("oid")?,
            system_identifier: identity.try_get("system_identifier")?,
            schema,
            progress,
            canonical,
            wallets,
            protocols,
            deposits,
            census,
            legacy_votes,
            votes,
            proofs,
            reader_evidence,
            reader_positions,
            reader_certificates,
        })
    }
}
// jscpd:ignore-end

#[cfg(test)]
mod semantic_migration_tests {
    use super::*;
    use crate::{ConnectionPool, VerifierDal};

    // jscpd:ignore-start Independent schema adapters exercise their own read-only connection and permanent sequences.
    #[tokio::test]
    async fn stopped_inspection_is_read_only_and_captures_absence_changes() {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut connection = pool.connection().await.unwrap();
        let sequence: (i64, bool) =
            sqlx::query_as("SELECT last_value,is_called FROM via_wallets_id_seq")
                .fetch_one(connection.conn())
                .await
                .unwrap();
        let mut snapshot =
            connection.transaction_builder().unwrap().set_readonly().build().await.unwrap();
        let read_only: bool =
            sqlx::query_scalar("SELECT current_setting('transaction_read_only')='on'")
                .fetch_one(snapshot.conn())
                .await
                .unwrap();
        assert!(read_only);
        let before = snapshot.via_indexer_dal().semantic_migration_input().await.unwrap();
        let repeated = snapshot.via_indexer_dal().semantic_migration_input().await.unwrap();
        assert_eq!(before, repeated);
        assert_eq!(
            serde_json::from_value::<SemanticMigrationInput>(
                serde_json::to_value(&before).unwrap()
            )
            .unwrap(),
            before
        );
        let advisory: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks WHERE pid=pg_backend_pid() AND locktype='advisory'",
        )
        .fetch_one(snapshot.conn())
        .await
        .unwrap();
        assert_eq!(advisory, 0);
        snapshot.rollback().await.unwrap();
        let after_sequence: (i64, bool) =
            sqlx::query_as("SELECT last_value,is_called FROM via_wallets_id_seq")
                .fetch_one(connection.conn())
                .await
                .unwrap();
        assert_eq!(after_sequence, sequence);
        let mut tx = connection.start_transaction().await.unwrap();
        tx.via_indexer_dal().init_indexer_metadata("semantic_input_change", 1).await.unwrap();
        tx.commit().await.unwrap();
        let changed = connection.via_indexer_dal().semantic_migration_input().await.unwrap();
        assert_ne!(before, changed);
        assert_eq!(before.canonical, changed.canonical);
        assert!(changed
            .progress
            .iter()
            .any(|position| position.module == "semantic_input_change" && position.height == 1));
    }
    // jscpd:ignore-end
}
