use std::{
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};

use bitcoin::{
    absolute,
    hashes::Hash,
    secp256k1::{Keypair, Secp256k1, SecretKey},
    transaction, Address, Amount, BlockHash, CompressedPublicKey, Network, OutPoint, ScriptBuf,
    Transaction, TxIn, TxOut, Txid,
};
use serde_json::{json, Value};
use tokio::sync::{oneshot, watch};
use tokio::time::timeout;
use via_btc_client::{client::BitcoinClient, indexer::BitcoinInscriptionIndexer, types::NodeAuth};
use via_main_node_reorg_detector::ViaMainNodeReorgDetector;
use via_test_utils::{bitcoin_rpc::BitcoinRpcServer, chain::Chain as TestChain};
use zksync_config::{
    configs::{via_btc_client::ViaBtcClientConfig, via_reorg_detector::ViaReorgDetectorConfig},
    ViaBtcWatchConfig,
};
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_types::{
    protocol_version::ProtocolSemanticVersion,
    via_wallet::{SystemWallets, SystemWalletsDetails},
    ProtocolVersion, ProtocolVersionId, H256,
};

use crate::BtcWatch;

type FetchBarrier = (&'static str, Value, oneshot::Sender<()>, mpsc::Receiver<()>);

#[derive(Default)]
struct Chain {
    model: TestChain,
    requests: Vec<Value>,
    barrier: Option<FetchBarrier>,
}

struct FetchPause {
    entered: oneshot::Receiver<()>,
    release: mpsc::Sender<()>,
}

impl Drop for FetchPause {
    fn drop(&mut self) {
        let _ = self.release.send(());
    }
}

struct BitcoinFixture {
    chain: Arc<Mutex<Chain>>,
    server: BitcoinRpcServer,
    wallets: SystemWallets,
}

fn keypair() -> Keypair {
    Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&[1; 32]).unwrap())
}

fn transaction(outputs: Vec<TxOut>) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn::default()],
        output: outputs,
    }
}

fn push(bytes: &[u8]) -> bitcoin::script::PushBytesBuf {
    bitcoin::script::PushBytesBuf::try_from(bytes.to_vec()).unwrap()
}

fn inscription(kind: &[u8], fields: Vec<Vec<u8>>) -> Transaction {
    via_test_utils::inscription::parser_transaction(keypair(), kind, &fields, 0)
}

fn packed_version(minor: ProtocolVersionId) -> Vec<u8> {
    let version = ProtocolSemanticVersion { minor, ..ProtocolSemanticVersion::default() };
    let mut bytes = vec![0; 32];
    version.pack().to_big_endian(&mut bytes);
    bytes
}

impl BitcoinFixture {
    fn new() -> anyhow::Result<Self> {
        let chain = Arc::new(Mutex::new(Chain::default()));
        let server = {
            let chain = chain.clone();
            BitcoinRpcServer::new_with_errors(
                Some(Duration::from_secs(10)),
                true,
                move |actor, mut request| {
                    request["actor"] = json!(actor);
                    let (result, barrier) = {
                        let mut chain = chain.lock().unwrap();
                        chain.requests.push(request.clone());
                        let result = chain.model.rpc(&request);
                        let matches =
                            chain.barrier.as_ref().is_some_and(|(method, argument, _, _)| {
                                request["method"] == *method && request["params"][0] == *argument
                            });
                        let barrier = if matches { chain.barrier.take() } else { None };
                        (result, barrier)
                    };
                    if let Some((_, _, entered, release)) = barrier {
                        let _ = entered.send(());
                        release.recv_timeout(Duration::from_secs(45)).unwrap();
                    }
                    result
                },
            )?
        };
        let key = keypair();
        let sender = Address::p2wpkh(&CompressedPublicKey(key.public_key()), Network::Regtest);
        let (internal, _) = key.x_only_public_key();
        let wallets = SystemWallets {
            sequencer: sender.clone(),
            governance: sender.clone(),
            verifiers: vec![sender],
            bridge: Address::p2tr(&Secp256k1::new(), internal, None, Network::Regtest),
        };
        Ok(Self { chain, server, wallets })
    }

