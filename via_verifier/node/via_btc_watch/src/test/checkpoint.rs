use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use bitcoin::{
    absolute,
    block::Header,
    consensus::encode::serialize_hex,
    hashes::Hash,
    secp256k1::{Keypair, Secp256k1, SecretKey},
    taproot::LeafVersion,
    transaction, Address, Block, BlockHash, CompressedPublicKey, Network, Transaction, TxIn, TxMerkleNode, Witness,
};
use serde_json::{json, Value};
use via_btc_client::{
    client::BitcoinClient,
    indexer::BitcoinInscriptionIndexer,
    types::{InscriptionMessage, L1BatchDAReferenceInput, NodeAuth, ProofDAReferenceInput},
};
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

struct BitcoinFixture {
    client: Arc<BitcoinClient>,
    blocks: Arc<Mutex<HashMap<u32, Block>>>,
    transactions: Arc<Mutex<HashMap<String, Transaction>>>,
    requests: Arc<AtomicUsize>,
    fetch_barrier: Arc<Mutex<Option<FetchBarrier>>>,
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
    address: std::net::SocketAddr,
    wallets: SystemWallets,
}

fn keypair() -> Keypair {
    Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&[1; 32]).unwrap())
}

fn inscription(message: InscriptionMessage, nonce: u32) -> Transaction {
    let key = keypair();
    let (internal, _) = key.x_only_public_key();
    let push = |bytes: &[u8]| bitcoin::script::PushBytesBuf::try_from(bytes.to_vec()).unwrap();
    let script = bitcoin::script::Builder::new()
        .push_slice(internal.serialize())
        .push_opcode(bitcoin::opcodes::all::OP_CHECKSIG)
        .push_opcode(bitcoin::opcodes::OP_FALSE)
        .push_opcode(bitcoin::opcodes::all::OP_IF)
        .push_slice(b"via_inscription_protocol");
    let script = match message {
        InscriptionMessage::L1BatchDAReference(batch) => script
            .push_slice(push(via_btc_client::types::L1_BATCH_DA_REFERENCE_MSG.as_bytes()))
            .push_slice(push(batch.l1_batch_hash.as_bytes()))
            .push_slice(batch.l1_batch_index.0.to_be_bytes())
            .push_slice(push(batch.da_identifier.as_bytes()))
            .push_slice(push(batch.blob_id.as_bytes()))
            .push_slice(push(batch.prev_l1_batch_hash.as_bytes())),
        InscriptionMessage::ProofDAReference(proof) => script
            .push_slice(push(via_btc_client::types::PROOF_DA_REFERENCE_MSG.as_bytes()))
            .push_slice(proof.l1_batch_reveal_txid.to_byte_array())
            .push_slice(push(proof.da_identifier.as_bytes()))
            .push_slice(push(proof.blob_id.as_bytes())),
        _ => unreachable!("checkpoint fixture only produces batch and proof messages"),
    }
    .push_opcode(bitcoin::opcodes::all::OP_ENDIF)
    .into_script();
    let mut control = vec![LeafVersion::TapScript.to_consensus()];
    control.extend(internal.serialize());
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::from_consensus(nonce),
        input: vec![
            TxIn { witness: Witness::from_slice(&[vec![0; 64], script.into_bytes(), control]), ..TxIn::default() },
            TxIn {
                witness: Witness::from_slice(&[vec![0; 64], key.public_key().serialize().to_vec()]),
                ..TxIn::default()
            },
        ],
        output: vec![],
    }
}

