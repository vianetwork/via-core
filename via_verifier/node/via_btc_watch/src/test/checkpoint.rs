use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use bitcoin::{
    absolute,
    block::Header,
    hashes::Hash,
    secp256k1::{Keypair, Secp256k1, SecretKey},
    transaction, Address, Block, BlockHash, CompressedPublicKey, Network, Transaction, TxIn,
    TxMerkleNode,
};
use via_btc_client::{
    client::BitcoinClient,
    indexer::BitcoinInscriptionIndexer,
    types::{InscriptionMessage, L1BatchDAReferenceInput, NodeAuth, ProofDAReferenceInput},
};
use via_test_utils::{bitcoin_rpc::BitcoinRpcServer, chain::Chain};
use via_verifier_block_reverter::ViaVerifierBlockReverter;
use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal, BTC_WATCH_OWNER_LOCK_KEY};
use zksync_config::{
    configs::{via_btc_client::ViaBtcClientConfig, via_reorg_detector::ViaReorgDetectorConfig},
    ViaBtcWatchConfig,
};
use zksync_types::{
    via_wallet::{SystemWallets, SystemWalletsDetails},
    L1BatchNumber, H256,
};

use crate::{message_processors::MessageProcessorError, metrics::METRICS, VerifierBtcWatch};

type FetchBarrier = (tokio::sync::oneshot::Sender<()>, std::sync::mpsc::Receiver<()>);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn withdrawal_scan_advances_and_records_once() -> anyhow::Result<()> {
    let (fixture, pool, mut watcher) = BitcoinFixture::with_withdrawal().await?;
    let mut storage = pool.connection().await?;
    let mut interrupted = storage.start_transaction().await?;
    watcher.loop_iteration(&mut interrupted).await?;
    fixture.assert_withdrawal_state(&mut interrupted, 2, Some(2)).await?;
    interrupted.rollback().await?;
    fixture.assert_withdrawal_state(&mut storage, 1, None).await?;
    let mut restarted = fixture.watcher(pool.clone()).await?;
    restarted.loop_iteration(&mut storage).await?;
    fixture.assert_withdrawal_state(&mut storage, 2, Some(2)).await?;
    let mut tx = storage.start_transaction().await?;
    tx.via_indexer_dal().lock_source_writer().await?;
    tx.via_indexer_dal().update_last_processed_l1_block("via_btc_watch", 1).await?;
    tx.commit().await?;
    let mut restarted = fixture.watcher(pool).await?;
    restarted.loop_iteration(&mut storage).await?;
    fixture.assert_withdrawal_state(&mut storage, 2, Some(2)).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn withdrawal_reorg_retains_holds_and_replays_new_inclusion() -> anyhow::Result<()> {
    let (fixture, pool, mut watcher) = BitcoinFixture::with_withdrawal().await?;
    let txid = fixture.chain.lock().unwrap().blocks[&2].txdata[1].compute_txid();
    let mut storage = pool.connection().await?;
    watcher.loop_iteration(&mut storage).await?;
    recover(&pool, 2).await?;
    fixture.assert_withdrawal_state(&mut storage, 1, Some(2)).await?;
    let wallet = fixture.wallets.bridge.script_pubkey();
    assert!(
        !storage
            .via_withdrawal_dal()
            .observed_withdrawal_transaction_has_canonical_inclusion(
                wallet.as_bytes(),
                txid.as_byte_array(),
            )
            .await?
    );
    let replacement_hash = {
        let mut chain = fixture.chain.lock().unwrap();
        let mut block = chain.blocks[&2].clone();
        block.header.nonce += 1;
        chain.insert(2, block)
    };
    fixture.seed(&mut storage, Some(1)).await?;
    watcher.loop_iteration(&mut storage).await?;
    fixture.assert_withdrawal_state(&mut storage, 2, Some(2)).await?;
    let observations =
        storage.via_withdrawal_dal().pending_withdrawal_observations(wallet.as_bytes()).await?;
    assert_eq!(observations.len(), 1);
    let inclusion = observations[0].inclusion.as_ref().unwrap();
    assert_eq!((inclusion.block_height, inclusion.block_hash), (2, replacement_hash));
    let history: i64 = sqlx::query_scalar("SELECT count(*) FROM via_withdrawal_inclusions")
        .fetch_one(storage.conn())
        .await?;
    assert_eq!(history, 2);
    Ok(())
}

struct BitcoinFixture {
    client: Arc<BitcoinClient>,
    chain: Arc<Mutex<Chain>>,
    requests: Arc<AtomicUsize>,
    fetch_barrier: Arc<Mutex<Option<FetchBarrier>>>,
    _server: BitcoinRpcServer,
    wallets: SystemWallets,
}

fn keypair() -> Keypair {
    Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&[1; 32]).unwrap())
}