    fn client(&self, actor: &str) -> Arc<BitcoinClient> {
        Arc::new(
            BitcoinClient::new(
                &self.server.url(actor),
                NodeAuth::None,
                ViaBtcClientConfig::for_tests(),
            )
            .unwrap(),
        )
    }

    fn block(&self, height: u32, fork: u32, txdata: Vec<Transaction>) -> BlockHash {
        let mut chain = self.chain.lock().unwrap();
        let mut block = bitcoin::blockdata::constants::genesis_block(Network::Regtest);
        block.header.nonce = height + fork;
        block.txdata = txdata;
        chain.model.insert(height, block)
    }

    fn deposit(&self) -> Transaction {
        transaction(vec![
            TxOut { value: Amount::from_sat(50_000), script_pubkey: self.wallets.bridge.script_pubkey() },
            TxOut { value: Amount::ZERO, script_pubkey: ScriptBuf::new_op_return(push(&[7; 20])) },
        ])
    }

    fn upgrade(&self) -> Transaction {
        let proposal = inscription(
            b"SystemContractUpgradeProposal",
            vec![packed_version(ProtocolVersionId::Version28), vec![2; 32], vec![3; 32], vec![4; 32]],
        );
        let funding = transaction(vec![TxOut {
            value: Amount::from_sat(100_000),
            script_pubkey: self.wallets.governance.script_pubkey(),
        }]);
        let mut payload = b"VIA_PROTOCOL:UPGRADE".to_vec();
        payload.push(0);
        payload.extend(proposal.compute_txid().to_byte_array());
        let mut approval = transaction(vec![
            TxOut { value: Amount::from_sat(90_000), script_pubkey: self.wallets.governance.script_pubkey() },
            TxOut { value: Amount::ZERO, script_pubkey: ScriptBuf::new_op_return(push(&payload)) },
        ]);
        approval.input[0].previous_output = OutPoint { txid: funding.compute_txid(), vout: 0 };
        let mut chain = self.chain.lock().unwrap();
        for tx in [proposal, funding] {
            chain.model.transactions.insert(tx.compute_txid().to_string(), tx);
        }
        approval
    }