impl BitcoinFixture {
    fn new() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let client = Arc::new(BitcoinClient::new(
            &format!("http://{address}"),
            NodeAuth::None,
            ViaBtcClientConfig {
                network: "regtest".into(),
                external_apis: vec![],
                fee_strategies: vec![],
                use_rpc_for_fee_rate: Some(true),
            },
        )?);
        let blocks = Arc::new(Mutex::new(HashMap::<u32, Block>::new()));
        let transactions = Arc::new(Mutex::new(HashMap::<String, Transaction>::new()));
        let requests = Arc::new(AtomicUsize::new(0));
        let fetch_barrier = Arc::new(Mutex::new(None::<FetchBarrier>));
        let stop = Arc::new(AtomicBool::new(false));
        let server = {
            let blocks = blocks.clone();
            let transactions = transactions.clone();
            let requests = requests.clone();
            let fetch_barrier = fetch_barrier.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                'connections: for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let stream = stream.unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap() == 0 {
                            continue 'connections;
                        }
                        if line == "\r\n" {
                            break;
                        }
                        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            length = value.trim().parse::<usize>().unwrap();
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    requests.fetch_add(1, Ordering::SeqCst);
                    let result = match request["method"].as_str().unwrap() {
                        "getblockcount" => json!(blocks.lock().unwrap().keys().max().copied().unwrap()),
                        "getblockhash" => json!(blocks.lock().unwrap()
                            [&(request["params"][0].as_u64().unwrap() as u32)]
                            .block_hash()
                            .to_string()),
                        "getblock" => {
                            let barrier = fetch_barrier.lock().unwrap().take();
                            if let Some((entered, release)) = barrier {
                                entered.send(()).unwrap();
                                release.recv().unwrap();
                            }
                            let blocks = blocks.lock().unwrap();
                            let block = blocks
                                .values()
                                .find(|block| block.block_hash().to_string() == request["params"][0].as_str().unwrap())
                                .unwrap();
                            json!(serialize_hex(block))
                        }
                        "getrawtransaction" => {
                            json!(serialize_hex(&transactions.lock().unwrap()[request["params"][0].as_str().unwrap()]))
                        }
                        method => panic!("unexpected Bitcoin fixture method {method}"),
                    };
                    let response = json!({"result": result, "error": null, "id": request["id"]}).to_string();
                    write!(reader.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", response.len(), response).unwrap();
                }
            })
        };
        let sender = Address::p2wpkh(&CompressedPublicKey(keypair().public_key()), Network::Regtest);
        let wallets = SystemWallets {
            sequencer: sender.clone(),
            bridge: sender.clone(),
            governance: sender.clone(),
            verifiers: vec![sender],
        };
        Ok(Self { client, blocks, transactions, requests, fetch_barrier, stop, server: Some(server), address, wallets })
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
                self.transactions.lock().unwrap().insert(batch.compute_txid().to_string(), batch);
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
        let hash = block.block_hash();
        self.blocks.lock().unwrap().insert(height, block);
        hash
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

    async fn seed(&self, storage: &mut Connection<'_, Verifier>, cursor: Option<u32>) -> anyhow::Result<()> {
        storage.via_wallet_dal().insert_wallets(&SystemWalletsDetails::try_from(self.wallets.clone())?, 1).await?;
        if let Some(cursor) = cursor {
            storage.via_indexer_dal().init_indexer_metadata("via_btc_watch", cursor).await?;
        }
        let blocks =
            self.blocks.lock().unwrap().iter().map(|(&height, block)| (height, block.block_hash())).collect::<Vec<_>>();
        for (height, hash) in blocks {
            storage.via_l1_block_dal().insert_l1_block(i64::from(height), hash.to_string()).await?;
        }
        Ok(())
    }
}

