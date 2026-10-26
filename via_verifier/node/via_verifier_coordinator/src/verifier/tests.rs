use bitcoin::{
    absolute, hashes::Hash, key::TweakedPublicKey, transaction, Address, Amount, Block, BlockHash,
    CompressedPublicKey, Network, OutPoint, PrivateKey, TxOut, Txid,
};
use bitcoincore_rpc::json::GetBlockStatsResult;
use parking_lot::Mutex;
use via_btc_client::{inscriber::test_utils::MockBitcoinOps, types::BitcoinClientResult};
use via_verifier_types::withdrawal::{CompleteWithdrawalBatch, WithdrawalRequest};
use zksync_db_connection::instrument::InstrumentExt;
use zksync_types::{
    via_wallet::{SystemWallets, SystemWalletsDetails},
    H256,
};

use super::*;

#[test]
fn later_input_missing_share_never_completes_round() {
    let transcript = BTreeMap::from([
        (0, BTreeMap::from([(0, "a".into()), (1, "b".into())])),
        (1, BTreeMap::from([(0, "c".into())])),
    ]);
    assert!(!complete(&transcript, 2, 2));
    assert!(validate_transcript(&transcript, 2, 2).is_err());
}

#[test]
fn substituted_own_nonce_is_not_retried_as_missing() {
    let transcript = BTreeMap::from([(0, BTreeMap::from([(0, "changed".into())]))]);
    let cached = BTreeMap::from([(
        0,
        NoncePair {
            signer_index: 0,
            nonce: "durable".into(),
        },
    )]);
    assert!(contains_batch(&transcript, &cached, 0).is_err());
}

// Only the Bitcoin boundary is simulated. All authorization, HTTP authentication,
// nonce handling, PostgreSQL commits, and signature aggregation are production code.
struct RecordingBitcoin {
    inner: MockBitcoinOps,
    broadcasts: Mutex<Vec<Vec<u8>>>,
    parent_fault: std::sync::atomic::AtomicU8,
    parent_fetches: std::sync::atomic::AtomicUsize,
}

#[axum::async_trait]
impl BitcoinOps for RecordingBitcoin {
    async fn best_block_hash(
        &self,
    ) -> via_btc_client::types::BitcoinClientResult<bitcoin::BlockHash> {
        self.inner.best_block_hash().await
    }
    async fn header_by_hash(
        &self,
        hash: &bitcoin::BlockHash,
    ) -> via_btc_client::types::BitcoinClientResult<Option<via_btc_client::types::BlockHeaderInfo>>
    {
        self.inner.header_by_hash(hash).await
    }
    async fn canonical_header_at(
        &self,
        height: u32,
    ) -> via_btc_client::types::BitcoinClientResult<via_btc_client::types::BlockHeaderInfo> {
        self.inner.canonical_header_at(height).await
    }
    async fn get_balance(&self, a: &Address) -> BitcoinClientResult<u128> {
        self.inner.get_balance(a).await
    }
    async fn broadcast_signed_transaction(&self, hex: &str) -> BitcoinClientResult<Txid> {
        let bytes = hex::decode(hex).unwrap();
        let tx: Transaction = bitcoin::consensus::deserialize(&bytes).unwrap();
        self.broadcasts.lock().push(bytes);
        Ok(tx.compute_txid())
    }
    async fn fetch_utxos(&self, a: &Address) -> BitcoinClientResult<Vec<(OutPoint, TxOut)>> {
        self.inner.fetch_utxos(a).await
    }
    async fn check_tx_confirmation(&self, id: &Txid, n: u32) -> BitcoinClientResult<bool> {
        self.inner.check_tx_confirmation(id, n).await
    }
    async fn fetch_block_height(&self) -> BitcoinClientResult<u64> {
        self.inner.fetch_block_height().await
    }
    async fn get_fee_rate(&self, n: u16) -> BitcoinClientResult<u64> {
        self.inner.get_fee_rate(n).await
    }
    fn get_network(&self) -> Network {
        self.inner.get_network()
    }
    async fn fetch_block(&self, n: u128) -> BitcoinClientResult<Block> {
        self.inner.fetch_block(n).await
    }
    async fn get_transaction(&self, id: &Txid) -> BitcoinClientResult<Transaction> {
        use std::sync::atomic::Ordering;
        self.parent_fetches.fetch_add(1, Ordering::SeqCst);
        if self.parent_fault.load(Ordering::SeqCst) == 1 {
            return Err(via_btc_client::types::BitcoinError::Other("parent unavailable".into()));
        }
        let mut transaction = self.inner.get_transaction(id).await?;
        if self.parent_fault.load(Ordering::SeqCst) == 2 {
            transaction.output[0].value += Amount::from_sat(1);
        }
        Ok(transaction)
    }
    async fn fetch_block_by_hash(&self, h: &BlockHash) -> BitcoinClientResult<Block> {
        self.inner.fetch_block_by_hash(h).await
    }
    async fn get_block_stats(&self, n: u64) -> BitcoinClientResult<GetBlockStatsResult> {
        self.inner.get_block_stats(n).await
    }
    async fn get_fee_history(&self, a: usize, b: usize) -> BitcoinClientResult<Vec<u64>> {
        self.inner.get_fee_history(a, b).await
    }
}

pub(super) fn withdrawal_client() -> WithdrawalClient {
    WithdrawalClient::new(
        Box::new(zksync_da_clients::no_da::NoDAClient),
        Network::Bitcoin,
        Box::new(
            zksync_web3_decl::client::Client::http("http://127.0.0.1:1".parse().unwrap())
                .unwrap()
                .build(),
        ),
    )
}

fn bitcoin_fixture() -> MockBitcoinOps {
    use via_btc_client::types::{BlockHeaderInfo, HeaderMembership};
    let chain = via_test_utils::chain::Chain::with_height(6);
    MockBitcoinOps {
        network: Network::Bitcoin,
        headers: chain
            .blocks
            .iter()
            .map(|(&height, block)| BlockHeaderInfo {
                height,
                hash: block.block_hash(),
                parent_hash: block.header.prev_blockhash,
                membership: HeaderMembership::Canonical(
                    std::num::NonZeroU32::new(7 - height).unwrap(),
                ),
            })
            .collect(),
        block: Some(chain.blocks[&6].clone()),
        block_height: 6,
        ..Default::default()
    }
}

async fn seed_reader(
    pool: &ConnectionPool<Verifier>,
    wallets: &SystemWallets,
    source: &dyn BitcoinOps,
) -> via_btc_storage::StoredHeader {
    use via_btc_storage::{ReaderDal, WalletSource, PROTOCOL_CONSUMER};
    use via_reorg::chain::{Header, BlockRef};
    let mut db = pool.connection().await.unwrap();
    let root = source.canonical_header_at(0).await.unwrap();
    db.via_store_mode_dal()
        .ensure_proof_verification_mode(&via_verifier_dal::via_store_mode_dal::StoreMode {
            proof_verification_dev_mode: false,
            bitcoin_network: source.get_network().to_string(),
            bitcoin_genesis_hash: root.hash.to_string(),
        })
        .await
        .unwrap();
    let mut tx = db.start_transaction().await.unwrap();
    let mut reader = ReaderDal::new(&mut tx);
    let head = source.canonical_header_at(6).await.unwrap();
    reader
        .initialize(
            &source.get_network().to_string(),
            root.hash,
            Header { height: 0, block_hash: root.hash, parent_hash: root.parent_hash },
            BlockRef { height: 6, block_hash: head.hash },
        )
        .await
        .unwrap();
    reader
        .insert_wallets(
            &SystemWalletsDetails::try_from(wallets.clone()).unwrap(),
            WalletSource::Unbound(0),
        )
        .await
        .unwrap();
    let anchor = reader.header_at(0).await.unwrap().unwrap();
    let mut previous = anchor.clone();
    for height in 0..=6 {
        if height > 0 {
            let header = source.canonical_header_at(height).await.unwrap();
            reader
                .save_block(
                    Header { height, block_hash: header.hash, parent_hash: header.parent_hash },
                    &previous,
                )
                .await
                .unwrap();
            previous = reader.header_at(height).await.unwrap().unwrap();
        }
        for name in [PROTOCOL_CONSUMER, "withdrawals"] {
            reader.complete_consumer(name, &previous, &anchor).await.unwrap();
        }
    }
    let proof_source = reader.header_at(1).await.unwrap().unwrap();
    reader.advance_frontier(&previous, &proof_source).await.unwrap();
    assert!(reader.complete_replay().await.unwrap());
    tx.commit().await.unwrap();
    proof_source
}

