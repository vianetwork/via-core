use std::{sync::Mutex, time::Duration};

use bitcoin::{hashes::Hash, Block, BlockHash};
use serde_json::Value;
use tokio::sync::watch;
use via_btc_client::types::NodeAuth;
use via_test_utils::bitcoin_rpc::BitcoinRpcServer;
use via_verifier_block_reverter::ViaVerifierBlockReverter;
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

use super::*;

static METRIC_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct BitcoinFixture {
    client: Arc<BitcoinClient>,
    blocks: Vec<Block>,
    requests: Arc<Mutex<Vec<Value>>>,
    _server: BitcoinRpcServer,
}

impl BitcoinFixture {
    fn new() -> anyhow::Result<Self> {
        let chain = via_test_utils::chain::Chain::with_height(3);
        let blocks = chain.blocks.range(1..).map(|(_, block)| block.clone()).collect();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server = {
            let requests = requests.clone();
            BitcoinRpcServer::new_with_errors(None, false, move |_, request| {
                requests.lock().unwrap().push(request.clone());
                chain.rpc(&request)
            })?
        };
        let client = Arc::new(BitcoinClient::new(&server.url(""), NodeAuth::None, ViaBtcClientConfig {
            network: "regtest".into(),
            external_apis: vec![],
            fee_strategies: vec![],
            use_rpc_for_fee_rate: Some(true),
        })?);
        Ok(Self { client, blocks, requests, _server: server })
    }

    fn assert_hits(&self, hashes: usize, blocks: usize, heights: usize, headers: usize) {
        let requests = self.requests.lock().unwrap();
        for (method, expected) in [
            ("getblockhash", hashes),
            ("getblock", blocks),
            ("getblockcount", heights),
            ("getblockheader", headers),
        ] {
            assert_eq!(requests.iter().filter(|request| request["method"] == method).count(), expected, "unexpected {method} count");
        }
        assert_eq!(requests.len(), hashes + blocks + heights + headers);
    }