fn inscription(message: InscriptionMessage, nonce: u32) -> Transaction {
    let (kind, fields) = match message {
        InscriptionMessage::L1BatchDAReference(batch) => (
            via_btc_client::types::L1_BATCH_DA_REFERENCE_MSG.as_bytes(),
            vec![
                batch.l1_batch_hash.as_bytes().to_vec(),
                batch.l1_batch_index.0.to_be_bytes().to_vec(),
                batch.da_identifier.as_bytes().to_vec(),
                batch.blob_id.as_bytes().to_vec(),
                batch.prev_l1_batch_hash.as_bytes().to_vec(),
            ],
        ),
        InscriptionMessage::ProofDAReference(proof) => (
            via_btc_client::types::PROOF_DA_REFERENCE_MSG.as_bytes(),
            vec![
                proof.l1_batch_reveal_txid.to_byte_array().to_vec(),
                proof.da_identifier.as_bytes().to_vec(),
                proof.blob_id.as_bytes().to_vec(),
            ],
        ),
        _ => unreachable!("checkpoint fixture only produces batch and proof messages"),
    };
    via_test_utils::inscription::parser_transaction(keypair(), kind, &fields, nonce)
}

impl BitcoinFixture {
    async fn assert_withdrawal_state(
        &self,
        storage: &mut Connection<'_, Verifier>,
        position: u64,
        payment_height: Option<u32>,
    ) -> anyhow::Result<()> {
        assert_eq!(
            storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?,
            position
        );
        let expected = payment_height
            .map(|height| {
                let txid = self.chain.lock().unwrap().blocks[&height].txdata[1]
                    .compute_txid()
                    .to_byte_array()
                    .to_vec();
                let wallet = self.wallets.bridge.script_pubkey().into_bytes();
                (
                    vec![(wallet.clone(), txid.clone())],
                    vec![(
                        wallet,
                        format!("{:02x}", height as u8).repeat(10),
                        "observation".to_owned(),
                        txid,
                    )],
                )
            })
            .unwrap_or_default();
        let observations: Vec<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
            "SELECT wallet,txid FROM via_withdrawal_observations ORDER BY wallet,txid",
        )
        .fetch_all(storage.conn())
        .await?;
        let holds: Vec<(Vec<u8>, String, String, Vec<u8>)> =
            sqlx::query_as("SELECT wallet,reference,reason,source FROM via_withdrawal_holds ORDER BY wallet,reference,reason,source")
                .fetch_all(storage.conn()).await?;
        assert_eq!((observations, holds), expected);
        Ok(())
    }

    fn add_withdrawal(&self, height: u32) -> anyhow::Result<bitcoin::Txid> {
        use bitcoin::OutPoint;
        use via_musig2::{
            transaction_builder::TransactionBuilder,
            types::{TransactionBuilderConfig, TransactionOutput},
        };
        use via_test_utils::utils::generate_dummy_outputs;

        let mut outputs = generate_dummy_outputs(1, 10_000);
        outputs[0].script_pubkey = self.wallets.bridge.script_pubkey();
        let parent = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::from_consensus(height),
            input: vec![TxIn::default()],
            output: outputs,
        };
        let payment = TransactionBuilder::new(self.client.clone())?
            .build_fixed_bridge_tx(
                &[(OutPoint::new(parent.compute_txid(), 0), parent.output[0].clone())],
                vec![TransactionOutput {
                    output: parent.output[0].clone(),
                    op_return_data: Some(vec![height as u8; 10]),
                }],
                &TransactionBuilderConfig::withdrawal(self.wallets.bridge.clone()),
                1,
            )?
            .tx;
        let txid = payment.compute_txid();
        self.add_block(height, None);
        let mut chain = self.chain.lock().unwrap();
        let mut block = chain.blocks[&height].clone();
        block.txdata = vec![parent, payment];
        chain.insert(height, block);
        Ok(txid)
    }

    async fn with_withdrawal() -> anyhow::Result<(Self, ConnectionPool<Verifier>, VerifierBtcWatch)>
    {
        let fixture = Self::new()?;
        fixture.add_withdrawal(2)?;
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        fixture.seed(&mut pool.connection().await?, Some(1)).await?;
        let watcher = fixture.watcher(pool.clone()).await?;
        Ok((fixture, pool, watcher))
    }

    fn new() -> anyhow::Result<Self> {
        let chain = Arc::new(Mutex::new(Chain::default()));
        let requests = Arc::new(AtomicUsize::new(0));
        let fetch_barrier = Arc::new(Mutex::new(None::<FetchBarrier>));
        let server = {
            let chain = chain.clone();
            let requests = requests.clone();
            let fetch_barrier = fetch_barrier.clone();
            BitcoinRpcServer::new_with_errors(
                Some(Duration::from_secs(5)),
                false,
                move |_, request| {
                    requests.fetch_add(1, Ordering::SeqCst);
                    if request["method"] == "getblock" {
                        let barrier = fetch_barrier.lock().unwrap().take();
                        if let Some((entered, release)) = barrier {
                            entered.send(()).unwrap();
                            release.recv().unwrap();
                        }
                    }
                    chain.lock().unwrap().rpc(&request)
                },
            )?
        };
        let client = Arc::new(BitcoinClient::new(
            &server.url(""),
            NodeAuth::None,
            ViaBtcClientConfig::for_tests(),
        )?);
        let sender = Address::p2wpkh(&CompressedPublicKey(keypair().public_key()), Network::Regtest);
        let wallets = SystemWallets {
            sequencer: sender.clone(),
            bridge: sender.clone(),
            governance: sender.clone(),
            verifiers: vec![sender],
        };
        Ok(Self { client, chain, requests, fetch_barrier, _server: server, wallets })
    }

    fn add_block(&self, height: u32, proof: Option<(u32, H256, H256)>) -> BlockHash {
        let txdata = proof
            .map(|(number, hash, parent)| {
                let batch = inscription(
                    InscriptionMessage::L1BatchDAReference(L1BatchDAReferenceInput {
                        l1_batch_index: L1BatchNumber(number),
                        l1_batch_hash: hash,
                        prev_l1_batch_hash: parent,
                        da_identifier: "fixture".into(),
                        blob_id: format!("batch-{number}"),
                    }),
                    number,
                );
                let proof = inscription(
                    InscriptionMessage::ProofDAReference(ProofDAReferenceInput {
                        l1_batch_reveal_txid: batch.compute_txid(),
                        da_identifier: "fixture".into(),
                        blob_id: format!("proof-{number}-{hash}"),
                    }),
                    height,
                );
                self.chain.lock().unwrap().transactions.insert(batch.compute_txid().to_string(), batch);
                vec![proof]
            })
            .unwrap_or_default();
        let block = Block {
            header: Header {
                version: Default::default(),
                prev_blockhash: BlockHash::all_zeros(),
                merkle_root: TxMerkleNode::all_zeros(),
                time: height,
                bits: Default::default(),
                nonce: height,
            },
            txdata,
        };
        self.chain.lock().unwrap().insert(height, block)
    }

    fn watcher(
        &self, pool: ConnectionPool<Verifier>,
    ) -> impl std::future::Future<Output = anyhow::Result<VerifierBtcWatch>> {
        let client = self.client.clone();
        let indexer = BitcoinInscriptionIndexer::new(client.clone(), Arc::new(self.wallets.clone()));
        let mut config = ViaBtcWatchConfig::for_tests();
        config.block_confirmations = 0;
        VerifierBtcWatch::new(config, indexer, client, pool)
    }

    async fn with_proof(
    ) -> anyhow::Result<(Self, ConnectionPool<Verifier>, Connection<'static, Verifier>)> {
        let fixture = Self::new()?;
        fixture.add_block(2, Some((1, H256::repeat_byte(1), H256::zero())));
        fixture.add_block(3, None);
        let (pool, storage) = fixture.seeded(Some(1)).await?;
        Ok((fixture, pool, storage))
    }

    async fn seeded(
        &self,
        cursor: Option<u32>,
    ) -> anyhow::Result<(ConnectionPool<Verifier>, Connection<'static, Verifier>)> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await?;
        self.seed(&mut storage, cursor).await?;
        Ok((pool, storage))
    }

    async fn seed(
        &self,
        storage: &mut Connection<'_, Verifier>,
        cursor: Option<u32>,
    ) -> anyhow::Result<()> {
        let mut tx = storage.start_transaction().await?;
        tx.via_indexer_dal().lock_btc_watch().await?;
        tx.via_wallet_dal()
            .insert_bootstrap_wallets(&SystemWalletsDetails::try_from(self.wallets.clone())?, 0)
            .await?;
        if let Some(cursor) = cursor {
            tx.via_indexer_dal().init_indexer_metadata("via_btc_watch", cursor).await?;
        }
        let blocks = self.chain.lock().unwrap().headers().collect::<Vec<_>>();
        for header in blocks {
            tx.via_l1_block_dal().accept_header(header).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

async fn rewind_headers(storage: &mut Connection<'_, Verifier>, height: i64) -> anyhow::Result<()> {
    let mut tx = storage.start_transaction().await?;
    tx.via_indexer_dal().lock_source_writer().await?;
    tx.via_l1_block_dal().delete_l1_blocks(height).await?;
    tx.commit().await?;
    Ok(())
}

async fn assert_reader_state(
    storage: &mut Connection<'_, Verifier>,
    progress: u64,
    tip: Option<(u32, Vec<u8>)>,
) -> anyhow::Result<()> {
    assert_eq!(
        storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?,
        progress
    );
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, tip);
    Ok(())
}

async fn recover(pool: &ConnectionPool<Verifier>, divergence: i64) -> anyhow::Result<()> {
    pool.connection().await?.via_l1_block_dal().insert_reorg_metadata(divergence, 0).await?;
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let runner = tokio::spawn(
        ViaVerifierBlockReverter::new(pool.clone(), ViaReorgDetectorConfig { poll_interval_ms: 10 }).run(receiver),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if pool.connection().await.unwrap().via_l1_block_dal().has_reorg_in_progress().await.unwrap().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    stop.send(true)?;
    runner.await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_above_lagging_cursor_imports_the_previously_skipped_proof() -> anyhow::Result<()>
{
    let fixture = BitcoinFixture::new()?;
    fixture.add_block(156450, None);
    fixture.add_block(156451, Some((266, H256::from_low_u64_be(266), H256::from_low_u64_be(265))));
    fixture.add_block(156456, None);
    let (pool, mut storage) = fixture.seeded(Some(156449)).await?;
    let mut tx = storage.start_transaction().await?;
    tx.via_indexer_dal().lock_source_writer().await?;
    let header = via_reorg::chain::Header {
        height: 156449,
        block_hash: BlockHash::from_byte_array([49; 32]),
        parent_hash: BlockHash::from_byte_array([48; 32]),
    };
    via_test_utils::reader_db::header(tx.conn(), header, true).await?;
    let (_, members) = tx.via_wallet_dal().verifier_set_before(156449).await?;
    let set = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM via_wallets WHERE role='Verifier' AND address=$1",
    )
    .bind(members.join(","))
    .fetch_one(tx.conn())
    .await?;
    for batch in 1..=265 {
        let identity=bitcoin::Txid::from_byte_array(H256::from_low_u64_be(batch as u64).0);
        via_test_utils::reader_db::verdict(tx.conn(),&via_test_utils::reader_db::ProofVerdict {
            batch,batch_hash:H256::from_low_u64_be(batch as u64),parent_hash:H256::from_low_u64_be((batch-1) as u64),
            proof_txid:identity,proof_blob:format!("proof-{batch}"),batch_txid:identity.to_string(),
            batch_blob:format!("batch-{batch}"),da_identifier:"fixture".into(),
        },via_reorg::chain::MessageRef { block:header.into(),carrier_txid:identity,tx_index:batch,message_index:0 },set).await?;
    }
    tx.commit().await?;
    recover(&pool, 156456).await?;
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 156449);
    fixture.chain.lock().unwrap().blocks.remove(&156456);
    let mut watcher = fixture.watcher(pool.clone()).await?;
    watcher.loop_iteration(&mut storage).await?;
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 156451);
    assert_eq!(
        storage.via_votes_dal().get_last_batch_in_canonical_chain().await?,
        Some((266, H256::from_low_u64_be(266).as_bytes().to_vec()))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gap_rolls_back_all_effects_and_pauses_retries() -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    fixture.add_block(2, Some((1, H256::repeat_byte(1), H256::zero())));
    fixture.add_block(3, Some((3, H256::repeat_byte(3), H256::repeat_byte(2))));
    let (pool, mut storage) = fixture.seeded(Some(1)).await?;
    let mut watcher = fixture.watcher(pool.clone()).await?;
    let error = watcher.loop_iteration(&mut storage).await.unwrap_err();
    let MessageProcessorError::ProofContinuityGap(gap) = error else {
        panic!("unexpected gap error {error:?}");
    };
    assert_eq!((gap.expected_batch, gap.observed_batch), (2, 3));
    assert_eq!(gap.canonical_parent, Some(H256::repeat_byte(1)));
    assert_eq!(gap.observed_parent, H256::repeat_byte(2));
    let mut proof_txid =
        fixture.chain.lock().unwrap().blocks[&3].txdata[0].compute_txid().to_byte_array();
    proof_txid.reverse();
    assert_eq!(gap.proof_txid, H256(proof_txid));
    assert_eq!(
        (gap.source_height, gap.source_hash),
        (3, fixture.chain.lock().unwrap().blocks[&3].block_hash())
    );
    assert_eq!(METRICS.proof_continuity_blocked.get(), 1);
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 1);
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, None);
    let requests = fixture.requests.load(Ordering::SeqCst);
    watcher.loop_iteration(&mut storage).await?;
    assert_eq!(fixture.requests.load(Ordering::SeqCst), requests);
    fixture.add_block(3, Some((2, H256::repeat_byte(2), H256::repeat_byte(1))));
    rewind_headers(&mut storage, 2).await?;
    fixture.seed(&mut storage, Some(1)).await?;
    let mut reconstructed = fixture.watcher(pool.clone()).await?;
    reconstructed.loop_iteration(&mut storage).await?;
    assert_reader_state(&mut storage, 3, Some((2, vec![2; 32]))).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scan_commits_before_waiting_recovery_and_reconstruction_replays() -> anyhow::Result<()> {
    let (fixture, pool, mut storage) = BitcoinFixture::with_proof().await?;
    let (entered, fetched) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    *fixture.fetch_barrier.lock().unwrap() = Some((entered, wait));
    let mut watcher = fixture.watcher(pool.clone()).await?;
    let scan_pool = pool.clone();
    let scan = tokio::spawn(async move {
        let mut storage = scan_pool.connection().await?;
        watcher.loop_iteration(&mut storage).await
    });
    fetched.await?;
    let recovery_pool = pool.clone();
    let recovery = tokio::spawn(async move { recover(&recovery_pool, 3).await });
    wait_for_owner_waiters(&mut storage, 1).await?;
    assert!(storage.via_l1_block_dal().has_reorg_in_progress().await?.is_none());
    assert!(!recovery.is_finished());
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 1);
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, None);
    release.send(())?;
    scan.await??;
    recovery.await??;
    assert_reader_state(&mut storage, 2, Some((1, vec![1; 32]))).await?;
    assert_eq!(storage.via_l1_block_dal().get_l1_block_hash(3).await?, None);
    fixture.seed(&mut storage, Some(1)).await?;
    let mut reconstructed = fixture.watcher(pool.clone()).await?;
    reconstructed.loop_iteration(&mut storage).await?;
    assert_reader_state(&mut storage, 3, Some((1, vec![1; 32]))).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rollback_before_commit_and_restart_after_commit_keep_effects_with_progress(
) -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    fixture.add_block(2, Some((1, H256::repeat_byte(1), H256::zero())));
    let (pool, mut storage) = fixture.seeded(Some(1)).await?;
    let mut watcher = fixture.watcher(pool.clone()).await?;
    let mut interrupted = storage.start_transaction().await?;
    watcher.loop_iteration(&mut interrupted).await?;
    drop(interrupted);
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 1);
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, None);
    let mut reconstructed = fixture.watcher(pool.clone()).await?;
    reconstructed.loop_iteration(&mut storage).await?;
    drop(reconstructed);
    let mut reconstructed = fixture.watcher(pool.clone()).await?;
    reconstructed.loop_iteration(&mut storage).await?;
    assert_reader_state(&mut storage, 2, Some((1, vec![1; 32]))).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_preserves_uninitialized_and_unchanged_progress() -> anyhow::Result<()> {
    for cursor in [None, Some(0), Some(1), Some(3)] {
        let fixture = BitcoinFixture::new()?;
        fixture.add_block(2, None);
        fixture.add_block(3, None);
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await?;
        fixture.seed(&mut storage, cursor).await?;
        recover(&pool, 3).await?;
        assert_eq!(
            storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?,
            u64::from(cursor.unwrap_or(0).min(2))
        );
        assert_eq!(storage.via_l1_block_dal().get_last_l1_block().await?.map(|row| row.0), Some(2));
        if cursor.unwrap_or(0) == 0 {
            let requests = fixture.requests.load(Ordering::SeqCst);
            let mut watcher = fixture.watcher(pool.clone()).await?;
            assert!(watcher.loop_iteration(&mut storage).await.is_err());
            assert_eq!(fixture.requests.load(Ordering::SeqCst), requests);
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn valid_fork_still_replaces_the_canonical_suffix() -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    fixture.add_block(2, Some((1, H256::repeat_byte(1), H256::zero())));
    fixture.add_block(3, Some((2, H256::repeat_byte(2), H256::repeat_byte(1))));
    let (pool, mut storage) = fixture.seeded(Some(1)).await?;
    let mut watcher = fixture.watcher(pool.clone()).await?;
    watcher.loop_iteration(&mut storage).await?;
    fixture.add_block(4, Some((2, H256::repeat_byte(4), H256::repeat_byte(1))));
    fixture.seed(&mut storage, Some(1)).await?;
    watcher.loop_iteration(&mut storage).await?;
    assert_reader_state(&mut storage, 4, Some((2, vec![4; 32]))).await?;
    assert!(watcher.proof_gap.is_none());
    Ok(())
}

async fn wait_for_owner_waiters(storage: &mut Connection<'_, Verifier>, count: i64) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE locktype='advisory' AND objid=$1::bigint::oid AND NOT granted AND database=(SELECT oid FROM pg_database WHERE datname=current_database())")
                .bind(BTC_WATCH_OWNER_LOCK_KEY).fetch_one(storage.conn()).await?;
            if waiting == count { break; }
            tokio::task::yield_now().await;
        }
        Ok::<_, anyhow::Error>(())
    }).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_owns_unchanged_height_before_scan_fetches() -> anyhow::Result<()> {
    let (fixture, pool, mut storage) = BitcoinFixture::with_proof().await?;
    let mut owner_connection = pool.connection().await?;
    let mut owner = owner_connection.start_transaction().await?;
    owner.via_indexer_dal().lock_btc_watch().await?;
    let recovery_pool = pool.clone();
    let recovery = tokio::spawn(async move { recover(&recovery_pool, 3).await });
    wait_for_owner_waiters(&mut storage, 1).await?;
    let mut watcher = fixture.watcher(pool.clone()).await?;
    let scan_pool = pool.clone();
    let scan = tokio::spawn(async move {
        let mut storage = scan_pool.connection().await?;
        watcher.loop_iteration(&mut storage).await
    });
    wait_for_owner_waiters(&mut storage, 2).await?;
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    owner.commit().await?;
    recovery.await??;
    scan.await??;
    let mut restarted = fixture.watcher(pool.clone()).await?;
    restarted.loop_iteration(&mut storage).await?;
    assert_reader_state(&mut storage, 2, Some((1, vec![1; 32]))).await?;
    assert_eq!(storage.via_l1_block_dal().get_l1_block_hash(3).await?, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_genesis_pauses_but_wrong_sequential_parent_is_rejected() -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    fixture.add_block(2, Some((3, H256::repeat_byte(3), H256::repeat_byte(2))));
    let (pool, mut storage) = fixture.seeded(Some(1)).await?;
    let mut watcher = fixture.watcher(pool.clone()).await?;
    let MessageProcessorError::ProofContinuityGap(gap) = watcher.loop_iteration(&mut storage).await.unwrap_err() else {
        panic!("missing genesis must surface a typed gap");
    };
    assert_eq!((gap.expected_batch, gap.canonical_parent, gap.observed_batch), (1, None, 3));
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 1);
    fixture.add_block(2, Some((1, H256::repeat_byte(1), H256::zero())));
    fixture.add_block(3, Some((2, H256::repeat_byte(2), H256::repeat_byte(9))));
    rewind_headers(&mut storage, 1).await?;
    fixture.seed(&mut storage, Some(1)).await?;
    let mut reconstructed = fixture.watcher(pool.clone()).await?;
    reconstructed.loop_iteration(&mut storage).await?;
    assert!(reconstructed.proof_gap.is_none());
    assert_reader_state(&mut storage, 3, Some((1, vec![1; 32]))).await?;
    Ok(())
}