async fn seed(
    pool: &ConnectionPool<Verifier>,
    wallets: &SystemWallets,
    batch: &CompleteWithdrawalBatch,
    bitcoin: &dyn BitcoinOps,
) {
    use via_btc_client::{
        indexer::replay::{Included, MessageOrigin, Payload},
        types::{L1BatchDAReferenceInput, ProofDAReferenceInput},
    };
    let source = seed_reader(pool, wallets, bitcoin).await;
    let mut db = pool.start_transaction().await.unwrap();
    let proof = H256::repeat_byte(4);
    let origin = MessageOrigin {
        height: 1,
        hash: source.header.block_hash,
        transaction_index: 0,
        message_index: 0,
        txid: Txid::from_byte_array([3; 32]),
    };
    let pubdata = Included {
        origin: origin.clone(),
        payload: Payload {
            input: L1BatchDAReferenceInput {
                l1_batch_index: zksync_types::L1BatchNumber(1),
                l1_batch_hash: H256::repeat_byte(3),
                prev_l1_batch_hash: H256::zero(),
                da_identifier: "fixture".into(),
                blob_id: batch.blob_id.clone(),
            },
            public_key: vec![],
            signature: vec![],
            sender_script: None,
        },
    };
    let pubdata_txid = pubdata.origin.txid;
    let input = via_test_utils::inscription::unsigned_proof(
        pubdata,
        MessageOrigin { txid: Txid::from_byte_array([4; 32]), transaction_index: 1, ..origin },
        ProofDAReferenceInput {
            l1_batch_reveal_txid: pubdata_txid,
            da_identifier: "fixture".into(),
            blob_id: "proof".into(),
        },
    );
    db.via_votes_dal().save_reader_proof(&input, source.id).await.unwrap();
    let id = db
        .via_votes_dal()
        .verify_votable_transaction(1, proof, true)
        .await
        .unwrap();
    sqlx::query("UPDATE via_votable_transactions SET is_finalized=TRUE,finalized_header_id=proof_header_id WHERE id=$1")
        .bind(id)
        .execute(db.conn())
        .await
        .unwrap();
    db.commit().await.unwrap();
    let mut db = pool.connection().await.unwrap();
    db.via_withdrawal_dal()
        .import_complete_batch(
            wallets.bridge.script_pubkey().as_bytes(),
            proof.as_bytes(),
            batch,
        )
        .await
        .unwrap();

    // A PostgreSQL error rolls back the first nonce write; sequence increments
    // survive rollback, so the next real iteration can retry without test hooks.
    // The second half asserts signing risk was committed in an earlier transaction
    // than the public shares, not merely assigned earlier within one transaction.
    for sql in [
        "CREATE SEQUENCE runtime_nonce_failures",
        "CREATE TABLE runtime_signing_marker (round_id BYTEA PRIMARY KEY, transaction_id BIGINT NOT NULL)",
        "CREATE FUNCTION runtime_fault_and_marker() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
          IF OLD.public_nonces IS NULL AND NEW.public_nonces IS NOT NULL AND nextval('runtime_nonce_failures') = 1 THEN
            RAISE EXCEPTION 'injected nonce rollback';
          END IF;
          IF OLD.state = 'admitted' AND NEW.state = 'may_have_signed' THEN
            INSERT INTO runtime_signing_marker VALUES (NEW.round_id, txid_current());
          END IF;
          IF OLD.public_signatures IS NULL AND NEW.public_signatures IS NOT NULL THEN
            IF OLD.state <> 'may_have_signed' OR NOT OLD.signed_risk OR NOT EXISTS (
              SELECT 1 FROM runtime_signing_marker WHERE round_id = NEW.round_id AND transaction_id <> txid_current()
            ) THEN RAISE EXCEPTION 'shares preceded durable signing risk'; END IF;
          END IF;
          RETURN NEW;
        END $$",
        "CREATE TRIGGER runtime_fault_and_marker BEFORE UPDATE ON via_withdrawal_attempts FOR EACH ROW EXECUTE FUNCTION runtime_fault_and_marker()",
    ] {
        sqlx::query(sql).instrument("runtime_recovery_fixture").execute(&mut db).await.unwrap();
    }
}

async fn withdrawal_reservation_counts(db: &mut Connection<'_, Verifier>) -> (i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM via_withdrawal_attempts), \
         (SELECT count(*) FROM via_withdrawal_request_reservations), \
         (SELECT count(*) FROM via_withdrawal_input_reservations)",
    )
    .fetch_one(db.conn())
    .await
    .unwrap()
}

// Requires the same isolated PostgreSQL test template as verifier_dal tests.
#[tokio::test]
async fn bounded_signing_valid_heavy_parent_downloads_signs_and_recovers() {
    run_actual_iterations(
        true,
        PeerMode::Writer,
        [PeerMode::Writer, PeerMode::Legacy],
        false,
        false,
    )
    .await;
}