    fn governance_update(&self) -> (Transaction, Address) {
        let key = Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&[2; 32]).unwrap());
        let address = Address::p2wpkh(&CompressedPublicKey(key.public_key()), Network::Regtest);
        let funding = transaction(vec![TxOut {
            value: Amount::from_sat(100_000),
            script_pubkey: self.wallets.governance.script_pubkey(),
        }]);
        let mut payload = b"VIA_PROTOCOL:GOV".to_vec();
        payload.push(0);
        payload.extend(address.to_string().as_bytes());
        let mut approval = transaction(vec![
            TxOut { value: Amount::from_sat(90_000), script_pubkey: self.wallets.governance.script_pubkey() },
            TxOut { value: Amount::ZERO, script_pubkey: ScriptBuf::new_op_return(push(&payload)) },
        ]);
        approval.input[0].previous_output = OutPoint { txid: funding.compute_txid(), vout: 0 };
        self.chain
            .lock()
            .unwrap()
            .model
            .transactions
            .insert(funding.compute_txid().to_string(), funding);
        (approval, address)
    }

    fn bootstrap_config(&self) -> zksync_config::configs::via_consensus::ViaGenesisConfig {
        let address = self.wallets.sequencer.to_string().into_bytes();
        let tx = inscription(
            b"SystemBootstrappingMessage",
            vec![
                1_u32.to_be_bytes().to_vec(),
                packed_version(ProtocolVersionId::Version27),
                vec![1; 32],
                vec![2; 32],
                vec![3; 32],
                vec![4; 32],
                address.clone(),
                address.clone(),
                self.wallets.bridge.to_string().into_bytes(),
                address,
            ],
        );
        let txid = tx.compute_txid();
        self.chain.lock().unwrap().model.transactions.insert(txid.to_string(), tx);
        zksync_config::configs::via_consensus::ViaGenesisConfig { bootstrap_txids: vec![txid.to_string()] }
    }

    fn pause(&self, hash: BlockHash) -> FetchPause {
        self.pause_request("getblock", json!(hash))
    }

    fn pause_request(&self, method: &'static str, argument: Value) -> FetchPause {
        let (entered, receiver) = oneshot::channel();
        let (release, blocked) = mpsc::channel();
        self.chain.lock().unwrap().barrier = Some((method, argument, entered, blocked));
        FetchPause { entered: receiver, release }
    }

    async fn seed(&self, pool: &ConnectionPool<Core>) -> anyhow::Result<()> {
        let mut storage = pool.connection().await?;
        let mut tx = storage.start_transaction().await?;
        tx.via_indexer_dal().lock_btc_watch().await?;
        tx.via_indexer_dal().init_indexer_metadata("via_btc_watch", 1).await?;
        tx.via_wallet_dal()
            .insert_bootstrap_wallets(&SystemWalletsDetails::try_from(self.wallets.clone())?, 0)
            .await?;
        let version = ProtocolVersion {
            version: ProtocolSemanticVersion {
                minor: ProtocolVersionId::Version27,
                ..ProtocolSemanticVersion::default()
            },
            ..ProtocolVersion::default()
        };
        tx.protocol_versions_dal().save_protocol_version_with_tx(&version).await?;
        tx.protocol_versions_dal()
            .mark_via_bootstrap_version(
                version.version,
                version.base_system_contracts_hashes,
                version.l1_verifier_config,
            )
            .await?;
        let canonical = self.chain.lock().unwrap().model.headers().collect::<Vec<_>>();
        for header in canonical {
            tx.via_l1_block_dal().accept_header(header).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn reader(&self, pool: ConnectionPool<Core>) -> anyhow::Result<BtcWatch> {
        let client = self.client("reader");
        BtcWatch::new(
            ViaBtcWatchConfig::for_tests(),
            BitcoinInscriptionIndexer::new(client.clone(), Arc::new(self.wallets.clone())),
            client,
            pool,
            true,
        )
        .await
    }

    fn detector(&self, pool: ConnectionPool<Core>) -> ViaMainNodeReorgDetector {
        ViaMainNodeReorgDetector::new(
            ViaReorgDetectorConfig { poll_interval_ms: 60_000 },
            pool,
            self.client("detector"),
            true,
        )
    }

    async fn detect_until(&self, pool: &ConnectionPool<Core>, query: &str) -> anyhow::Result<()> {
        let (stop, receiver) = watch::channel(false);
        let detector = tokio::spawn(self.detector(pool.clone()).run(receiver));
        let result = wait_until(pool, query).await;
        stop.send(true)?;
        detector.await??;
        result
    }

    async fn sync_headers(&self, pool: &ConnectionPool<Core>) -> anyhow::Result<()> {
        let hash = self.chain.lock().unwrap().model.blocks[&3].block_hash();
        let native =
            hash.to_byte_array().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        self.detect_until(pool, &format!("SELECT EXISTS(SELECT 1 FROM via_l1_blocks WHERE number=3 AND hash=decode('{native}','hex'))"))
            .await
    }
}

async fn wait_until(pool: &ConnectionPool<Core>, query: &str) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut storage = pool.connection().await?;
        while !sqlx::query_scalar::<_, bool>(query).fetch_one(storage.conn()).await? {
            tokio::task::yield_now().await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

async fn assert_deposit(storage: &mut Connection<'_, Core>, txid: Txid, expected_progress: u64) -> anyhow::Result<()> {
    let mut normalized = txid.to_byte_array();
    normalized.reverse();
    let rows: Vec<(i32, String)> =
        sqlx::query_as("SELECT l1_block_number, l1_tx_mint::text FROM transactions WHERE signature=$1")
            .bind(normalized.as_slice())
            .fetch_all(storage.conn())
            .await?;
    assert_eq!(rows, vec![(2, "500000000000000".into())]);
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, expected_progress);
    Ok(())
}

const OWNER_WAITER: &str = "SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=1464095563::oid AND objid=0 AND objsubid=2 AND NOT granted AND database=(SELECT oid FROM pg_database WHERE datname=current_database()))";
const SOFT_RECOVERED: &str = "SELECT NOT EXISTS(SELECT 1 FROM via_l1_reorg) AND EXISTS(SELECT 1 FROM via_indexer_metadata WHERE module='via_btc_watch' AND last_indexer_l1_block=1) AND NOT EXISTS(SELECT 1 FROM via_l1_blocks WHERE number>1)";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scan_finishes_before_soft_recovery_and_replacement_deposit_is_imported(
) -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    fixture.block(1, 0, vec![]);
    let old = fixture.block(2, 0, vec![]);
    fixture.block(3, 0, vec![]);
    let pool = ConnectionPool::<Core>::test_pool().await;
    fixture.seed(&pool).await?;
    let mut reader = fixture.reader(pool.clone()).await?;
    let mut pause = fixture.pause(old);
    let scan_pool = pool.clone();
    let scan = tokio::spawn(async move {
        let result=reader.loop_iteration(&mut scan_pool.connection().await?).await;
        Ok::<_, anyhow::Error>((reader,result))
    });
    tokio::time::timeout(Duration::from_secs(10), &mut pause.entered).await??;
    let deposit = fixture.deposit();
    fixture.block(2, 100, vec![deposit.clone()]);
    fixture.block(3, 100, vec![]);
    let (stop, receiver) = watch::channel(false);
    let detector = tokio::spawn(fixture.detector(pool.clone()).run(receiver));
    wait_until(&pool, OWNER_WAITER).await?;
    assert_eq!(pool.connection().await?.via_l1_block_dal().has_reorg_in_progress().await?, None);
    drop(pause);
    let (mut reader, result) = scan.await??;
    assert!(result.unwrap_err().to_string().contains("not canonical at requested height"));
    wait_until(&pool, SOFT_RECOVERED).await?;
    stop.send(true)?;
    detector.await??;
    fixture.sync_headers(&pool).await?;
    let mut storage = pool.connection().await?;
    reader.loop_iteration(&mut storage).await?;
    assert_deposit(&mut storage, deposit.compute_txid(), 3).await?;
    reader.loop_iteration(&mut storage).await?;
    assert_deposit(&mut storage, deposit.compute_txid(), 3).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn detection_owns_unchanged_progress_before_reader_fetches() -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    fixture.block(1, 0, vec![]);
    fixture.block(2, 0, vec![]);
    fixture.block(3, 0, vec![]);
    let pool = ConnectionPool::<Core>::test_pool().await;
    fixture.seed(&pool).await?;
    let deposit = fixture.deposit();
    fixture.block(2, 100, vec![deposit.clone()]);
    fixture.block(3, 100, vec![]);
    let mut pause = fixture.pause_request("getblockhash", json!(2));
    let (stop, receiver) = watch::channel(false);
    let detector = tokio::spawn(fixture.detector(pool.clone()).run(receiver));
    let wait_timeout = Duration::from_secs(10);
    timeout(wait_timeout, &mut pause.entered).await.expect("block hash barrier timed out")?;
    let requests =
        fixture.chain.lock().unwrap().requests.iter().filter(|request| request["actor"] == "/reader").count();
    let mut reader = fixture.reader(pool.clone()).await?;
    let scan_pool = pool.clone();
    let scan = tokio::spawn(async move {
        reader.loop_iteration(&mut scan_pool.connection().await?).await?;
        Ok::<_, anyhow::Error>(reader)
    });
    wait_until(&pool, OWNER_WAITER).await?;
    assert_eq!(
        fixture.chain.lock().unwrap().requests.iter().filter(|request| request["actor"] == "/reader").count(),
        requests
    );
    drop(pause);
    wait_until(&pool, SOFT_RECOVERED).await?;
    stop.send(true)?;
    timeout(wait_timeout, detector).await.expect("detector shutdown timed out")??;
    let mut reader = timeout(wait_timeout, scan).await.expect("reader scan timed out")??;
    timeout(wait_timeout, fixture.sync_headers(&pool)).await.expect("header sync timed out")?;
    let mut storage = pool.connection().await?;
    reader.loop_iteration(&mut storage).await?;
    assert_deposit(&mut storage, deposit.compute_txid(), 3).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn committed_soft_marker_blocks_scans_and_resumes_after_restart() -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    fixture.block(1, 0, vec![]);
    fixture.block(2, 0, vec![]);
    fixture.block(3, 0, vec![]);
    let pool = ConnectionPool::<Core>::test_pool().await;
    fixture.seed(&pool).await?;
    let mut reader = fixture.reader(pool.clone()).await?;
    let mut storage = pool.connection().await?;
    reader.loop_iteration(&mut storage).await?;
    let deposit = fixture.deposit();
    fixture.block(2, 100, vec![deposit.clone()]);
    fixture.block(3, 100, vec![]);
    sqlx::raw_sql("CREATE FUNCTION interrupt_soft_recovery() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'soft recovery interrupted'; END $$; CREATE TRIGGER interrupt_soft_recovery BEFORE DELETE ON via_l1_reorg FOR EACH ROW EXECUTE FUNCTION interrupt_soft_recovery();")
        .execute(storage.conn()).await?;
    fixture
        .detect_until(&pool, "SELECT EXISTS(SELECT 1 FROM via_l1_reorg WHERE l1_block_number=2 AND l1_batch_number=0)")
        .await?;
    let requests = fixture.chain.lock().unwrap().requests.len();
    reader.loop_iteration(&mut storage).await?;
    assert_eq!(fixture.chain.lock().unwrap().requests.len(), requests);
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 3);
    sqlx::raw_sql("DROP TRIGGER interrupt_soft_recovery ON via_l1_reorg; DROP FUNCTION interrupt_soft_recovery();")
        .execute(storage.conn())
        .await?;
    fixture.detect_until(&pool, SOFT_RECOVERED).await?;
    fixture.sync_headers(&pool).await?;
    reader.loop_iteration(&mut storage).await?;
    assert_deposit(&mut storage, deposit.compute_txid(), 3).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hard_detection_commits_pending_state_and_blocks_reader() -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    fixture.block(1, 0, vec![]);
    fixture.block(2, 0, vec![fixture.deposit()]);
    fixture.block(3, 0, vec![]);
    let pool = ConnectionPool::<Core>::test_pool().await;
    fixture.seed(&pool).await?;
    let mut reader = fixture.reader(pool.clone()).await?;
    let mut storage = pool.connection().await?;
    reader.loop_iteration(&mut storage).await?;
    sqlx::query("UPDATE transactions SET l1_batch_number=3 WHERE is_priority").execute(storage.conn()).await?;
    fixture.block(2, 100, vec![]);
    fixture.block(3, 100, vec![]);
    fixture
        .detect_until(&pool, "SELECT EXISTS(SELECT 1 FROM via_l1_reorg WHERE l1_block_number=2 AND l1_batch_number=3)")
        .await?;
    let requests = fixture.chain.lock().unwrap().requests.len();
    reader.loop_iteration(&mut storage).await?;
    assert_eq!(fixture.chain.lock().unwrap().requests.len(), requests);
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 3);
    assert_eq!(storage.via_l1_block_dal().has_reorg_in_progress().await?, Some((2, 3)));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_commit_rolls_back_deposit_wallet_and_upgrade_then_same_reader_retries(
) -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    let deposit = fixture.deposit();
    let upgrade = fixture.upgrade();
    let (wallet_update, new_governance) = fixture.governance_update();
    fixture.block(1, 0, vec![]);
    fixture.block(2, 0, vec![deposit.clone(), wallet_update, upgrade]);
    fixture.block(3, 0, vec![]);
    let pool = ConnectionPool::<Core>::test_pool().await;
    fixture.seed(&pool).await?;
    let mut reader = fixture.reader(pool.clone()).await?;
    let mut storage = pool.connection().await?;
    sqlx::raw_sql("CREATE FUNCTION reject_watch_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'watch commit rejected'; END $$; CREATE CONSTRAINT TRIGGER reject_watch_commit AFTER UPDATE ON via_indexer_metadata DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_watch_commit();")
        .execute(storage.conn()).await?;
    let error = reader.loop_iteration(&mut storage).await.unwrap_err();
    assert!(error.to_string().contains("watch commit rejected"), "{error}");
    let mut observer = pool.connection().await?;
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM transactions), (SELECT count(*) FROM via_wallets WHERE l1_block_number=2)",
    )
    .fetch_one(observer.conn())
    .await?;
    assert_eq!(counts, (0, 0));
    assert_eq!(
        observer.protocol_versions_dal().latest_semantic_version().await?.unwrap().minor,
        ProtocolVersionId::Version27
    );
    assert_eq!(observer.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 1);
    sqlx::raw_sql("DROP TRIGGER reject_watch_commit ON via_indexer_metadata; DROP FUNCTION reject_watch_commit();")
        .execute(observer.conn())
        .await?;
    reader.loop_iteration(&mut storage).await?;
    assert_deposit(&mut observer, deposit.compute_txid(), 3).await?;
    reader.loop_iteration(&mut storage).await?;
    assert_deposit(&mut observer, deposit.compute_txid(), 3).await?;
    assert_eq!(
        observer.protocol_versions_dal().latest_semantic_version().await?.unwrap().minor,
        ProtocolVersionId::Version28
    );
    let upgrade_hash: Option<Vec<u8>> = sqlx::query_scalar("SELECT upgrade_tx_hash FROM protocol_versions WHERE id=28")
        .fetch_one(observer.conn())
        .await?;
    let system_count: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE hash=$1")
        .bind(upgrade_hash.unwrap())
        .fetch_one(observer.conn())
        .await?;
    assert_eq!(system_count, 1);
    let wallets = observer.via_wallet_dal().get_system_wallets_raw(3).await?.unwrap();
    assert_eq!(SystemWallets::try_from(wallets)?.governance, new_governance);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn initialization_seed_and_configured_restart_wait_for_owner() -> anyhow::Result<()> {
    use via_node_storage_init::ViaMainNodeStorageInitializer;
    let fixture = BitcoinFixture::new()?;
    fixture.block(1, 0, vec![]);
    let second = fixture.block(2, 0, vec![]);
    fixture.block(3, 0, vec![]);
    let pool = ConnectionPool::<Core>::test_pool().await;
    let bootstrap = fixture.bootstrap_config();
    let mut storage = pool.connection().await?;
    storage
        .protocol_versions_dal()
        .save_protocol_version_with_tx(&ProtocolVersion {
            version: ProtocolSemanticVersion {
                minor: ProtocolVersionId::Version27,
                ..ProtocolSemanticVersion::default()
            },
            base_system_contracts_hashes: zksync_contracts::BaseSystemContractsHashes {
                bootloader: H256::repeat_byte(1),
                default_aa: H256::repeat_byte(2),
                evm_emulator: Some(H256::repeat_byte(4)),
            },
            l1_verifier_config: zksync_types::protocol_version::L1VerifierConfig {
                snark_wrapper_vk_hash: H256::repeat_byte(3),
            },
            ..ProtocolVersion::default()
        })
        .await?;
    assert!(storage.via_indexer_dal().lock_btc_watch().await.is_err());
    let mut owner = storage.start_transaction().await?;
    owner.via_indexer_dal().lock_btc_watch().await?;
    let mut other = pool.connection().await?;
    let mut verifier = other.start_transaction().await?;
    for query in [
        "SELECT pg_try_advisory_xact_lock(1464095562::bigint)",
        "SELECT pg_try_advisory_xact_lock(1464095559::bigint)",
        "SELECT pg_try_advisory_xact_lock(1464095561,0)",
    ] {
        assert!(sqlx::query_scalar::<_, bool>(query).fetch_one(verifier.conn()).await?);
    }
    verifier.commit().await?;
    let init = tokio::spawn(ViaMainNodeStorageInitializer::new(
        pool.clone(),
        fixture.client("initializer"),
        bootstrap.clone(),
        ViaBtcWatchConfig::for_tests(),
    ));
    wait_until(&pool, OWNER_WAITER).await?;
    assert_eq!(fixture.chain.lock().unwrap().requests.len(), 0);
    owner.rollback().await?;
    init.await??;
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 1);
    let wallets = storage.via_wallet_dal().get_system_wallets_raw(1).await?.unwrap();
    assert_eq!(SystemWallets::try_from(wallets)?, fixture.wallets);
    let mut owner = storage.start_transaction().await?;
    owner.via_indexer_dal().lock_btc_watch().await?;
    let (stop, receiver) = watch::channel(false);
    let detector = tokio::spawn(fixture.detector(pool.clone()).run(receiver));
    wait_until(&pool, OWNER_WAITER).await?;
    assert_eq!(other.via_l1_block_dal().get_last_l1_block().await?, None);
    assert!(!fixture.chain.lock().unwrap().requests.iter().any(|request| request["actor"] == "/detector"));
    owner.rollback().await?;
    wait_until(&pool, "SELECT EXISTS(SELECT 1 FROM via_l1_blocks WHERE number=3)").await?;
    stop.send(true)?;
    detector.await??;
    let first_hash = fixture.chain.lock().unwrap().model.blocks[&1].block_hash();
    assert_eq!(storage.via_l1_block_dal().get_l1_block_hash(1).await?, Some(first_hash.to_string()));
    let mut reader = fixture.reader(pool.clone()).await?;
    let mut pause = fixture.pause(second);
    let scan_pool = pool.clone();
    let scan = tokio::spawn(async move { reader.loop_iteration(&mut scan_pool.connection().await?).await });
    tokio::time::timeout(Duration::from_secs(10), &mut pause.entered).await??;
    let restart = tokio::spawn(ViaMainNodeStorageInitializer::new(
        pool.clone(),
        fixture.client("initializer"),
        bootstrap,
        ViaBtcWatchConfig { restart_indexing: true, ..ViaBtcWatchConfig::for_tests() },
    ));
    wait_until(&pool, OWNER_WAITER).await?;
    drop(pause);
    scan.await??;
    restart.await??;
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn soft_recovery_preserves_zero_and_missing_progress() -> anyhow::Result<()> {
    for missing in [false, true] {
        let fixture = BitcoinFixture::new()?;
        let first = fixture.block(1, 0, vec![]);
        let boundary = fixture.block(2, 0, vec![]);
        fixture.block(3, 0, vec![]);
        let pool = ConnectionPool::<Core>::test_pool().await;
        fixture.seed(&pool).await?;
        let mut storage = pool.connection().await?;
        if missing {
            sqlx::query("DELETE FROM via_indexer_metadata WHERE module='via_btc_watch'")
                .execute(storage.conn())
                .await?;
        } else {
            let mut tx=storage.start_transaction().await?;
            tx.via_indexer_dal().lock_btc_watch().await?;
            tx.via_indexer_dal().update_last_processed_l1_block("via_btc_watch", 0).await?;
            tx.commit().await?;
        }
        storage.via_l1_block_dal().insert_reorg_metadata(3, 0).await?;
        fixture.detect_until(&pool, "SELECT NOT EXISTS(SELECT 1 FROM via_l1_reorg)").await?;
        let progress: Option<i64> =
            sqlx::query_scalar("SELECT last_indexer_l1_block FROM via_indexer_metadata WHERE module='via_btc_watch'")
                .fetch_optional(storage.conn())
                .await?;
        assert_eq!(progress, if missing { None } else { Some(0) });
        assert_eq!(
            storage.via_l1_block_dal().list_l1_blocks(0, 10).await?,
            vec![(0, bitcoin::blockdata::constants::genesis_block(Network::Regtest).block_hash().to_string()), (1, first.to_string()), (2, boundary.to_string())]
        );
    }
    Ok(())
}