#[tokio::test]
async fn header_owner_allows_wallet_checks_but_invalidation_gate_excludes_them(
) -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    let (pool, mut storage) = fixture.seeded(Some(1)).await?;
    let wallets = storage.via_wallet_dal().get_system_wallets_raw(1).await?.unwrap();
    let wallet = SystemWallets::try_from(wallets)?.bridge.script_pubkey();
    let mut owner_connection = pool.connection().await?;
    let mut owner = owner_connection.start_transaction().await?;
    owner.via_indexer_dal().lock_btc_watch().await?;
    storage.via_withdrawal_dal().verify_withdrawal_wallet(wallet.as_bytes()).await?;
    owner.via_indexer_dal().lock_withdrawal_invalidation().await?;
    let mut competing = storage.start_transaction().await?;
    sqlx::query("SET LOCAL lock_timeout = '100ms'").execute(competing.conn()).await?;
    let error = competing.via_withdrawal_dal().verify_withdrawal_wallet(wallet.as_bytes()).await.unwrap_err();
    assert!(format!("{error:#}").contains("lock timeout"), "{error:#}");
    competing.rollback().await?;
    owner.commit().await?;
    storage.via_withdrawal_dal().verify_withdrawal_wallet(wallet.as_bytes()).await?;
    Ok(())
}