async fn run_actual_iterations(
    heavy: bool,
    coordinator_mode: PeerMode,
    signer_modes: [PeerMode; 2],
    historical: bool,
    lose_acks: bool,
) {
    let secp = Secp256k1::new();
    let (keys, public, wallets) = fixture_wallets(2);
    let addresses = &wallets.verifiers;
    let bridge = &wallets.bridge;
    let output_key =
        bitcoin::XOnlyPublicKey::from_slice(&bridge.script_pubkey().as_bytes()[2..]).unwrap();
    let wallet = bridge.script_pubkey().into_bytes();
    let parent = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![],
        output: vec![
            TxOut {
                value: Amount::from_sat(100_000),
                script_pubkey: bridge.script_pubkey(),
            },
            TxOut {
                value: Amount::from_sat(100_000),
                script_pubkey: bridge.script_pubkey(),
            },
        ],
    };
    let (core, parent) = if heavy {
        let (node, parent) = heavy_parent(&bridge.script_pubkey(), 790);
        assert!((390_000..400_000).contains(&bitcoin::consensus::serialize(&parent).len()));
        (Some(node), parent)
    } else {
        (None, parent)
    };
    let funding_parent = parent.clone();
    let btc = Arc::new(RecordingBitcoin {
        inner: MockBitcoinOps {
            utxos: parent_utxos(&parent),
            transaction: Some(parent),
            fee_rate: 2,
            ..bitcoin_fixture()
        },
        broadcasts: Mutex::new(Vec::new()),
        parent_fault: std::sync::atomic::AtomicU8::new(0),
        parent_fetches: std::sync::atomic::AtomicUsize::new(0),
    });
    // A gross amount larger than either input forces a complete two-input batch.
    let batch = CompleteWithdrawalBatch {
        batch_number: 1,
        chain_id: 270,
        network: Network::Bitcoin,
        protocol_version: 1,
        blob_id: "runtime-batch".into(),
        pubdata_hash: H256::repeat_byte(9),
        start_block: 1,
        end_block: 1,
        pubdata: vec![1, 2, 3],
        receipts: vec![],
        nonpayable: vec![],
        withdrawals: vec![fixture_withdrawal(&addresses[1], 150_000)],
    };
    let pools = [
        ConnectionPool::<Verifier>::test_pool().await,
        ConnectionPool::<Verifier>::test_pool().await,
    ];
    for pool in &pools {
        seed(pool, &wallets, &batch, btc.as_ref()).await;
    }
    let coordinator =
        TestCoordinator::start(pools[0].clone(), btc.clone(), &public, &keys[0], coordinator_mode)
            .await;
    let config = coordinator.config.clone();
    let session = coordinator.session.clone();
    let bridge_config = ViaBridgeConfig {
        verifiers_pub_keys: public.clone(),
        bridge_address: bridge.to_string(),
    };
    let make_verifier = |index: usize| {
        let mut config = config.clone();
        config.withdrawal_compact_transport_writes_enabled =
            signer_modes[index] == PeerMode::Writer;
        config.role = if index == 0 {
            ViaNodeRole::Coordinator
        } else {
            ViaNodeRole::Verifier
        };
        config.wallet_address = addresses[index].to_string();
        ViaWithdrawalVerifier::new(
            config,
            ViaWallet::new(addresses[index].to_string(), keys[index].to_wif()),
            pools[index].clone(),
            btc.clone(),
            withdrawal_client(),
            bridge_config.clone(),
        )
        .unwrap()
    };
    let mut first = make_verifier(0);
    let mut second = make_verifier(1);
    let mut owner_a = pools[0].connection().await.unwrap();
    let mut owner_b = pools[1].connection().await.unwrap();
    assert!(owner_a
        .via_withdrawal_dal()
        .acquire_withdrawal_signer_lock(&wallet)
        .await
        .unwrap());
    assert!(owner_b
        .via_withdrawal_dal()
        .acquire_withdrawal_signer_lock(&wallet)
        .await
        .unwrap());
    // Exercises the real GET /session/ route and signed response binding.
    assert!(first.snapshot().await.unwrap().session_op.is_empty());
    if historical {
        first.withdrawal_session.prepare_session().await.unwrap();
        let operation = first.withdrawal_session.session().await.unwrap().unwrap();
        let SessionOperation::Withdrawal(tx, messages, _) = operation;
        let operation = SessionOperation::Withdrawal(tx, messages, vec![funding_parent.clone()]);
        let expected = first.withdrawal_session.authorize_withdrawal(&operation).await.unwrap();
        let content = bincode::serialize(&AdmittedContent {
            proposal: via_btc_client::traits::Serializable::to_bytes(&operation),
            network: Network::Bitcoin,
            chain_id: 270,
            wallet: wallet.clone(),
            participants: public.clone(),
            tweak: None,
            expected: expected.clone(),
        })
        .unwrap();
        let round_id = random_id();
        owner_a
            .via_withdrawal_dal()
            .expose_withdrawal_proposal(
                &wallet,
                &round_id,
                &content,
                &expected,
                &operation.get_unsigned_bridge_tx().utxos,
            )
            .await
            .unwrap();
        *session.write().await = crate::types::SigningSession {
            round_id,
            content_hash: crate::auth::digest(&content),
            authorized_content: content.clone(),
            session_op: Some(operation),
            created_at: crate::utils::seconds_since_epoch(),
            ..Default::default()
        };
        let old = TestCoordinator::start(
            pools[0].clone(),
            btc.clone(),
            &public,
            &keys[0],
            PeerMode::Legacy,
        )
        .await;
        *old.session.write().await = session.read().await.clone();
        let (request, response) = old
            .call(
                &keys[0],
                "GET",
                "/session/?transport_version=2",
                TransportVersion::Legacy,
                Vec::new(),
            )
            .await;
        assert_eq!(response.status().as_u16(), 200);
        let bytes = response.bytes().await.unwrap();
        assert!(bytes.len() > MAX_BODY, "legacy final envelope must cross the real receive cap");
        let body =
            super::legacy::reply_body(&request, &bytes, &keys[0].public_key(&secp).inner, 200)
                .unwrap();
        let old_snapshot: super::legacy::SigningSessionResponse =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(old_snapshot.authorized_content, content);
        let url = first.verifier_config.coordinator_http_url.clone();
        first.verifier_config.coordinator_http_url = old.config.coordinator_http_url.clone();
        assert!(first
            .snapshot()
            .await
            .unwrap_err()
            .to_string()
            .contains("Oversized coordinator response"));
        first.verifier_config.coordinator_http_url = url;
        let (request, response) = coordinator
            .call(&keys[0], "GET", "/session/", TransportVersion::Legacy, Vec::new())
            .await;
        assert_unservable_reply(&request, response, &public[0]).await;
        assert_eq!(
            owner_a
                .via_withdrawal_dal()
                .get_withdrawal_attempt(&wallet, &round_id)
                .await
                .unwrap()
                .unwrap()
                .content,
            content
        );
        assert_eq!(
            owner_a.via_withdrawal_dal().held_withdrawal_inputs(&wallet).await.unwrap().len(),
            2
        );
    }
    if heavy && !historical {
        for fault in [1, 2] {
            btc.parent_fault.store(fault, std::sync::atomic::Ordering::SeqCst);
            assert!(first.iteration(&mut owner_a, &wallet).await.is_err());
            assert!(first.live.is_none());
            assert!(session.read().await.session_op.is_none());
            assert!(owner_a
                .via_withdrawal_dal()
                .list_recoverable_withdrawal_attempts(&wallet)
                .await
                .unwrap()
                .is_empty());
        }
        btc.parent_fault.store(0, std::sync::atomic::Ordering::SeqCst);
    }
    assert!(first.iteration(&mut owner_a, &wallet).await.is_err());
    let pending_a = first.live.as_ref().unwrap().public_nonces.clone();
    let round = first.live.as_ref().unwrap().id;
    let failed = owner_a
        .via_withdrawal_dal()
        .get_withdrawal_attempt(&wallet, &round)
        .await
        .unwrap()
        .unwrap();
    let unauthenticated = reqwest::Client::new()
        .get(format!("{}/session/", config.coordinator_http_url))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(failed.state, WithdrawalAttemptState::Admitted);
    assert!(failed.public_nonces.is_none());
    assert!(first.snapshot().await.unwrap().nonces.is_empty());
    if lose_acks {
        use std::sync::atomic::Ordering;
        coordinator.faults.mask.store(3, Ordering::SeqCst);
        let posts = coordinator.faults.nonces.load(Ordering::SeqCst);
        assert!(first.iteration(&mut owner_a, &wallet).await.is_err());
        assert_eq!(coordinator.faults.nonces.load(Ordering::SeqCst), posts + 1);
        assert_eq!(first.live.as_ref().unwrap().public_nonces, pending_a);
        assert!(!first.snapshot().await.unwrap().nonces.is_empty());
    }
    first.iteration(&mut owner_a, &wallet).await.unwrap();
    assert_eq!(
        owner_a
            .via_withdrawal_dal()
            .get_withdrawal_attempt(&wallet, &round)
            .await
            .unwrap()
            .unwrap()
            .public_nonces
            .unwrap(),
        pending_a
    );
    // Slow peers cannot consume more bridge inputs through age-based replacement.
    session.write().await.created_at = 1;
    first.iteration(&mut owner_a, &wallet).await.unwrap();
    let stalled = first.snapshot().await.unwrap();
    assert_eq!(stalled.round_id, round);
    assert_eq!(stalled.authorized_content, failed.content);
    assert_eq!(first.live.as_ref().unwrap().public_nonces, pending_a);
    if heavy && !historical {
        let original = session.read().await.clone();
        for change_payment in [false, true] {
            let mut substituted = original.clone();
            let mut admitted: AdmittedContent =
                bincode::deserialize(&substituted.authorized_content).unwrap();
            if change_payment {
                let mut operation: SessionOperation =
                    bincode::deserialize(&admitted.proposal).unwrap();
                let SessionOperation::Withdrawal(tx, _, _) = &mut operation;
                tx.tx.output[0].value += Amount::from_sat(1);
                tx.txid = tx.tx.compute_txid();
                admitted.proposal = via_btc_client::traits::Serializable::to_bytes(&operation);
                substituted.session_op = Some(operation);
            } else {
                admitted.expected[0].amount += Amount::from_sat(1);
            }
            substituted.authorized_content = bincode::serialize(&admitted).unwrap();
            substituted.content_hash = crate::auth::digest(&substituted.authorized_content);
            *session.write().await = substituted;
            let error = second.iteration(&mut owner_b, &wallet).await.unwrap_err();
            assert!(!error.to_string().contains("Invalid proposal commitment"));
            assert!(second.live.is_none());
            assert!(owner_b
                .via_withdrawal_dal()
                .get_withdrawal_attempt(&wallet, &round)
                .await
                .unwrap()
                .is_none());
        }
        *session.write().await = original;
        let before = btc.parent_fetches.load(std::sync::atomic::Ordering::SeqCst);
        for fault in [1, 2] {
            btc.parent_fault.store(fault, std::sync::atomic::Ordering::SeqCst);
            assert!(second.iteration(&mut owner_b, &wallet).await.is_err());
            assert!(second.live.is_none());
            assert!(owner_b
                .via_withdrawal_dal()
                .get_withdrawal_attempt(&wallet, &round)
                .await
                .unwrap()
                .is_none());
        }
        assert!(btc.parent_fetches.load(std::sync::atomic::Ordering::SeqCst) > before);
        btc.parent_fault.store(0, std::sync::atomic::Ordering::SeqCst);
    }
    assert!(second.iteration(&mut owner_b, &wallet).await.is_err());
    let pending_b = second.live.as_ref().unwrap().public_nonces.clone();
    // Model the ambiguous-commit outcome at the DB boundary: persistence has
    // committed while the live process still has the same pending nonce batch.
    // No production fault hook or regeneration of secret state is involved.
    owner_b
        .via_withdrawal_dal()
        .persist_withdrawal_nonces(
            &wallet,
            &round,
            &second.live.as_ref().unwrap().content,
            &pending_b,
        )
        .await
        .unwrap();
    second.iteration(&mut owner_b, &wallet).await.unwrap();
    let snapshot = first.snapshot().await.unwrap();
    let op: SessionOperation = bincode::deserialize(&snapshot.session_op).unwrap();
    assert_eq!(
        op.get_parent_transactions().is_empty(),
        coordinator_mode != PeerMode::Legacy && !historical
    );
    if let Some(core) = &core {
        let parent =
            btc.inner.get_transaction(&op.get_unsigned_bridge_tx().utxos[0].0.txid).await.unwrap();
        assert_eq!(
            parent,
            bitcoin::consensus::deserialize::<Transaction>(
                &hex::decode(
                    core.client
                        .call::<String>(
                            "getrawtransaction",
                            &[serde_json::json!(parent.compute_txid()),]
                        )
                        .unwrap()
                )
                .unwrap(),
            )
            .unwrap()
        );
        assert!(bitcoin::consensus::serialize(&parent).len() >= 390_000);
    }
    assert_eq!(op.get_message_to_sign().len(), 2);
    assert!(complete(&snapshot.nonces, 2, 2));
    let own_b = serde_json::from_slice(&pending_b).unwrap();
    assert!(contains_batch(&snapshot.nonces, &own_b, 1).unwrap());
    if lose_acks {
        use std::sync::atomic::Ordering;
        let posts = coordinator.faults.shares.load(Ordering::SeqCst);
        assert!(first.iteration(&mut owner_a, &wallet).await.is_err());
        assert_eq!(coordinator.faults.shares.load(Ordering::SeqCst), posts + 1);
        assert!(first.live.as_ref().unwrap().signers.is_none());
        let cached = owner_a
            .via_withdrawal_dal()
            .get_withdrawal_attempt(&wallet, &round)
            .await
            .unwrap()
            .unwrap()
            .public_signatures
            .unwrap();
        first.iteration(&mut owner_a, &wallet).await.unwrap();
        assert_eq!(
            owner_a
                .via_withdrawal_dal()
                .get_withdrawal_attempt(&wallet, &round)
                .await
                .unwrap()
                .unwrap()
                .public_signatures
                .unwrap(),
            cached
        );
        assert_eq!(coordinator.faults.shares.load(Ordering::SeqCst), posts + 1);
    } else {
        first.iteration(&mut owner_a, &wallet).await.unwrap();
    }
    second.iteration(&mut owner_b, &wallet).await.unwrap();
    for owner in [&mut owner_a, &mut owner_b] {
        let record = owner
            .via_withdrawal_dal()
            .get_withdrawal_attempt(&wallet, &round)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.state, WithdrawalAttemptState::Signed);
        assert!(record.public_signatures.is_some());
        assert!(record.finalized_transaction.is_none());
    }
    assert!(complete(&first.snapshot().await.unwrap().signatures, 2, 2));
    let durable_shares = owner_a
        .via_withdrawal_dal()
        .get_withdrawal_attempt(&wallet, &round)
        .await
        .unwrap()
        .unwrap()
        .public_signatures
        .unwrap();
    assert!(btc.broadcasts.lock().is_empty());

    if heavy {
        use std::sync::atomic::Ordering;
        let shares = coordinator.faults.shares.load(Ordering::SeqCst);
        sqlx::query("UPDATE via_btc_reader SET replay_ready=FALSE")
            .execute(owner_a.conn())
            .await
            .unwrap();
        owner_a.via_withdrawal_dal().release_withdrawal_signer_lock(&wallet).await.unwrap();
        drop(owner_a);
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let run = tokio::spawn(first.run(receiver));
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        stop.send(true).unwrap();
        run.await.unwrap().unwrap();
        owner_a = pools[0].connection().await.unwrap();
        let held = owner_a
            .via_withdrawal_dal()
            .get_withdrawal_attempt(&wallet, &round)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(held.state, WithdrawalAttemptState::Signed);
        assert_eq!(held.public_signatures.as_ref(), Some(&durable_shares));
        assert!(held.finalized_transaction.is_none());
        assert!(btc.broadcasts.lock().is_empty());
        assert_eq!(coordinator.faults.shares.load(Ordering::SeqCst), shares);
        sqlx::query("UPDATE via_btc_reader SET replay_ready=TRUE")
            .execute(owner_a.conn())
            .await
            .unwrap();
        assert!(owner_a
            .via_withdrawal_dal()
            .acquire_withdrawal_signer_lock(&wallet)
            .await
            .unwrap());
        first = make_verifier(0);
    }

    // Lose every secret nonce and the coordinator's in-memory public transcript.
    // The restarted coordinator reconstructs its round from its durable shares;
    // the second signer republishes its durable batch over authenticated HTTP.
    drop(first);
    drop(second);
    *session.write().await = crate::types::SigningSession::default();
    owner_a
        .via_withdrawal_dal()
        .release_withdrawal_signer_lock(&wallet)
        .await
        .unwrap();
    owner_b
        .via_withdrawal_dal()
        .release_withdrawal_signer_lock(&wallet)
        .await
        .unwrap();
    drop(owner_a);
    drop(owner_b);
    let mut first = make_verifier(0);
    let mut second = make_verifier(1);
    let mut owner_a = pools[0].connection().await.unwrap();
    let mut owner_b = pools[1].connection().await.unwrap();
    for owner in [&mut owner_a, &mut owner_b] {
        assert!(owner
            .via_withdrawal_dal()
            .acquire_withdrawal_signer_lock(&wallet)
            .await
            .unwrap());
        owner
            .via_withdrawal_dal()
            .retire_incomplete_withdrawal_attempts(&wallet)
            .await
            .unwrap();
    }
    first.iteration(&mut owner_a, &wallet).await.unwrap();
    second.iteration(&mut owner_b, &wallet).await.unwrap();
    first.iteration(&mut owner_a, &wallet).await.unwrap();
    assert!(first.live.is_none());
    assert!(second.live.is_none());
    let finalized = owner_a
        .via_withdrawal_dal()
        .get_withdrawal_attempt(&wallet, &round)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finalized.state, WithdrawalAttemptState::Finalized);
    assert_eq!(finalized.content, failed.content);
    assert_eq!(finalized.public_signatures.as_ref(), Some(&durable_shares));
    let bytes = finalized.finalized_transaction.unwrap();
    let tx: Transaction = bitcoin::consensus::deserialize(&bytes).unwrap();
    if let Some(core) = &core {
        let accepted: Vec<serde_json::Value> = core
            .client
            .call("testmempoolaccept", &[serde_json::json!([hex::encode(&bytes)])])
            .unwrap();
        assert_eq!(accepted[0]["allowed"], true, "{accepted:?}");
    }
    let unsigned = op.get_unsigned_bridge_tx().tx;
    assert_eq!(tx.compute_txid(), unsigned.compute_txid());
    let mut stripped = tx.clone();
    for input in &mut stripped.input {
        input.witness = Witness::default();
    }
    assert_eq!(stripped, unsigned);
    for (input, message) in tx.input.iter().zip(op.get_message_to_sign()) {
        let witness: Vec<_> = input.witness.iter().collect();
        assert_eq!(witness.len(), 1);
        assert_eq!(witness[0].len(), 65);
        assert_eq!(witness[0][64], TapSighashType::All as u8);
        let signature =
            bitcoin::secp256k1::schnorr::Signature::from_slice(&witness[0][..64]).unwrap();
        secp.verify_schnorr(
            &signature,
            &bitcoin::secp256k1::Message::from_digest_slice(&message).unwrap(),
            &output_key,
        )
        .unwrap();
    }
    assert_eq!(btc.broadcasts.lock().as_slice(), &[bytes.clone()]);
    let mut changed = tx.clone();
    changed.output[0].value = Amount::from_sat(changed.output[0].value.to_sat() + 1);
    assert!(first
        .broadcast(
            &mut owner_a,
            &wallet,
            &op,
            &bitcoin::consensus::serialize(&changed),
        )
        .await
        .is_err());
    assert_eq!(btc.broadcasts.lock().as_slice(), &[bytes.clone()]);
    assert!(owner_a
        .via_withdrawal_dal()
        .observed_withdrawal_transaction_exists(&wallet, tx.compute_txid().as_byte_array())
        .await
        .unwrap());
    // An unconfirmed local observation blocks a new round, not rebroadcast of
    // these exact finalized bytes. A second signer must derive the same witness.
    assert!(!first
        .withdrawal_session
        .is_session_in_progress(&op)
        .await
        .unwrap());
    second.iteration(&mut owner_b, &wallet).await.unwrap();
    first.iteration(&mut owner_a, &wallet).await.unwrap();
    assert!(first.snapshot().await.unwrap().session_op.is_empty());
    {
        let broadcasts = btc.broadcasts.lock();
        assert!(broadcasts.len() >= 3);
        assert!(broadcasts.iter().all(|broadcast| broadcast == &bytes));
    }
    let other = owner_b
        .via_withdrawal_dal()
        .get_withdrawal_attempt(&wallet, &round)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other.finalized_transaction.as_ref(), Some(&bytes));
    owner_a
        .via_withdrawal_dal()
        .release_withdrawal_signer_lock(&wallet)
        .await
        .unwrap();
    owner_b
        .via_withdrawal_dal()
        .release_withdrawal_signer_lock(&wallet)
        .await
        .unwrap();
}

