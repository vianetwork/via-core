use super::*;
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
        self.inner.get_transaction(id).await
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
    let secp = Secp256k1::new();
    let keys = [1u8, 2]
        .map(|n| PrivateKey::new(SecretKey::from_slice(&[n; 32]).unwrap(), Network::Bitcoin));
    let public: Vec<_> = keys
        .iter()
        .map(|key| key.public_key(&secp).to_string())
        .collect();
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
    let bridge = Address::p2tr_tweaked(
        TweakedPublicKey::dangerous_assume_tweaked(output_key),
        Network::Bitcoin,
    );
    let wallets = SystemWallets {
        sequencer: addresses[0].clone(),
        verifiers: addresses.clone(),
        governance: addresses[0].clone(),
        bridge: bridge.clone(),
    };
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = ViaVerifierConfig::for_tests();
    config.coordinator_http_url = format!("http://{}", listener.local_addr().unwrap());
    config.coordinator_public_key = public[0].clone();
    config.withdrawal_signing_enabled = true;
    config.wallet_address = addresses[0].to_string();
    let api = crate::coordinator::api_decl::RestApi::new(
        config.clone(),
        pools[0].clone(),
        btc.clone(),
        withdrawal_client(),
        public.clone(),
        keys[0].to_wif(),
    )
    .unwrap();
    let session = api.state.signing_session.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, api.into_router()).await.unwrap();
    });
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
    server.abort();
}