    async fn seed(
        &self,
        storage: &mut Connection<'_, Verifier>,
        reorg: bool,
        hard: bool,
    ) -> anyhow::Result<()> {
        let mut tx = storage.start_transaction().await?;
        tx.via_indexer_dal().lock_source_writer().await?;
        tx.via_indexer_dal().init_indexer_metadata("via_btc_watch", 2).await?;
        tx.via_l1_block_dal()
            .accept_header(via_reorg::chain::Header {
                height: 1,
                block_hash: self.blocks[0].block_hash(),
                parent_hash: self.blocks[0].header.prev_blockhash,
            })
            .await?;
        let tip_hash = if reorg { BlockHash::all_zeros() } else { self.blocks[1].block_hash() };
        tx.via_l1_block_dal()
            .accept_header(via_reorg::chain::Header {
                height: 2,
                block_hash: tip_hash,
                parent_hash: self.blocks[0].block_hash(),
            })
            .await?;
        if hard {
            sqlx::query("INSERT INTO via_transactions(priority_id,tx_id,receiver,value,canonical_tx_hash,l1_block_number,l1_batch_number) VALUES(1,$1,'fixture',1,$1,2,3)")
                .bind(vec![1_u8; 32]).execute(tx.conn()).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

fn metrics() -> (u64, u64, usize, usize) {
    (
        METRICS.soft_reorg.get(),
        METRICS.hard_reorg.get(),
        METRICS.reorg_data[&ReorgInfo::StartBlock].get(),
        METRICS.reorg_data[&ReorgInfo::EndBlock].get(),
    )
}

async fn iterate(detector: &mut ViaVerifierReorgDetector, storage: &mut Connection<'_, Verifier>) -> anyhow::Result<()> {
    // The iteration must finish before a reintroduced 30-second handoff wait could expire.
    tokio::time::timeout(Duration::from_secs(10), detector.loop_iteration(storage)).await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn detected_reorg_commits_before_recovery_without_header_sync() -> anyhow::Result<()> {
    let _guard = METRIC_TEST_LOCK.lock().await;
    for hard in [false, true] {
        let fixture = BitcoinFixture::new()?;
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await?;
        let mut observer = pool.connection().await?;
        fixture.seed(&mut storage, true, hard).await?;
        let before = metrics();
        let mut detector = ViaVerifierReorgDetector::new(ViaReorgDetectorConfig::for_tests(), pool.clone(), fixture.client.clone());
        iterate(&mut detector, &mut storage).await?;
        assert_eq!(observer.via_l1_block_dal().has_reorg_in_progress().await?, Some((2, if hard { 3 } else { 0 })));
        let mut owner = observer.start_transaction().await?;
        owner.via_indexer_dal().lock_btc_watch().await?;
        owner.commit().await?;
        fixture.assert_hits(2, 0, 0,0);
        assert_eq!(metrics(), (before.0 + u64::from(!hard), before.1 + u64::from(hard), 2, 2));

        let (stop, receiver) = watch::channel(false);
        let reverter = tokio::spawn(ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig { poll_interval_ms: 1 }).run(receiver));
        tokio::time::timeout(Duration::from_secs(10), async {
            while observer.via_l1_block_dal().has_reorg_in_progress().await?.is_some() {
                tokio::task::yield_now().await;
            }
            stop.send(true)?;
            reverter.await?
        })
        .await??;
        assert_eq!(observer.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 1);
        assert_eq!(observer.via_l1_block_dal().get_last_l1_block().await?, Some((1, fixture.blocks[0].block_hash().to_string())));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_outer_commit_does_not_publish_reorg_metrics() -> anyhow::Result<()> {
    let _guard = METRIC_TEST_LOCK.lock().await;
    for hard in [false, true] {
        let fixture = BitcoinFixture::new()?;
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await?;
        fixture.seed(&mut storage, true, hard).await?;
        sqlx::raw_sql("CREATE FUNCTION reject_reorg_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reorg commit rejected'; END $$; CREATE CONSTRAINT TRIGGER reject_reorg_commit AFTER INSERT ON via_l1_reorg DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_reorg_commit();")
            .execute(storage.conn()).await?;
        let before = metrics();
        let mut detector = ViaVerifierReorgDetector::new(ViaReorgDetectorConfig::for_tests(), pool.clone(), fixture.client.clone());
        let error = iterate(&mut detector, &mut storage).await.unwrap_err();
        assert!(format!("{error:#}").contains("reorg commit rejected"));
        assert_eq!(metrics(), before);
        let mut observer = pool.connection().await?;
        assert_eq!(observer.via_l1_block_dal().has_reorg_in_progress().await?, None);
        let mut owner = observer.start_transaction().await?;
        owner.via_indexer_dal().lock_btc_watch().await?;
        owner.commit().await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_chain_syncs_and_pending_recovery_stops_polling() -> anyhow::Result<()> {
    let _guard = METRIC_TEST_LOCK.lock().await;
    let fixture = BitcoinFixture::new()?;
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await?;
    fixture.seed(&mut storage, false, false).await?;
    let before = metrics();
    let mut detector = ViaVerifierReorgDetector::new(ViaReorgDetectorConfig::for_tests(), pool, fixture.client.clone());
    iterate(&mut detector, &mut storage).await?;
    assert_eq!(storage.via_l1_block_dal().get_last_l1_block().await?, Some((3, fixture.blocks[2].block_hash().to_string())));
    assert_eq!(metrics(), before);
    fixture.assert_hits(4, 0, 1, 1);
    storage.via_l1_block_dal().insert_reorg_metadata(2, 0).await?;
    let requests = fixture.requests.lock().unwrap().len();
    iterate(&mut detector, &mut storage).await?;
    assert_eq!(fixture.requests.lock().unwrap().len(), requests);
    assert_eq!(storage.via_l1_block_dal().has_reorg_in_progress().await?, Some((2, 0)));
    assert_eq!(metrics(), before);
    Ok(())
}