// TaprootBuilder supplies real commitment and control block.
// corepc bitcoind crate owns Core process: https://git.rust-bitcoin.org/rust-bitcoin/corepc/src/commit/eca585605f6fbf79a1c8ce81ce86e2e4205cdf31/bitcoind/src/lib.rs
fn heavy_parent(bridge: &bitcoin::Script, pushes: u32) -> (bitcoind::BitcoinD, Transaction) {
    use bitcoin::{
        hashes::sha256,
        opcodes::all,
        script::Builder,
        taproot::{LeafVersion, TaprootBuilder},
        Sequence, TxIn, Witness,
    };
    use serde_json::{json, Value};

    let executable = std::path::PathBuf::from(
        std::env::var_os("VIA_TEST_BITCOIND")
            .expect("VIA_TEST_BITCOIND must name the provisioned Core 28.2 executable"),
    );
    assert!(executable.is_absolute());
    assert_eq!(
        sha256::Hash::hash(&std::fs::read(&executable).unwrap()).to_string(),
        "79011d54248a80c76bf2865f91f540963c83634e170365139180372a4410f3b1"
    );
    let mut conf = bitcoind::Conf::default();
    conf.args = vec![
        "-regtest",
        "-fallbackfee=0.0001",
        "-txindex=1",
        "-dbcache=32",
        "-persistmempool=0",
        "-acceptnonstdtxn=0",
    ];
    let node = bitcoind::BitcoinD::with_conf(executable, &conf).unwrap();
    let address: String = node.client.call("getnewaddress", &[]).unwrap();
    let _: Value = node.client.call("generatetoaddress", &[json!(101), json!(address)]).unwrap();

    let secp = Secp256k1::new();
    let internal = SecretKey::from_slice(&[9; 32]).unwrap().x_only_public_key(&secp).0;
    let mut script = Builder::new().push_int(0).push_opcode(all::OP_IF);
    for index in 0..pushes {
        let mut bytes = [0u8; 500];
        for (part, chunk) in bytes.chunks_mut(32).enumerate() {
            let hash =
                sha256::Hash::hash(&[index.to_le_bytes(), (part as u32).to_le_bytes()].concat());
            chunk.copy_from_slice(&hash.as_byte_array()[..chunk.len()]);
        }
        script =
            script.push_slice(bitcoin::script::PushBytesBuf::try_from(bytes.to_vec()).unwrap());
    }
    let script = script.push_opcode(all::OP_ENDIF).push_int(1).into_script();
    let spend = TaprootBuilder::new()
        .add_leaf(0, script.clone())
        .unwrap()
        .finalize(&secp, internal)
        .unwrap();
    let address = Address::p2tr_tweaked(spend.output_key(), Network::Regtest);
    let funding: String =
        node.client.call("sendtoaddress", &[json!(address.to_string()), json!(0.01)]).unwrap();
    let mining: String = node.client.call("getnewaddress", &[]).unwrap();
    let _: Value = node.client.call("generatetoaddress", &[json!(1), json!(mining)]).unwrap();
    let raw: String = node.client.call("getrawtransaction", &[json!(funding)]).unwrap();
    let funding: Transaction = bitcoin::consensus::deserialize(&hex::decode(raw).unwrap()).unwrap();
    let vout =
        funding.output.iter().position(|out| out.script_pubkey == address.script_pubkey()).unwrap()
            as u32;
    let control =
        spend.control_block(&(script.clone(), LeafVersion::TapScript)).unwrap().serialize();
    let parent = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint { txid: funding.compute_txid(), vout },
            script_sig: bitcoin::ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[script.as_bytes(), control.as_slice()]),
        }],
        output: vec![
            TxOut { value: Amount::from_sat(100_000), script_pubkey: bridge.to_owned() },
            TxOut { value: Amount::from_sat(100_000), script_pubkey: bridge.to_owned() },
        ],
    };
    let bytes = bitcoin::consensus::serialize(&parent);
    assert!(parent.weight().to_wu() < 400_000);
    let accepted: Vec<Value> =
        node.client.call("testmempoolaccept", &[json!([hex::encode(&bytes)])]).unwrap();
    assert_eq!(accepted[0]["allowed"], true, "{accepted:?}");
    let id: String = node.client.call("sendrawtransaction", &[json!(hex::encode(bytes))]).unwrap();
    assert_eq!(id, parent.compute_txid().to_string());
    let _: Value = node.client.call("generatetoaddress", &[json!(1), json!(mining)]).unwrap();
    let mined: Value = node
        .client
        .call("getrawtransaction", &[json!(parent.compute_txid()), json!(true)])
        .unwrap();
    assert!(mined["confirmations"].as_u64().unwrap() >= 1);
    (node, parent)
}

