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

#[derive(Debug)]
pub struct ViaIndexerDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Core>,
}

// jscpd:ignore-start Legacy metadata and canonical SQL stay under each concrete role’s transaction / source owner.
impl ViaIndexerDal<'_, '_> {
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
        self.lock_btc_watch().await.map_err(|error| zksync_db_connection::instrument::Instrumented::new("init_indexer_metadata").constraint_error(error))?;
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
        self.lock_btc_watch().await.map_err(|error| zksync_db_connection::instrument::Instrumented::new("update_last_processed_l1_block").constraint_error(error))?;
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

    use crate::{ConnectionPool, Core, CoreDal};

    #[tokio::test]
    async fn reader_header_replay_preserves_payload_and_bitcoin_byte_order() {
        let pool=ConnectionPool::<Core>::test_pool().await;
        let mut storage=pool.connection().await.unwrap();
        let mut tx=storage.start_transaction().await.unwrap();
        tx.via_indexer_dal().lock_btc_watch().await.unwrap();
        let hash:BlockHash=format!("{:064x}",0x1234).parse().unwrap();
        let header=Header { height:1,block_hash:hash,parent_hash:BlockHash::all_zeros() };
        tx.via_l1_block_dal().retain_header(header).await.unwrap();
        tx.via_l1_block_dal().retain_header(header).await.unwrap();
        let conflicting=Header { parent_hash:BlockHash::from_byte_array([7;32]),..header };
        assert!(tx.via_l1_block_dal().retain_header(conflicting).await.is_err());
        assert_eq!(tx.via_l1_block_dal().retained_header(hash).await.unwrap(),Some(header));
        let bytes:Vec<u8>=sqlx::query_scalar("SELECT hash FROM via_btc_headers WHERE height=1").fetch_one(tx.conn()).await.unwrap();
        assert_eq!(&bytes[..2],&[0x34,0x12]);assert_eq!(&bytes[2..],&[0;30]);
        tx.via_l1_block_dal().insert_l1_block(1,hash.to_string()).await.unwrap();
        assert_eq!(tx.via_l1_block_dal().canonical_header(1).await.unwrap(),Some(header));
        let mut missing=tx.start_transaction().await.unwrap();
        assert!(missing.via_l1_block_dal().insert_l1_block(2,BlockHash::from_byte_array([8;32]).to_string()).await.is_err());
        missing.rollback().await.unwrap();
        assert!(tx.via_l1_block_dal().insert_l1_block(3,"unproved legacy hash".into()).await.is_err());
        let mut wrong_height=tx.start_transaction().await.unwrap();
        assert!(sqlx::query("UPDATE via_l1_blocks SET number=4 WHERE number=1").execute(wrong_height.conn()).await.is_err());
        wrong_height.rollback().await.unwrap();
        assert_eq!(tx.via_l1_block_dal().canonical_header(1).await.unwrap(),Some(header));
        assert_eq!(tx.via_indexer_dal().scan_position(ScanGroup::Configuration).await.unwrap(),None);
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn reader_position_cas_rolls_back_effects_even_when_the_error_is_caught() {
        let pool = ConnectionPool::<Core>::test_pool().await;
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
        tx.via_indexer_dal()
            .init_indexer_metadata("reader_cas_effect", 2)
            .await
            .unwrap();
        tx.via_indexer_dal()
            .compare_and_swap_scan_position(ScanGroup::Deposits, old.into(), next.into())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            storage
                .via_indexer_dal()
                .scan_position(ScanGroup::Deposits)
                .await
                .unwrap(),
            Some(next.into())
        );
        let mut tx = storage.start_transaction().await.unwrap();
        tx.via_indexer_dal()
            .update_last_processed_l1_block("reader_cas_effect", 3)
            .await
            .unwrap();
        let stale = BlockRef {
            height: 2,
            block_hash: old.block_hash,
        };
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
            storage
                .via_indexer_dal()
                .scan_position(ScanGroup::Deposits)
                .await
                .unwrap(),
            Some(next.into())
        );
        let mut tx = storage.start_transaction().await.unwrap();
        tx.via_indexer_dal()
            .update_last_processed_l1_block("reader_cas_effect", 4)
            .await
            .unwrap();
        let unretained = BlockRef {
            height: 3,
            block_hash: BlockHash::from_byte_array([3; 32]),
        };
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
            storage
                .via_indexer_dal()
                .scan_position(ScanGroup::Deposits)
                .await
                .unwrap(),
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
    pub upgrades: Vec<MigrationSystemTransaction>,
    pub census: MigrationCensus,
    pub legacy_votes: i64,
    pub votes: Vec<MigrationVote>,
    pub reader_evidence: i64,
    pub reader_positions: i64,
    pub reader_certificates:i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationVote {
    pub batch: i64,
    pub proof: Vec<u8>,
    pub verifier: String,
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationRevision { pub version:i64, pub checksum:Vec<u8>, pub success:bool }
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationProgress { pub module:String, pub height:i64 }
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationCanonical { pub height:i64, pub hash:String }
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationWallet {
    pub id:i64, pub role:String, pub address:String, pub tx_hash:String, pub baseline:i64,
    pub origin:String, pub message_index:i64, pub payload:Option<serde_json::Value>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationProtocol {
    pub minor:i32, pub patch:i32, pub timestamp:Option<i64>, pub bootloader:Vec<u8>,
    pub default_account:Vec<u8>, pub evm_emulator:Option<Vec<u8>>, pub verifier_key:Vec<u8>,
    pub upgrade_tx_hash:Option<Vec<u8>>, pub executed:Option<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationCensus {
    pub deposit_operations: i64,
    pub upgrade_transactions: i64,
    pub l2_batches: i64,
    pub l2_blocks: i64,
    pub delivery_requests: i64,
    pub delivery_history: i64,
    pub delivery_subjects: i64,
    pub snapshots: i64,
    pub recovery_barriers: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationDeposit {
    pub hash: Vec<u8>,
    pub initiator_address: Vec<u8>,
    pub gas_limit: Option<String>,
    pub max_fee_per_gas: Option<String>,
    pub gas_per_pubdata_limit: Option<String>,
    pub data: serde_json::Value,
    pub priority_op_id: Option<i64>,
    pub full_fee: Option<String>,
    pub layer_2_tip_fee: Option<String>,
    pub contract_address: Option<Vec<u8>>,
    pub l1_block_number: Option<i32>,
    pub value: String,
    pub paymaster: Vec<u8>,
    pub paymaster_input: Vec<u8>,
    pub tx_format: Option<i32>,
    pub l1_tx_mint: Option<String>,
    pub l1_tx_refund_recipient: Option<Vec<u8>>,
    pub signature: Option<Vec<u8>>,
    pub l1_batch_number: Option<i64>,
    pub miniblock_number: Option<i64>,
    pub protected_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MigrationSystemTransaction {
    pub hash: Vec<u8>,
    pub payload: serde_json::Value,
    pub protected_execution: bool,
}

impl MigrationSystemTransaction {
    /// The system transaction owner supplies every immutable field; received_at is local arrival time.
    /// https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine.zig
    pub fn source_equivalent(&self, tx: &zksync_types::protocol_upgrade::ProtocolUpgradeTx) -> bool {
        let bytes = |value: &[u8]| format!("\\x{}",hex::encode(value));
        let common = &tx.common_data;
        self.hash == common.hash().as_bytes() && self.payload == serde_json::json!({
            "initiator":bytes(common.sender.as_bytes()),"gas_limit":common.gas_limit.to_string(),
            "max_fee":common.max_fee_per_gas.to_string(),"pubdata_limit":common.gas_per_pubdata_limit.to_string(),
            "execute":tx.execute,"upgrade_id":common.upgrade_id as i32,
            "contract":tx.execute.contract_address.map(|address| bytes(address.as_bytes())),
            "l1_height":common.eth_block,"value":tx.execute.value.to_string(),
            "paymaster":bytes(zksync_types::Address::zero().as_bytes()),"paymaster_input":bytes(&[]),
            "format":common.tx_format() as i32,"mint":common.to_mint.to_string(),
            "refund":bytes(common.refund_recipient.as_bytes()),"priority_id":Option::<i64>::None,
            "full_fee":Option::<String>::None,"tip_fee":Option::<String>::None,"signature":Option::<String>::None,
        })
    }
}

impl MigrationDeposit {
    /// Compare the insert owner's entire immutable payload, without adopting legacy execution facts.
    /// https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine.zig
    pub fn source_equivalent(&self, tx: &zksync_types::l1::L1Tx, protocol_txid: zksync_types::H256) -> bool {
        let common = &tx.common_data;
        self.hash == tx.hash().as_bytes()
            && self.initiator_address == common.sender.as_bytes()
            && self.gas_limit.as_deref() == Some(common.gas_limit.to_string().as_str())
            && self.max_fee_per_gas.as_deref() == Some(common.max_fee_per_gas.to_string().as_str())
            && self.gas_per_pubdata_limit.as_deref() == Some(common.gas_per_pubdata_limit.to_string().as_str())
            && self.data == serde_json::to_value(&tx.execute).expect("L1 execute serializes")
            && self.priority_op_id == i64::try_from(tx.serial_id().0).ok()
            && self.full_fee.as_deref() == Some(common.full_fee.to_string().as_str())
            && self.layer_2_tip_fee.as_deref() == Some(common.layer_2_tip_fee.to_string().as_str())
            && self.contract_address.as_deref() == Some(tx.execute.contract_address.unwrap_or_default().as_bytes())
            && self.l1_block_number == i32::try_from(tx.eth_block().0).ok()
            && self.value == tx.execute.value.to_string()
            && self.paymaster == zksync_types::Address::zero().as_bytes()
            && self.paymaster_input.is_empty()
            && self.tx_format == Some(common.tx_format() as i32)
            && self.l1_tx_mint.as_deref() == Some(common.to_mint.to_string().as_str())
            && self.l1_tx_refund_recipient.as_deref() == Some(common.refund_recipient.as_bytes())
            && self.signature.as_deref() == Some(protocol_txid.as_bytes())
    }
}

impl ViaIndexerDal<'_, '_> {
    /// Only SELECTs: callers can enforce REPEATABLE READ, READ ONLY without allocating IDs or locks.
    /// Like pg_rewind, application must compare the complete inspected identity before any mutation.
    /// https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/bin/pg_rewind/pg_rewind.c
    pub async fn semantic_migration_input(&mut self) -> anyhow::Result<SemanticMigrationInput> {
        use sqlx::Row;
        const LIMIT:i64=100_001;
        let identity=sqlx::query("SELECT current_database() AS name,(SELECT oid::BIGINT FROM pg_database WHERE datname=current_database()) AS oid,(SELECT system_identifier::TEXT FROM pg_control_system()) AS system_identifier")
            .fetch_one(self.storage.conn()).await?;
        let schema:Vec<MigrationRevision>=sqlx::query_as("SELECT version,checksum,success FROM _sqlx_migrations ORDER BY version LIMIT $1")
            .bind(LIMIT).fetch_all(self.storage.conn()).await?;
        let reader_schema=schema.iter().any(|revision| revision.version==20261023041443 && revision.success);
        let identity_schema=schema.iter().any(|revision| revision.version==20261023030434 && revision.success);
        let progress:Vec<MigrationProgress>=sqlx::query_as("SELECT module,last_indexer_l1_block AS height FROM via_indexer_metadata ORDER BY module LIMIT $1")
            .bind(LIMIT).fetch_all(self.storage.conn()).await?;
        let canonical:Vec<MigrationCanonical>=sqlx::query_as("SELECT number AS height,hash::TEXT AS hash FROM via_l1_blocks ORDER BY number LIMIT $1")
            .bind(LIMIT).fetch_all(self.storage.conn()).await?;
        let wallet_query=if reader_schema {
            "SELECT id,role,address,tx_hash,l1_block_number AS baseline,origin_kind AS origin,message_index,immutable_payload AS payload FROM via_wallets ORDER BY id LIMIT $1"
        } else {
            "SELECT id,role,address,tx_hash,l1_block_number AS baseline,'legacy'::TEXT AS origin,0::BIGINT AS message_index,NULL::JSONB AS payload FROM via_wallets ORDER BY id LIMIT $1"
        };
        let wallets:Vec<MigrationWallet>=sqlx::query_as(wallet_query).bind(LIMIT).fetch_all(self.storage.conn()).await?;
        let protocols:Vec<MigrationProtocol>=sqlx::query_as("SELECT p.id AS minor,pp.patch,p.timestamp,p.bootloader_code_hash AS bootloader,p.default_account_code_hash AS default_account,p.evm_emulator_code_hash AS evm_emulator,pp.snark_wrapper_vk_hash AS verifier_key,p.upgrade_tx_hash,NULL::BOOLEAN AS executed FROM protocol_versions p JOIN protocol_patches pp ON pp.minor=p.id ORDER BY p.id,pp.patch LIMIT $1").bind(LIMIT).fetch_all(self.storage.conn()).await?;
        let deposits:Vec<MigrationDeposit>=sqlx::query_as("SELECT hash,initiator_address,trim_scale(gas_limit)::TEXT AS gas_limit,trim_scale(max_fee_per_gas)::TEXT AS max_fee_per_gas,trim_scale(gas_per_pubdata_limit)::TEXT AS gas_per_pubdata_limit,data,priority_op_id,trim_scale(full_fee)::TEXT AS full_fee,trim_scale(layer_2_tip_fee)::TEXT AS layer_2_tip_fee,contract_address,l1_block_number,trim_scale(value)::TEXT AS value,paymaster,paymaster_input,tx_format,trim_scale(l1_tx_mint)::TEXT AS l1_tx_mint,l1_tx_refund_recipient,signature,l1_batch_number,miniblock_number,(l1_batch_number IS NOT NULL OR miniblock_number IS NOT NULL OR index_in_block IS NOT NULL OR l1_batch_tx_index IS NOT NULL OR error IS NOT NULL OR execution_info<>'{}'::JSONB OR refunded_gas<>0) AS protected_execution FROM transactions WHERE is_priority AND priority_op_id IS NOT NULL ORDER BY priority_op_id,hash LIMIT $1").bind(LIMIT).fetch_all(self.storage.conn()).await?;
        let upgrades:Vec<MigrationSystemTransaction>=sqlx::query_as("SELECT hash,jsonb_build_object('initiator',initiator_address,'gas_limit',trim_scale(gas_limit)::TEXT,'max_fee',trim_scale(max_fee_per_gas)::TEXT,'pubdata_limit',trim_scale(gas_per_pubdata_limit)::TEXT,'execute',data,'upgrade_id',upgrade_id,'contract',contract_address,'l1_height',l1_block_number,'value',trim_scale(value)::TEXT,'paymaster',paymaster,'paymaster_input',paymaster_input,'format',tx_format,'mint',trim_scale(l1_tx_mint)::TEXT,'refund',l1_tx_refund_recipient,'priority_id',priority_op_id,'full_fee',trim_scale(full_fee)::TEXT,'tip_fee',trim_scale(layer_2_tip_fee)::TEXT,'signature',signature) AS payload,(l1_batch_number IS NOT NULL OR miniblock_number IS NOT NULL OR index_in_block IS NOT NULL OR l1_batch_tx_index IS NOT NULL OR error IS NOT NULL OR execution_info<>'{}'::JSONB OR refunded_gas<>0) AS protected_execution FROM transactions WHERE is_priority AND priority_op_id IS NULL ORDER BY hash LIMIT $1").bind(LIMIT).fetch_all(self.storage.conn()).await?;
        anyhow::ensure!([schema.len(),progress.len(),canonical.len(),wallets.len(),protocols.len(),deposits.len(),upgrades.len()].into_iter().all(|count|count<LIMIT as usize),"semantic certificate input exceeds its bounded census; split only at an explicitly proved baseline");
        let census=MigrationCensus {
            deposit_operations: sqlx::query_scalar("SELECT count(*) FROM transactions WHERE is_priority AND priority_op_id IS NOT NULL").fetch_one(self.storage.conn()).await?,
            upgrade_transactions: sqlx::query_scalar("SELECT count(*) FROM transactions WHERE is_priority AND priority_op_id IS NULL").fetch_one(self.storage.conn()).await?,
            l2_batches: sqlx::query_scalar("SELECT count(*) FROM l1_batches WHERE number>0").fetch_one(self.storage.conn()).await?,
            l2_blocks: sqlx::query_scalar("SELECT count(*) FROM miniblocks WHERE number>0").fetch_one(self.storage.conn()).await?,
            delivery_requests: sqlx::query_scalar("SELECT count(*) FROM via_btc_inscriptions_request").fetch_one(self.storage.conn()).await?,
            delivery_history: sqlx::query_scalar("SELECT count(*) FROM via_btc_inscriptions_request_history").fetch_one(self.storage.conn()).await?,
            delivery_subjects: sqlx::query_scalar(if reader_schema { "SELECT count(*) FROM via_batch_delivery_facts" } else { "SELECT count(*) FROM via_l1_batch_inscription_request" }).fetch_one(self.storage.conn()).await?,
            snapshots: sqlx::query_scalar("SELECT count(*) FROM snapshots").fetch_one(self.storage.conn()).await?,
            recovery_barriers: sqlx::query_scalar("SELECT count(*) FROM via_l1_reorg").fetch_one(self.storage.conn()).await?,
        };
        let legacy_votes=sqlx::query_scalar(if reader_schema { "SELECT count(*) FROM via_legacy_votes" } else { "SELECT count(*) FROM via_votes" }).fetch_one(self.storage.conn()).await?;
        let votes:Vec<MigrationVote>=sqlx::query_as(if reader_schema {
            "SELECT l1_batch_number AS batch,proof_reveal_tx_id AS proof,verifier_address AS verifier,vote AS approved FROM via_legacy_votes ORDER BY l1_batch_number,proof_reveal_tx_id,verifier_address LIMIT $1"
        } else {
            "SELECT l1_batch_number AS batch,proof_reveal_tx_id AS proof,verifier_address AS verifier,vote AS approved FROM via_votes ORDER BY l1_batch_number,proof_reveal_tx_id,verifier_address LIMIT $1"
        }).bind(LIMIT).fetch_all(self.storage.conn()).await?;
        anyhow::ensure!(votes.len()<LIMIT as usize,"legacy vote census exceeds its bound");
        let reader_evidence=if reader_schema {
            sqlx::query_scalar("SELECT (SELECT count(*) FROM via_proof_evidence)+(SELECT count(*) FROM via_vote_evidence)+(SELECT count(*) FROM via_deposit_evidence)+(SELECT count(*) FROM via_upgrade_evidence)+(SELECT count(*) FROM via_wallet_inclusions)")
                .fetch_one(self.storage.conn()).await?
        } else { 0 };
        let reader_positions=if identity_schema {
            sqlx::query_scalar("SELECT count(*) FROM via_btc_scan_positions").fetch_one(self.storage.conn()).await?
        } else { 0 };
        let reader_certificates=if identity_schema {
            sqlx::query_scalar("SELECT count(*) FROM via_btc_migration_certificates").fetch_one(self.storage.conn()).await?
        } else { 0 };
        Ok(SemanticMigrationInput {
            database:identity.try_get("name")?,database_oid:identity.try_get("oid")?,system_identifier:identity.try_get("system_identifier")?,
            schema,progress,canonical,wallets,protocols,deposits,upgrades,census,legacy_votes,votes,reader_evidence,reader_positions,reader_certificates,
        })
    }
}
// jscpd:ignore-end

#[cfg(test)]
mod semantic_migration_tests {
    use super::*;
    use crate::{ConnectionPool,CoreDal};

    // jscpd:ignore-start Independent schema adapters exercise their own read-only connection and permanent sequences.
    #[tokio::test]
    async fn stopped_inspection_is_read_only_and_captures_absence_changes() {
        let pool=ConnectionPool::<Core>::test_pool().await;
        let mut connection=pool.connection().await.unwrap();
        let sequence:(i64,bool)=sqlx::query_as("SELECT last_value,is_called FROM via_wallets_id_seq")
            .fetch_one(connection.conn()).await.unwrap();
        let mut snapshot=connection.transaction_builder().unwrap().set_readonly().build().await.unwrap();
        let read_only:bool=sqlx::query_scalar("SELECT current_setting('transaction_read_only')='on'")
            .fetch_one(snapshot.conn()).await.unwrap();
        assert!(read_only);
        let before=snapshot.via_indexer_dal().semantic_migration_input().await.unwrap();
        let repeated=snapshot.via_indexer_dal().semantic_migration_input().await.unwrap();
        assert_eq!(before,repeated);
        assert_eq!(serde_json::from_value::<SemanticMigrationInput>(serde_json::to_value(&before).unwrap()).unwrap(),before);
        let advisory:i64=sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE pid=pg_backend_pid() AND locktype='advisory'")
            .fetch_one(snapshot.conn()).await.unwrap();
        assert_eq!(advisory,0);
        snapshot.rollback().await.unwrap();
        let after_sequence:(i64,bool)=sqlx::query_as("SELECT last_value,is_called FROM via_wallets_id_seq")
            .fetch_one(connection.conn()).await.unwrap();
        assert_eq!(after_sequence,sequence);
        let mut tx=connection.start_transaction().await.unwrap();
        tx.via_indexer_dal().init_indexer_metadata("semantic_input_change",1).await.unwrap();
        tx.commit().await.unwrap();
        let changed=connection.via_indexer_dal().semantic_migration_input().await.unwrap();
        assert_ne!(before,changed);
        assert_eq!(before.canonical,changed.canonical);
        assert!(changed.progress.iter().any(|position|position.module=="semantic_input_change"&&position.height==1));
    }
    // jscpd:ignore-end
}
