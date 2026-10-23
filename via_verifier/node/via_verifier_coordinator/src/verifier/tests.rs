use bitcoin::{
    absolute, hashes::Hash, key::TweakedPublicKey, transaction, Address, Amount, Block, BlockHash,
    CompressedPublicKey, Network, OutPoint, PrivateKey, TxOut, Txid,
};
use bitcoincore_rpc::json::GetBlockStatsResult;
use parking_lot::Mutex;
use via_btc_client::{
    inscriber::test_utils::{MockBitcoinOps, MockBitcoinOpsConfig},
    types::BitcoinClientResult,
};
use via_verifier_types::withdrawal::{CompleteWithdrawalBatch, WithdrawalRequest};
use zksync_db_connection::instrument::InstrumentExt;
use zksync_types::{via_wallet::SystemWalletsDetails, H256};

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

fn withdrawal_client() -> WithdrawalClient {
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

async fn seed(
    pool: &ConnectionPool<Verifier>,
    wallets: &SystemWallets,
    batch: &CompleteWithdrawalBatch,
) {
    let mut db = pool.connection().await.unwrap();
    db.via_wallet_dal()
        .insert_wallets(&SystemWalletsDetails::try_from(wallets.clone()).unwrap(), 1)
        .await
        .unwrap();
    db.via_indexer_dal()
        .init_indexer_metadata("via_btc_watch", 1)
        .await
        .unwrap();
    let proof = H256::repeat_byte(4);
    let source_block = BlockHash::from_byte_array([5; 32]);
    db.via_l1_block_dal()
        .insert_l1_block(1, source_block.to_string())
        .await
        .unwrap();
    db.via_votes_dal()
        .insert_votable_transaction(
            1,
            H256::repeat_byte(3),
            H256::zero(),
            "fixture".into(),
            proof,
            "proof".into(),
            "pubdata".into(),
            batch.blob_id.clone(),
            Some((1, source_block)),
        )
        .await
        .unwrap();
    let id = db
        .via_votes_dal()
        .verify_votable_transaction(1, proof, true)
        .await
        .unwrap();
    for verifier in &wallets.verifiers {
        db.via_votes_dal()
            .insert_vote(id, &verifier.to_string(), true, Some((1, source_block)))
            .await
            .unwrap();
    }
    assert!(db
        .via_votes_dal()
        .finalize_transaction_if_needed(id, 1.0, 2)
        .await
        .unwrap());
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

// Requires the same isolated PostgreSQL test template as verifier_dal tests.
#[tokio::test]
async fn actual_iterations_retry_exact_nonces_and_recover_public_only_over_http() {
    run_actual_iterations(false).await;
}

#[tokio::test]
async fn bounded_signing_valid_heavy_parent_downloads_signs_and_recovers() {
    run_actual_iterations(true).await;
}

async fn run_actual_iterations(heavy: bool) {
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
        let (node, parent) = heavy_parent(&bridge.script_pubkey());
        (Some(node), parent)
    } else {
        (None, parent)
    };
    let btc = Arc::new(RecordingBitcoin {
        inner: MockBitcoinOps::new(MockBitcoinOpsConfig {
            utxos: parent
                .output
                .iter()
                .enumerate()
                .map(|(vout, output)| {
                    (
                        OutPoint {
                            txid: parent.compute_txid(),
                            vout: vout as u32,
                        },
                        output.clone(),
                    )
                })
                .collect(),
            transaction: Some(parent),
            fee_rate: 2,
            block_height: 1,
            ..Default::default()
        }),
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
        withdrawals: vec![WithdrawalRequest {
            id: "08080808080808080000".into(),
            receiver: addresses[1].clone(),
            amount: Amount::from_sat(150_000),
            l2_sender: zksync_types::Address::repeat_byte(7),
            l2_tx_hash: H256::repeat_byte(8),
            l2_tx_log_index: 0,
        }],
    };
    let pools = [
        ConnectionPool::<Verifier>::test_pool().await,
        ConnectionPool::<Verifier>::test_pool().await,
    ];
    for pool in &pools {
        seed(pool, &wallets, &batch).await;
    }
    let coordinator =
        TestCoordinator::start(pools[0].clone(), btc.clone(), &public, &keys[0]).await;
    let config = coordinator.config.clone();
    let session = coordinator.session.clone();
    let bridge_config = ViaBridgeConfig {
        verifiers_pub_keys: public.clone(),
        bridge_address: bridge.to_string(),
    };
    let make_verifier = |index: usize| {
        let mut config = config.clone();
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
            ViaBtcWatchConfig::for_tests(),
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
    if heavy {
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
    if heavy {
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
    assert!(op.get_parent_transactions().is_empty());
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
    first.iteration(&mut owner_a, &wallet).await.unwrap();
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
    assert!(btc.broadcasts.lock().is_empty());

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
    let bytes = finalized.finalized_transaction.unwrap();
    let tx: Transaction = bitcoin::consensus::deserialize(&bytes).unwrap();
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
    let broadcasts = btc.broadcasts.lock();
    assert!(broadcasts.len() >= 3);
    assert!(broadcasts.iter().all(|broadcast| broadcast == &bytes));
    drop(broadcasts);
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

// The process owner is the established corepc library; this scenario stays local to
// this test file. TaprootBuilder supplies the actual commitment and control block.
// https://git.rust-bitcoin.org/rust-bitcoin/corepc/src/commit/eca585605f6fbf79a1c8ce81ce86e2e4205cdf31/bitcoind/src/lib.rs
fn heavy_parent(bridge: &bitcoin::Script) -> (bitcoind::BitcoinD, Transaction) {
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
    for index in 0..790u32 {
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
    assert!((390_000..400_000).contains(&bytes.len()));
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

#[tokio::test]
async fn bounded_signing_admission_refuses_before_reserving_real_requests_and_inputs() {
    use crate::{
        auth::Envelope,
        metrics::{SigningSizeLabel, SigningSizeStage, METRICS},
    };
    for (inputs, accepted) in [(150usize, true), (400, false)] {
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
        let btc = Arc::new(MockBitcoinOps::new(MockBitcoinOpsConfig {
            utxos: parent
                .output
                .iter()
                .enumerate()
                .map(|(vout, output)| {
                    (OutPoint { txid: parent.compute_txid(), vout: vout as u32 }, output.clone())
                })
                .collect(),
            transaction: Some(parent),
            fee_rate: 2,
            block_height: 1,
            ..Default::default()
        }));
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
            withdrawals: vec![WithdrawalRequest {
                id: "08080808080808080000".into(),
                receiver: wallets.verifiers[1].clone(),
                amount: Amount::from_sat(inputs as u64 * 100_000 - 50_000),
                l2_sender: zksync_types::Address::repeat_byte(7),
                l2_tx_hash: H256::repeat_byte(8),
                l2_tx_log_index: 0,
            }],
        };
        seed(&pool, &wallets, &batch).await;
        let coordinator = TestCoordinator::start(pool.clone(), btc, &public, &keys[0]).await;
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
        let counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM via_withdrawal_attempts), \
             (SELECT count(*) FROM via_withdrawal_request_reservations), \
             (SELECT count(*) FROM via_withdrawal_input_reservations)",
        )
        .fetch_one(db.conn())
        .await
        .unwrap();
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
}

impl TestCoordinator {
    async fn start(
        pool: ConnectionPool<Verifier>,
        btc: Arc<dyn BitcoinOps>,
        public: &[String],
        key: &PrivateKey,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = ViaVerifierConfig::for_tests();
        config.coordinator_http_url = format!("http://{}", listener.local_addr().unwrap());
        config.coordinator_public_key = public[0].clone();
        config.withdrawal_signing_enabled = true;
        let api = crate::coordinator::api_decl::RestApi::new(
            config.clone(),
            pool,
            btc,
            withdrawal_client(),
            public.to_vec(),
            key.to_wif(),
        )
        .unwrap();
        let session = api.state.signing_session.clone();
        let server =
            tokio::spawn(async move { axum::serve(listener, api.into_router()).await.unwrap() });
        Self { config, session, server }
    }
}

impl Drop for TestCoordinator {
    fn drop(&mut self) {
        self.server.abort();
    }
}