fn fixture_wallets(signers: usize) -> (Vec<PrivateKey>, Vec<String>, SystemWallets) {
    let secp = Secp256k1::new();
    let keys: Vec<_> = (1..=signers)
        .map(|n| PrivateKey::new(SecretKey::from_slice(&[n as u8; 32]).unwrap(), Network::Bitcoin))
        .collect();
    let public: Vec<_> = keys.iter().map(|key| key.public_key(&secp).to_string()).collect();
    let addresses: Vec<_> = keys
        .iter()
        .map(|key| {
            Address::p2wpkh(
                &CompressedPublicKey::from_private_key(&secp, key).unwrap(),
                Network::Bitcoin,
            )
        })
        .collect();
    let aggregate = get_signer_with_merkle_root(&keys[0].to_wif(), public.clone(), None)
        .unwrap()
        .aggregated_pubkey();
    let output_key =
        bitcoin::XOnlyPublicKey::from_slice(&aggregate.x_only_public_key().0.serialize()).unwrap();
    let wallets = SystemWallets {
        sequencer: addresses[0].clone(),
        verifiers: addresses.clone(),
        governance: addresses[0].clone(),
        bridge: Address::p2tr_tweaked(
            TweakedPublicKey::dangerous_assume_tweaked(output_key),
            Network::Bitcoin,
        ),
    };
    (keys, public, wallets)
}

fn parent_utxos(parent: &Transaction) -> Vec<(OutPoint, TxOut)> {
    parent
        .output
        .iter()
        .enumerate()
        .map(|(vout, output)| {
            (OutPoint { txid: parent.compute_txid(), vout: vout as u32 }, output.clone())
        })
        .collect()
}

fn fixture_withdrawal(receiver: &Address, amount: u64) -> WithdrawalRequest {
    WithdrawalRequest {
        id: "08080808080808080000".into(),
        receiver: receiver.clone(),
        amount: Amount::from_sat(amount),
        l2_sender: zksync_types::Address::repeat_byte(7),
        l2_tx_hash: H256::repeat_byte(8),
        l2_tx_log_index: 0,
    }
}

#[tokio::test]
async fn bounded_signing_admission_refuses_before_reserving_real_requests_and_inputs() {
    use crate::{
        auth::Envelope,
        metrics::{SigningSizeLabel, SigningSizeStage, METRICS},
    };
    for (mode, inputs, accepted) in [
        (PeerMode::Reader, 150usize, true),
        (PeerMode::Reader, 400, false),
        (PeerMode::Writer, 150, true),
        (PeerMode::Writer, 400, false),
    ] {
        let (keys, public, wallets) = fixture_wallets(50);
        let wallet = wallets.bridge.script_pubkey().into_bytes();
        let parent = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: Vec::new(),
            output: vec![
                TxOut {
                    value: Amount::from_sat(100_000),
                    script_pubkey: wallets.bridge.script_pubkey(),
                };
                inputs
            ],
        };
        let btc = Arc::new(MockBitcoinOps {
            utxos: parent_utxos(&parent),
            transaction: Some(parent),
            fee_rate: 2,
            ..bitcoin_fixture()
        });
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let batch = CompleteWithdrawalBatch {
            batch_number: 1,
            chain_id: 270,
            network: Network::Bitcoin,
            protocol_version: 1,
            blob_id: "size-admission".into(),
            pubdata_hash: H256::repeat_byte(9),
            start_block: 1,
            end_block: 1,
            pubdata: vec![1, 2, 3],
            receipts: Vec::new(),
            nonpayable: Vec::new(),
            withdrawals: vec![fixture_withdrawal(
                &wallets.verifiers[1],
                inputs as u64 * 100_000 - 50_000,
            )],
        };
        seed(&pool, &wallets, &batch, btc.as_ref()).await;
        let coordinator = TestCoordinator::start(pool.clone(), btc, &public, &keys[0], mode).await;
        let config = &coordinator.config;
        let session = &coordinator.session;
        let binding = Binding {
            version: 1,
            sequencer_version: via_verifier_types::protocol_version::get_sequencer_version()
                .to_string(),
            principal: public[0].clone(),
            audience: public[0].clone(),
            method: "POST".into(),
            target: "/session/new".into(),
            round: [0; 32],
            content: [0; 32],
            challenge: random_id(),
            timestamp: chrono::Utc::now().timestamp(),
            status: 0,
        };
        let envelope = Envelope::sign(binding.clone(), Vec::new(), &keys[0].inner).unwrap();
        let label = SigningSizeLabel { stage: SigningSizeStage::Admission };
        let before = METRICS.signing_size_refusals[&label].get();
        if mode == PeerMode::Writer && inputs == 150 {
            for hold in [
                "UPDATE via_btc_reader SET replay_ready=FALSE",
                "UPDATE via_votable_transactions SET proof_l1_block_hash='orphan'",
            ] {
                let mut db = pool.connection().await.unwrap();
                sqlx::query(hold).execute(db.conn()).await.unwrap();
                let mut held_binding = binding.clone();
                held_binding.challenge = random_id();
                let held = Envelope::sign(held_binding, Vec::new(), &keys[0].inner).unwrap();
                let _response = reqwest::Client::new()
                    .post(format!("{}/session/new", config.coordinator_http_url))
                    .json(&held).send().await.unwrap();
                assert_eq!(withdrawal_reservation_counts(&mut db).await, (0, 0, 0));
                assert!(session.read().await.session_op.is_none());
                sqlx::query("UPDATE via_btc_reader SET replay_ready=TRUE")
                    .execute(db.conn()).await.unwrap();
                sqlx::query("UPDATE via_votable_transactions SET proof_l1_block_hash=(SELECT hash FROM via_btc_headers WHERE number=1)")
                    .execute(db.conn()).await.unwrap();
            }
        }
        let response = reqwest::Client::new()
            .post(format!("{}/session/new", config.coordinator_http_url))
            .json(&envelope)
            .send()
            .await
            .unwrap();
        let status = response.status();
        assert_eq!(status.as_u16(), if accepted { 200 } else { 413 });
        let reply: Envelope = response.json().await.unwrap();
        reply
            .verify_response(
                &binding,
                &keys[0].public_key(&Secp256k1::new()).inner,
                status.as_u16(),
                chrono::Utc::now().timestamp(),
                30,
            )
            .unwrap();
        let mut db = pool.connection().await.unwrap();
        let counts = withdrawal_reservation_counts(&mut db).await;
        if accepted {
            assert_eq!(counts, (1, 1, inputs as i64));
            assert!(session.read().await.session_op.is_some());
        } else {
            assert_eq!(counts, (0, 0, 0));
            assert!(session.read().await.session_op.is_none());
            let error: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
            assert_eq!(error["error"], "signing_round_too_large");
            assert!(METRICS.signing_size_refusals[&label].get() > before);
            assert!(db
                .via_withdrawal_dal()
                .held_withdrawal_inputs(&wallet)
                .await
                .unwrap()
                .is_empty());
        }
    }
}

struct TestCoordinator {
    config: ViaVerifierConfig,
    session: Arc<tokio::sync::RwLock<crate::types::SigningSession>>,
    server: tokio::task::JoinHandle<()>,
    faults: Arc<ReplyFaults>,
}

impl TestCoordinator {
    async fn start(
        pool: ConnectionPool<Verifier>,
        btc: Arc<dyn BitcoinOps>,
        public: &[String],
        key: &PrivateKey,
        mode: PeerMode,
    ) -> Self {
        Self::start_routes(pool, btc, public, key, mode, ProbeRoutes::Normal).await
    }