impl Drop for BitcoinFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        self.server.take().unwrap().join().unwrap();
    }
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
async fn recovery_above_lagging_cursor_imports_the_previously_skipped_proof() -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    fixture.add_block(156450, None);
    fixture.add_block(156451, Some((266, H256::from_low_u64_be(266), H256::from_low_u64_be(265))));
    fixture.add_block(156456, None);
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await?;
    fixture.seed(&mut storage, Some(156449)).await?;
    for batch in 1..=265 {
        sqlx::query("INSERT INTO via_votable_transactions(l1_batch_number,l1_batch_hash,prev_l1_batch_hash,proof_blob_id,proof_reveal_tx_id,pubdata_blob_id,pubdata_reveal_tx_id,da_identifier) VALUES($1,$2,$3,$4,$2,$5,$5,'fixture')")
            .bind(i64::from(batch))
            .bind(H256::from_low_u64_be(batch as u64).as_bytes())
            .bind(H256::from_low_u64_be((batch - 1) as u64).as_bytes())
            .bind(format!("proof-{batch}"))
            .bind(format!("batch-{batch}"))
            .execute(storage.conn()).await?;
    }
    recover(&pool, 156456).await?;
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 156449);
    fixture.blocks.lock().unwrap().remove(&156456);
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
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await?;
    fixture.seed(&mut storage, Some(1)).await?;
    let mut watcher = fixture.watcher(pool.clone()).await?;
    let error = watcher.loop_iteration(&mut storage).await.unwrap_err();
    let MessageProcessorError::ProofContinuityGap(gap) = error else {
        panic!("unexpected gap error {error:?}");
    };
    assert_eq!((gap.expected_batch, gap.observed_batch), (2, 3));
    assert_eq!(gap.canonical_parent, Some(H256::repeat_byte(1)));
    assert_eq!(gap.observed_parent, H256::repeat_byte(2));
    let mut proof_txid = fixture.blocks.lock().unwrap()[&3].txdata[0].compute_txid().to_byte_array();
    proof_txid.reverse();
    assert_eq!(gap.proof_txid, H256(proof_txid));
    assert_eq!((gap.source_height, gap.source_hash), (3, fixture.blocks.lock().unwrap()[&3].block_hash()));
    assert_eq!(METRICS.proof_continuity_blocked.get(), 1);
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 1);
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, None);
    let requests = fixture.requests.load(Ordering::SeqCst);
    watcher.loop_iteration(&mut storage).await?;
    assert_eq!(fixture.requests.load(Ordering::SeqCst), requests);
    fixture.add_block(3, Some((2, H256::repeat_byte(2), H256::repeat_byte(1))));
    storage.via_l1_block_dal().delete_l1_blocks(2).await?;
    fixture.seed(&mut storage, Some(1)).await?;
    let mut reconstructed = fixture.watcher(pool.clone()).await?;
    reconstructed.loop_iteration(&mut storage).await?;
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 3);
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, Some((2, vec![2; 32])));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scan_commits_before_waiting_recovery_and_reconstruction_replays() -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    fixture.add_block(2, Some((1, H256::repeat_byte(1), H256::zero())));
    fixture.add_block(3, None);
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await?;
    fixture.seed(&mut storage, Some(1)).await?;
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
    tokio::time::timeout(Duration::from_secs(5), async {
        while storage.via_l1_block_dal().has_reorg_in_progress().await.unwrap().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert!(!recovery.is_finished());
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 1);
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, None);
    release.send(())?;
    scan.await??;
    recovery.await??;
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 2);
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, Some((1, vec![1; 32])));
    assert_eq!(storage.via_l1_block_dal().get_l1_block_hash(3).await?, None);
    fixture.seed(&mut storage, Some(1)).await?;
    let mut reconstructed = fixture.watcher(pool.clone()).await?;
    reconstructed.loop_iteration(&mut storage).await?;
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 3);
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, Some((1, vec![1; 32])));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rollback_before_commit_and_restart_after_commit_keep_effects_with_progress() -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    fixture.add_block(2, Some((1, H256::repeat_byte(1), H256::zero())));
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await?;
    fixture.seed(&mut storage, Some(1)).await?;
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
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 2);
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, Some((1, vec![1; 32])));
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
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await?;
    fixture.seed(&mut storage, Some(1)).await?;
    let mut watcher = fixture.watcher(pool.clone()).await?;
    watcher.loop_iteration(&mut storage).await?;
    fixture.add_block(4, Some((2, H256::repeat_byte(4), H256::repeat_byte(1))));
    fixture.seed(&mut storage, Some(1)).await?;
    watcher.loop_iteration(&mut storage).await?;
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 4);
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, Some((2, vec![4; 32])));
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
    let fixture = BitcoinFixture::new()?;
    fixture.add_block(2, Some((1, H256::repeat_byte(1), H256::zero())));
    fixture.add_block(3, None);
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await?;
    fixture.seed(&mut storage, Some(1)).await?;
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
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 2);
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, Some((1, vec![1; 32])));
    assert_eq!(storage.via_l1_block_dal().get_l1_block_hash(3).await?, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_genesis_pauses_but_wrong_sequential_parent_is_rejected() -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    fixture.add_block(2, Some((3, H256::repeat_byte(3), H256::repeat_byte(2))));
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await?;
    fixture.seed(&mut storage, Some(1)).await?;
    let mut watcher = fixture.watcher(pool.clone()).await?;
    let MessageProcessorError::ProofContinuityGap(gap) = watcher.loop_iteration(&mut storage).await.unwrap_err() else {
        panic!("missing genesis must surface a typed gap");
    };
    assert_eq!((gap.expected_batch, gap.canonical_parent, gap.observed_batch), (1, None, 3));
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 1);
    fixture.add_block(2, Some((1, H256::repeat_byte(1), H256::zero())));
    fixture.add_block(3, Some((2, H256::repeat_byte(2), H256::repeat_byte(9))));
    storage.via_l1_block_dal().delete_l1_blocks(1).await?;
    fixture.seed(&mut storage, Some(1)).await?;
    let mut reconstructed = fixture.watcher(pool.clone()).await?;
    reconstructed.loop_iteration(&mut storage).await?;
    assert!(reconstructed.proof_gap.is_none());
    assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 3);
    assert_eq!(storage.via_votes_dal().get_last_batch_in_canonical_chain().await?, Some((1, vec![1; 32])));
    Ok(())
}

#[tokio::test]
async fn header_owner_allows_wallet_checks_but_invalidation_gate_excludes_them() -> anyhow::Result<()> {
    let fixture = BitcoinFixture::new()?;
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await?;
    fixture.seed(&mut storage, Some(1)).await?;
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