    async fn start_routes(
        pool: ConnectionPool<Verifier>,
        btc: Arc<dyn BitcoinOps>,
        public: &[String],
        key: &PrivateKey,
        mode: PeerMode,
        routes: ProbeRoutes,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = ViaVerifierConfig::for_tests();
        config.coordinator_http_url = format!("http://{}", listener.local_addr().unwrap());
        config.coordinator_public_key = public[0].clone();
        config.withdrawal_signing_enabled = true;
        config.withdrawal_compact_transport_writes_enabled = mode == PeerMode::Writer;
        let api = crate::coordinator::api_decl::RestApi::new(
            config.clone(),
            pool,
            btc.clone(),
            withdrawal_client(),
            public.to_vec(),
            key.to_wif(),
        )
        .unwrap();
        let session = api.state.signing_session.clone();
        let faults = Arc::new(ReplyFaults::default());
        let router = match routes {
            ProbeRoutes::Normal if mode == PeerMode::Legacy => super::legacy::router(api, btc),
            ProbeRoutes::Normal => api.into_router(),
            ProbeRoutes::Bounded => bounded_probe_router(api, faults.clone()),
            ProbeRoutes::Chunked => chunked_probe_router(api),
            ProbeRoutes::Corrupt => corrupt_snapshot_router(api),
        };
        let response_faults = faults.clone();
        let router = router.layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let faults = response_faults.clone();
                async move {
                    use std::sync::atomic::Ordering;
                    let bit = match request.uri().path() {
                        "/session/nonce" => {
                            faults.nonces.fetch_add(1, Ordering::SeqCst);
                            1
                        }
                        "/session/signature" => {
                            faults.shares.fetch_add(1, Ordering::SeqCst);
                            2
                        }
                        _ => 0,
                    };
                    let response = next.run(request).await;
                    if response.status().is_success()
                        && faults.mask.fetch_and(!bit, Ordering::SeqCst) & bit != 0
                    {
                        return axum::response::Response::new(axum::body::Body::from(
                            "lost acknowledgment",
                        ));
                    }
                    response
                }
            },
        ));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { config, session, server, faults }
    }
}

impl Drop for TestCoordinator {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PeerMode {
    Legacy,
    Reader,
    Writer,
}

#[tokio::test]
async fn bounded_signing_distinct_coordinator_and_signer_modes_sign_and_recover_over_http() {
    for coordinator in [PeerMode::Legacy, PeerMode::Reader, PeerMode::Writer] {
        for signer in [PeerMode::Reader, PeerMode::Writer] {
            run_actual_iterations(false, coordinator, [signer; 2], false, false).await;
        }
    }
}

impl TestCoordinator {
    async fn call(
        &self,
        key: &PrivateKey,
        method: &str,
        target: &str,
        version: TransportVersion,
        body: Vec<u8>,
    ) -> (Binding, reqwest::Response) {
        let session = self.session.read().await;
        let binding = Binding {
            version: 1,
            sequencer_version: via_verifier_types::protocol_version::get_sequencer_version()
                .to_string(),
            principal: key.public_key(&Secp256k1::new()).to_string(),
            audience: self.config.coordinator_public_key.clone(),
            method: method.to_owned(),
            target: target.to_owned(),
            round: if method == "GET" { [0; 32] } else { session.round_id },
            content: if method == "GET" { [0; 32] } else { session.content_hash },
            challenge: random_id(),
            timestamp: chrono::Utc::now().timestamp(),
            status: 0,
        };
        drop(session);
        let envelope = Envelope::sign(binding.clone(), body, &key.inner).unwrap();
        let bytes = transport::encode_envelope(envelope, version, MAX_BODY).unwrap();
        let response = reqwest::Client::new()
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
                format!("{}{}", self.config.coordinator_http_url, target),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
            .send()
            .await
            .map_err(|error| error.without_url())
            .unwrap();
        (binding, response)
    }
}

async fn verified_reply(
    request: &Binding,
    response: reqwest::Response,
    coordinator: &str,
) -> (Envelope, TransportVersion, u16, usize) {
    let status = response.status().as_u16();
    let bytes = response.bytes().await.unwrap();
    assert!(bytes.len() <= MAX_BODY);
    let (envelope, version) = transport::decode_envelope(&bytes).unwrap();
    envelope
        .verify_response(
            request,
            &PublicKey::from_str(coordinator).unwrap(),
            status,
            chrono::Utc::now().timestamp(),
            30,
        )
        .unwrap();
    (envelope, version, status, bytes.len())
}

async fn assert_unservable_reply(
    request: &Binding,
    response: reqwest::Response,
    coordinator: &str,
) {
    let (reply, _, status, _) = verified_reply(request, response, coordinator).await;
    assert_eq!(status, 503);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&reply.body).unwrap()["error"],
        "signing_round_unservable"
    );
}

#[tokio::test]
async fn bounded_signing_historical_parent_bytes_recover_compactly_and_keep_legacy_holds() {
    run_actual_iterations(true, PeerMode::Writer, [PeerMode::Writer; 2], true, false).await;
}

#[tokio::test]
async fn bounded_signing_fitting_historical_round_serves_by_actual_size_over_http() {
    use via_musig2::types::{TransactionBuilderConfig, TransactionOutput};

    let (keys, public, wallets) = fixture_wallets(2);
    let (_core, parent) = heavy_parent(&wallets.bridge.script_pubkey(), 590);
    assert_eq!(bitcoin::consensus::serialize(&parent).len(), 296_953);
    let utxos = parent_utxos(&parent);
    let btc = Arc::new(MockBitcoinOps {
        utxos: utxos.clone(),
        transaction: Some(parent.clone()),
        fee_rate: 2,
        ..bitcoin_fixture()
    });
    let builder = TransactionBuilder::new(btc.clone()).unwrap();
    let unsigned = builder
        .build_fixed_bridge_tx(
            &utxos,
            vec![TransactionOutput {
                output: TxOut {
                    value: Amount::from_sat(150_000),
                    script_pubkey: wallets.verifiers[1].script_pubkey(),
                },
                op_return_data: Some(hex::decode("08080808080808080000").unwrap()),
            }],
            &TransactionBuilderConfig::withdrawal(wallets.bridge.clone()),
            2,
        )
        .unwrap();
    let messages = builder.get_tr_sighashes(&unsigned).unwrap();
    assert_eq!(messages.len(), 2);
    let operation = SessionOperation::Withdrawal(unsigned, messages, vec![parent]);
    let proposal = via_btc_client::traits::Serializable::to_bytes(&operation);
    let content = bincode::serialize(&AdmittedContent {
        proposal: proposal.clone(),
        network: Network::Bitcoin,
        chain_id: 270,
        wallet: wallets.bridge.script_pubkey().into_bytes(),
        participants: public.clone(),
        tweak: None,
        expected: vec![fixture_withdrawal(&wallets.verifiers[1], 150_000)],
    })
    .unwrap();
    let mut retained = crate::types::SigningSession {
        round_id: [1; 32],
        content_hash: crate::auth::digest(&content),
        authorized_content: content.clone(),
        session_op: Some(operation),
        created_at: 1,
        ..Default::default()
    };
    let mut signer = get_signer_with_merkle_root(&keys[0].to_wif(), public.clone(), None).unwrap();
    let nonce = signer.start_signing_session(vec![7; 32]).unwrap();
    let signature = musig2::PartialSignature::from_slice(&[1; 32]).unwrap();
    for input in 0..2 {
        for slot in 0..2 {
            retained.received_nonces.entry(input).or_default().insert(slot, nonce.clone());
            retained.received_sigs.entry(input).or_default().insert(slot, signature);
        }
    }
    assert!(transport::check_round_size_in(&retained, 2, TransportVersion::Legacy).is_err());
    // Repeated parent evidence isolates inner encoding overflow from final-envelope overflow.
    let mut oversized = retained.clone();
    let SessionOperation::Withdrawal(_, _, parents) = oversized.session_op.as_mut().unwrap();
    let parent = parents[0].clone();
    parents.resize(22, parent);
    let mut admitted: AdmittedContent = bincode::deserialize(&content).unwrap();
    admitted.proposal =
        via_btc_client::traits::Serializable::to_bytes(oversized.session_op.as_ref().unwrap());
    oversized.authorized_content = bincode::serialize(&admitted).unwrap();
    oversized.content_hash = crate::auth::digest(&oversized.authorized_content);
    for version in [TransportVersion::Legacy, TransportVersion::Compact] {
        let snapshot = transport::snapshot(&oversized, 2, version).unwrap();
        assert!(transport::encode_snapshot(snapshot, version).is_err());
    }
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    seed_reader(&pool, &wallets, btc.as_ref()).await;
    for mode in [PeerMode::Reader, PeerMode::Writer] {
        let server =
            TestCoordinator::start(pool.clone(), btc.clone(), &public, &keys[0], mode).await;
        *server.session.write().await = retained.clone();
        for target in ["/session/", "/session/?transport_version=2"] {
            let expected = if mode == PeerMode::Writer && target.ends_with("=2") {
                TransportVersion::Compact
            } else {
                TransportVersion::Legacy
            };
            let (request, response) =
                server.call(&keys[1], "GET", target, TransportVersion::Legacy, Vec::new()).await;
            let (envelope, version, status, len) =
                verified_reply(&request, response, &public[0]).await;
            assert_eq!(status, 200, "{mode:?}, {target}");
            assert_eq!(version, expected);
            if version == TransportVersion::Legacy {
                assert!((6_000_000..MAX_BODY).contains(&len));
                let old: super::legacy::SigningSessionResponse =
                    serde_json::from_slice(&envelope.body).unwrap();
                assert_eq!(old.authorized_content, content);
                assert_eq!(old.session_op, proposal);
            }
            let snapshot = transport::decode_snapshot(&envelope.body, version).unwrap();
            assert_eq!(snapshot.authorized_content, content);
            assert_eq!(snapshot.content_hash, retained.content_hash);
            assert_eq!(snapshot.round_id, retained.round_id);
            assert_eq!(snapshot.session_op, proposal);
            assert_eq!(snapshot.required_signers, 2);
            assert_eq!(snapshot.nonces.len(), 2);
            assert_eq!(snapshot.signatures.len(), 2);
            for input in 0..2 {
                assert_eq!(snapshot.nonces[&input].len(), 2);
                assert_eq!(snapshot.signatures[&input].len(), 2);
                assert_eq!(snapshot.received_nonces[&input], 2);
                assert_eq!(snapshot.received_partial_signatures[&input], 2);
            }
            assert_eq!(server.session.read().await.authorized_content, content);
        }
        *server.session.write().await = oversized.clone();
        for target in ["/session/", "/session/?transport_version=2"] {
            let (request, response) =
                server.call(&keys[1], "GET", target, TransportVersion::Legacy, Vec::new()).await;
            assert_unservable_reply(&request, response, &public[0]).await;
            assert_eq!(
                server.session.read().await.authorized_content,
                oversized.authorized_content
            );
        }
    }
}

#[derive(Default)]
struct ReplyFaults {
    mask: std::sync::atomic::AtomicU8,
    nonces: std::sync::atomic::AtomicUsize,
    shares: std::sync::atomic::AtomicUsize,
    responses: std::sync::atomic::AtomicUsize,
}

#[tokio::test]
async fn bounded_signing_lost_ack_does_not_reencode_replay_or_regenerate_secrets() {
    run_actual_iterations(false, PeerMode::Writer, [PeerMode::Writer; 2], false, true).await;
}

#[tokio::test]
async fn bounded_signing_http_selects_each_reply_and_binds_the_offer() {
    for mode in [PeerMode::Reader, PeerMode::Writer] {
        let network = TestNetwork::empty().await;
        let server = network.coordinator(mode, ProbeRoutes::Normal).await;
        let (keys, public) = (&network.keys, &network.public);
        for request_version in [TransportVersion::Legacy, TransportVersion::Compact] {
            for offer in ["", "?transport_version=2"] {
                let expected = if mode == PeerMode::Writer && !offer.is_empty() {
                    TransportVersion::Compact
                } else {
                    TransportVersion::Legacy
                };
                for (method, path, status) in [
                    ("GET", "/session/", 200),
                    ("POST", "/session/new", 401),
                    ("POST", "/session/nonce", 400),
                    ("POST", "/session/signature", 400),
                ] {
                    let (request, response) = server
                        .call(
                            &keys[1],
                            method,
                            &format!("{path}{offer}"),
                            request_version,
                            b"{}".to_vec(),
                        )
                        .await;
                    let (_, version, actual_status, _) =
                        verified_reply(&request, response, &public[0]).await;
                    assert_eq!(version, expected);
                    assert_eq!(actual_status, status);
                }
            }
        }
        for offer in [
            "transport_version=1",
            "transport_version=3",
            "transport_version=x",
            "transport_version=",
            "transport_version=2&transport_version=2",
        ] {
            let (request, response) = server
                .call(
                    &keys[1],
                    "POST",
                    &format!("/session/nonce?{offer}"),
                    TransportVersion::Compact,
                    b"{}".to_vec(),
                )
                .await;
            let (reply, version, status, _) = verified_reply(&request, response, &public[0]).await;
            assert_eq!(status, 400);
            assert_eq!(version, TransportVersion::Legacy);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&reply.body).unwrap()["error"],
                "invalid_transport_encoding"
            );
        }
        let (binding, response) = server
            .call(
                &keys[1],
                "GET",
                &format!("/session/?transport_version={}2", "0".repeat(65_300)),
                TransportVersion::Legacy,
                Vec::new(),
            )
            .await;
        assert_eq!(response.status().as_u16(), 413);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["error"],
            "request_too_large"
        );
        let envelope = Envelope::sign(binding, Vec::new(), &keys[1].inner).unwrap();
        let mut malformed: serde_json::Value = serde_json::from_slice(
            &transport::encode_envelope(envelope, TransportVersion::Compact, MAX_BODY).unwrap(),
        )
        .unwrap();
        malformed["body"] = serde_json::json!("!");
        let response = reqwest::Client::new()
            .get(format!("{}/session/", server.config.coordinator_http_url))
            .json(&malformed)
            .send()
            .await
            .map_err(|error| error.without_url())
            .unwrap();
        assert_eq!(response.status().as_u16(), 400);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["error"],
            "invalid_transport_encoding"
        );
        // No re-signing: offer is part of authentication target.
        let (mut binding, response) =
            server.call(&keys[1], "GET", "/session/", TransportVersion::Legacy, Vec::new()).await;
        assert_eq!(response.status().as_u16(), 200);
        binding.challenge = random_id();
        let envelope = Envelope::sign(binding, Vec::new(), &keys[1].inner).unwrap();
        let response = reqwest::Client::new()
            .get(format!("{}/session/?transport_version=2", server.config.coordinator_http_url,))
            .json(&envelope)
            .send()
            .await
            .map_err(|error| error.without_url())
            .unwrap();
        assert_eq!(response.status().as_u16(), 401);
        assert!(server.session.read().await.session_op.is_none());
        let mut db = network.pool.connection().await.unwrap();
        let counts = withdrawal_reservation_counts(&mut db).await;
        assert_eq!(counts, (0, 0, 0));
    }
}

#[derive(Clone, Copy)]
enum ProbeRoutes {
    Normal,
    Bounded,
    Chunked,
    Corrupt,
}

#[derive(Deserialize, Serialize)]
struct Probe {
    final_bytes: usize,
    status: u16,
}

fn probe_body(request: &Binding, version: TransportVersion, probe: &Probe) -> Vec<u8> {
    let binding = transport::response_binding(
        request,
        &request.audience,
        probe.status,
        chrono::Utc::now().timestamp(),
    );
    let mut body = br#"{"probe":true}"#.to_vec();
    let encoded = transport::encode_envelope(
        Envelope { binding: binding.clone(), body: body.clone(), signature: "A".repeat(88) },
        version,
        MAX_BODY + 16,
    )
    .unwrap();
    match version {
        TransportVersion::Legacy => {
            let extra = probe.final_bytes - encoded.len();
            // Space costs three numeric-array bytes, tab two.
            let tabs = match extra % 3 {
                0 => 0,
                1 => 2,
                _ => 1,
            };
            body.resize(body.len() + (extra - 2 * tabs) / 3, b' ');
            body.extend(std::iter::repeat(b'\t').take(tabs));
        }
        TransportVersion::Compact => {
            let overhead = transport::encode_envelope(
                Envelope { binding, body: Vec::new(), signature: "A".repeat(88) },
                version,
                MAX_BODY,
            )
            .unwrap()
            .len();
            assert_eq!((probe.final_bytes - overhead) % 4, 0);
            body.resize((probe.final_bytes - overhead) / 4 * 3, b' ');
        }
    }
    body
}

fn bounded_probe_router(
    api: crate::coordinator::api_decl::RestApi,
    faults: Arc<ReplyFaults>,
) -> axum::Router {
    use crate::coordinator::auth_middleware::{auth_middleware, AuthenticatedRequest};
    let state = Arc::new(api);
    let router = axum::Router::new().route(
        "/session/",
        axum::routing::get(
            move |axum::Extension(auth): axum::Extension<AuthenticatedRequest>,
                  axum::Json(probe): axum::Json<Probe>| {
                let faults = faults.clone();
                async move {
                    faults.responses.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let body = probe_body(&auth.binding, auth.transport, &probe);
                    axum::response::Response::builder()
                        .status(probe.status)
                        .body(axum::body::Body::from(body))
                        .unwrap()
                }
            },
        ),
    );
    router
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state)
}

fn chunked_probe_router(api: crate::coordinator::api_decl::RestApi) -> axum::Router {
    let key = api.response_key;
    axum::Router::new().route(
        "/session/",
        axum::routing::get(move |axum::Json(request): axum::Json<Envelope>| async move {
            let probe: Probe = serde_json::from_slice(&request.body).unwrap();
            let binding = transport::response_binding(
                &request.binding,
                &request.binding.audience,
                200,
                chrono::Utc::now().timestamp(),
            );
            let version =
                TransportVersion::negotiate(&request.binding.target.parse().unwrap(), true)
                    .unwrap();
            let envelope = Envelope::sign(binding, b"{}".to_vec(), &key).unwrap();
            let mut bytes = transport::encode_envelope(envelope, version, MAX_BODY).unwrap();
            // Whitespace outside JSON is unsigned framing, yet counts against body limit.
            // Body streams without Content-Length.
            bytes.resize(probe.final_bytes, b' ');
            let chunks: Vec<_> =
                bytes.chunks(4096).map(|chunk| Ok::<_, std::io::Error>(chunk.to_vec())).collect();
            axum::response::Response::new(axum::body::Body::from_stream(tokio_stream::iter(chunks)))
        }),
    )
}

#[tokio::test]
async fn bounded_signing_actual_http_final_limit_and_signed_size_fallback() {
    for mode in [PeerMode::Reader, PeerMode::Writer] {
        let network = TestNetwork::empty().await;
        let server = network.coordinator(mode, ProbeRoutes::Bounded).await;
        let (keys, public) = (&network.keys, &network.public);
        let version = if mode == PeerMode::Writer {
            TransportVersion::Compact
        } else {
            TransportVersion::Legacy
        };
        for status in [200, 400] {
            for final_bytes in [MAX_BODY - 1, MAX_BODY, MAX_BODY + 1] {
                let mut target = "/session/".to_owned();
                if version == TransportVersion::Compact {
                    // Fixed-size Base64 IDs make H independent of random challenge values.
                    for zeros in 0..4 {
                        let candidate =
                            format!("/session/?transport_version={}2", "0".repeat(zeros));
                        let binding = Binding {
                            version: 1,
                            sequencer_version:
                                via_verifier_types::protocol_version::get_sequencer_version()
                                    .to_string(),
                            principal: public[0].clone(),
                            audience: public[1].clone(),
                            method: "GET".into(),
                            target: candidate.clone(),
                            round: [0; 32],
                            content: [0; 32],
                            challenge: [0; 32],
                            timestamp: chrono::Utc::now().timestamp(),
                            status,
                        };
                        let overhead = transport::encode_envelope(
                            Envelope { binding, body: Vec::new(), signature: "A".repeat(88) },
                            version,
                            MAX_BODY,
                        )
                        .unwrap()
                        .len();
                        if (final_bytes - overhead) % 4 == 0 {
                            target = candidate;
                            break;
                        }
                    }
                    assert!(target.contains('?'));
                }
                let probe = Probe { final_bytes, status };
                let (request, response) = server
                    .call(
                        &keys[1],
                        "GET",
                        &target,
                        TransportVersion::Legacy,
                        serde_json::to_vec(&probe).unwrap(),
                    )
                    .await;
                let (reply, actual_version, actual_status, len) =
                    verified_reply(&request, response, &public[0]).await;
                assert_eq!(actual_version, version);
                if final_bytes <= MAX_BODY {
                    assert_eq!(len, final_bytes);
                    assert_eq!(actual_status, status);
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&reply.body).unwrap()["probe"],
                        true
                    );
                } else {
                    assert_eq!(actual_status, 503);
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&reply.body).unwrap()["error"],
                        "response_too_large",
                    );
                }
            }
        }
        assert_eq!(server.faults.responses.load(std::sync::atomic::Ordering::SeqCst), 6);
    }
}

#[tokio::test]
async fn bounded_signing_actual_client_counts_chunked_bytes_before_decode() {
    let network = TestNetwork::empty().await;
    let server = network.coordinator(PeerMode::Writer, ProbeRoutes::Chunked).await;
    let verifier = network.writer(&server);
    let (keys, public) = (&network.keys, &network.public);
    let (request, response) = server
        .call(
            &keys[1],
            "GET",
            "/session/?transport_version=2",
            TransportVersion::Legacy,
            serde_json::to_vec(&Probe { final_bytes: MAX_BODY - 1, status: 200 }).unwrap(),
        )
        .await;
    assert!(response.headers().get(reqwest::header::CONTENT_LENGTH).is_none());
    assert_eq!(response.headers().get(reqwest::header::TRANSFER_ENCODING).unwrap(), "chunked");
    verified_reply(&request, response, &public[0]).await;
    for final_bytes in [MAX_BODY - 1, MAX_BODY, MAX_BODY + 1] {
        let body = serde_json::to_vec(&Probe { final_bytes, status: 200 }).unwrap();
        let result = verifier
            .request("GET", "/session/", [0; 32], [0; 32], body, TransportVersion::Legacy)
            .await;
        if final_bytes <= MAX_BODY {
            assert_eq!(result.unwrap().0, b"{}");
        } else {
            assert!(result.unwrap_err().to_string().contains("Oversized coordinator response"));
        }
    }
}

#[tokio::test]
async fn bounded_signing_writer_disable_and_old_binary_rebootstrap_preserve_authentication() {
    use std::sync::atomic::Ordering;
    let network = TestNetwork::empty().await;
    let writer = network.coordinator(PeerMode::Writer, ProbeRoutes::Normal).await;
    let reader = network.coordinator(PeerMode::Reader, ProbeRoutes::Normal).await;
    let old = network.coordinator(PeerMode::Legacy, ProbeRoutes::Normal).await;
    let mut client = network.writer(&writer);
    let snapshot = client.snapshot().await.unwrap();
    assert_eq!(snapshot.transport, TransportVersion::Compact);
    client.verifier_config.coordinator_http_url = reader.config.coordinator_http_url.clone();
    let error = client
        .request("POST", "/session/nonce", [0; 32], [0; 32], b"{}".to_vec(), snapshot.transport)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("No active round"));
    assert_eq!(client.snapshot().await.unwrap().transport, TransportVersion::Legacy);
    client.verifier_config.coordinator_http_url = old.config.coordinator_http_url.clone();
    let before = old.faults.nonces.load(Ordering::SeqCst);
    assert!(client
        .request("POST", "/session/nonce", [0; 32], [0; 32], b"{}".to_vec(), snapshot.transport,)
        .await
        .is_err());
    assert_eq!(old.faults.nonces.load(Ordering::SeqCst), before + 1);
    assert_eq!(client.snapshot().await.unwrap().transport, TransportVersion::Legacy);
    assert_eq!(old.faults.nonces.load(Ordering::SeqCst), before + 1);
    assert!(client.live.is_none());
}

fn corrupt_snapshot_router(api: crate::coordinator::api_decl::RestApi) -> axum::Router {
    use base64::Engine;
    let key = api.response_key;
    axum::Router::new().route(
        "/session/",
        axum::routing::get(move |axum::Json(request): axum::Json<Envelope>| async move {
            let body = serde_json::to_vec(&serde_json::json!({
                "transport_version": 2,
                "round_id": base64::engine::general_purpose::STANDARD.encode([1; 32]),
                "content_hash": base64::engine::general_purpose::STANDARD.encode([2; 32]),
                "authorized_content": "AA==",
                "nonces": {}, "signatures": {}, "required_signers": 2,
                "received_nonces": {}, "received_partial_signatures": {}, "created_at": 1,
            }))
            .unwrap();
            let binding = transport::response_binding(
                &request.binding,
                &request.binding.audience,
                200,
                chrono::Utc::now().timestamp(),
            );
            let envelope = Envelope::sign(binding, body, &key).unwrap();
            axum::body::Body::from(
                transport::encode_envelope(envelope, TransportVersion::Compact, MAX_BODY).unwrap(),
            )
        }),
    )
}

#[tokio::test]
async fn bounded_signing_digest_precedes_bincode_and_real_signer_admission() {
    let network = TestNetwork::empty().await;
    let server = network.coordinator(PeerMode::Writer, ProbeRoutes::Corrupt).await;
    let mut client = network.writer(&server);
    let (pool, wallets) = (&network.pool, &network.wallets);
    let mut owner = pool.connection().await.unwrap();
    let mut tx = owner.start_transaction().await.unwrap();
    via_btc_storage::ReaderDal::new(&mut tx)
        .insert_wallets(
            &SystemWalletsDetails::try_from(wallets.clone()).unwrap(),
            via_btc_storage::WalletSource::Unbound(1),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let wallet = wallets.bridge.script_pubkey().into_bytes();
    assert!(owner.via_withdrawal_dal().acquire_withdrawal_signer_lock(&wallet).await.unwrap());
    let error = client.iteration(&mut owner, &wallet).await.unwrap_err();
    assert!(error.to_string().contains("Invalid proposal commitment"), "{error:#}");
    assert!(client.live.is_none());
    let counts = withdrawal_reservation_counts(&mut owner).await;
    assert_eq!(counts, (0, 0, 0));
    owner.via_withdrawal_dal().release_withdrawal_signer_lock(&wallet).await.unwrap();
}

struct TestNetwork {
    keys: Vec<PrivateKey>,
    public: Vec<String>,
    wallets: SystemWallets,
    pool: ConnectionPool<Verifier>,
    btc: Arc<MockBitcoinOps>,
}

impl TestNetwork {
    async fn empty() -> Self {
        let (keys, public, wallets) = fixture_wallets(2);
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let btc = Arc::new(bitcoin_fixture());
        seed_reader(&pool, &wallets, btc.as_ref()).await;
        Self { keys, public, wallets, pool, btc }
    }

    async fn coordinator(&self, mode: PeerMode, routes: ProbeRoutes) -> TestCoordinator {
        TestCoordinator::start_routes(
            self.pool.clone(),
            self.btc.clone(),
            &self.public,
            &self.keys[0],
            mode,
            routes,
        )
        .await
    }

    fn writer(&self, coordinator: &TestCoordinator) -> ViaWithdrawalVerifier {
        let mut config = coordinator.config.clone();
        config.withdrawal_compact_transport_writes_enabled = true;
        config.role = ViaNodeRole::Verifier;
        config.wallet_address = self.wallets.verifiers[1].to_string();
        ViaWithdrawalVerifier::new(
            config,
            ViaWallet::new(self.wallets.verifiers[1].to_string(), self.keys[1].to_wif()),
            self.pool.clone(),
            self.btc.clone(),
            withdrawal_client(),
            ViaBridgeConfig {
                verifiers_pub_keys: self.public.clone(),
                bridge_address: self.wallets.bridge.to_string(),
            },
        )
        .unwrap()
    }
}
