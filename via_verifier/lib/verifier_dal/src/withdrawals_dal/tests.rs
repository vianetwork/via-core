use bitcoin::{
    absolute, transaction, Address, Amount, BlockHash, Network, ScriptBuf, Sequence, TxIn, Txid,
    WPubkeyHash, Witness,
};
use via_verifier_types::withdrawal_observation::ObservedWithdrawal;
use zksync_types::{Address as L2Address, H256};

use super::*;
use crate::{ConnectionPool, VerifierDal, WITHDRAWAL_INVALIDATION_LOCK_KEY};

fn wallet() -> ScriptBuf {
    ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([7; 20]))
}

fn request(id: u64) -> WithdrawalRequest {
    WithdrawalRequest {
        id: format!("{id:020x}"),
        receiver: Address::from_script(
            &ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([8; 20])),
            Network::Regtest,
        )
        .unwrap(),
        amount: Amount::from_sat(10_000),
        l2_sender: L2Address::repeat_byte(3),
        l2_tx_hash: H256::from_low_u64_be(id),
        l2_tx_log_index: 0,
    }
}

fn input(id: u8) -> (OutPoint, TxOut) {
    (
        OutPoint {
            txid: Txid::from_byte_array([id; 32]),
            vout: 2,
        },
        TxOut {
            value: Amount::from_sat(20_000),
            script_pubkey: wallet(),
        },
    )
}

#[tokio::test]
async fn reader_reset_keeps_completed_verdict_and_signed_risk_parent_closure() {
    let (pool, batch, proof) = imported(vec![request(1)]).await;
    let mut db = pool.connection().await.unwrap();
    let bridge = wallet();
    db.via_store_mode_dal()
        .ensure_proof_verification_mode(&crate::via_store_mode_dal::StoreMode {
            proof_verification_dev_mode: false,
            bitcoin_network: "regtest".into(),
            bitcoin_genesis_hash: "00".repeat(32),
        })
        .await
        .unwrap();
    db.via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[1; 32],
            b"reset-risk",
            &batch.withdrawals,
            &[input(4)],
        )
        .await
        .unwrap();
    db.via_withdrawal_dal()
        .persist_withdrawal_nonces(bridge.as_bytes(), &[1; 32], b"reset-risk", b"retained-nonces")
        .await
        .unwrap();
    db.via_withdrawal_dal()
        .mark_withdrawal_may_have_signed(bridge.as_bytes(), &[1; 32], b"reset-risk")
        .await
        .unwrap();
    let tables = [
        "via_verifier_store_mode",
        "via_votable_transactions",
        "via_withdrawal_attempts",
        "via_withdrawal_request_reservations",
        "via_withdrawal_input_reservations",
        "via_withdrawal_expected",
        "via_withdrawal_origins",
        "via_withdrawal_batches",
        "via_withdrawal_authority_domain",
        "via_withdrawal_active_wallet",
    ];
    let mut before = Vec::new();
    for table in tables {
        before.push(
            sqlx::query_scalar::<_, serde_json::Value>(&format!(
                "SELECT jsonb_agg(to_jsonb(t)) FROM {table} t"
            ))
            .instrument("reset_kept_before")
            .fetch_one(&mut db)
            .await
            .unwrap(),
        );
    }
    let mut reset = db.start_transaction().await.unwrap();
    reset.via_indexer_dal().delete_reader_suffix(0).await.unwrap();
    reset.commit().await.unwrap();
    for (table, expected) in tables.into_iter().zip(before) {
        let actual = sqlx::query_scalar::<_, serde_json::Value>(&format!(
            "SELECT jsonb_agg(to_jsonb(t)) FROM {table} t"
        ))
        .instrument("reset_kept_after")
        .fetch_one(&mut db)
        .await
        .unwrap();
        assert_eq!(actual, expected, "{table}");
    }
    assert!(db
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[2; 32],
            b"replacement",
            &batch.withdrawals,
            &[input(5)]
        )
        .await
        .is_err());
    let verdict = sqlx::query_scalar::<_, bool>(
        "SELECT l1_batch_status FROM via_votable_transactions WHERE proof_reveal_tx_id=$1",
    )
    .bind(proof)
    .instrument("reset_kept_verdict")
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert!(verdict);
}

fn batch(number: u32, requests: Vec<WithdrawalRequest>) -> CompleteWithdrawalBatch {
    CompleteWithdrawalBatch {
        batch_number: number,
        chain_id: 270,
        network: Network::Regtest,
        protocol_version: 1,
        blob_id: format!("blob-{number}"),
        pubdata_hash: H256::repeat_byte(9),
        start_block: u64::from(number),
        end_block: u64::from(number),
        pubdata: vec![1, 2, 3],
        receipts: vec![],
        withdrawals: requests,
        nonpayable: vec![],
    }
}

async fn reader_context(
    storage: &mut Connection<'_, Verifier>,
    genesis: BlockHash,
    height: u32,
    hash: BlockHash,
) {
    storage
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&crate::via_store_mode_dal::StoreMode {
            proof_verification_dev_mode: false,
            bitcoin_network: "regtest".into(),
            bitcoin_genesis_hash: "00".repeat(32),
        })
        .await
        .unwrap();
    sqlx::query("INSERT INTO via_btc_reader(singleton,format_version,bitcoin_network,genesis_hash,bootstrap_number,bootstrap_hash,replay_cutoff_number,replay_cutoff_hash,confirmed_number,confirmed_hash,replay_ready) VALUES(TRUE,1,'regtest',$1,0,$1,$2,$3,$2,$3,TRUE) ON CONFLICT DO NOTHING")
        .bind(genesis.to_string()).bind(i64::from(height)).bind(hash.to_string())
        .instrument("withdrawal_fixture_reader").execute(storage).await.unwrap();
}

async fn source(
    storage: &mut Connection<'_, Verifier>,
    batch: &CompleteWithdrawalBatch,
) -> Vec<u8> {
    let proof_hash = H256::from_low_u64_be(u64::from(batch.batch_number));
    let proof = proof_hash.as_bytes().to_vec();
    let source_hash = BlockHash::from_byte_array([200; 32]);
    canonical(storage, 0, source_hash).await;
    reader_context(storage, source_hash, 0, source_hash).await;

    storage
        .via_votes_dal()
        .insert_votable_transaction(
            batch.batch_number,
            proof_hash,
            proof_hash,
            "fixture".to_owned(),
            proof_hash,
            format!("proof-{}", batch.batch_number),
            format!("pubdata-{}", batch.batch_number),
            batch.blob_id.clone(),
            Some((0, source_hash)),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE via_votable_transactions SET is_finalized=TRUE,l1_batch_status=TRUE,proof_header_id=(SELECT id FROM via_btc_headers WHERE number=0),finalized_header_id=(SELECT id FROM via_btc_headers WHERE number=0) WHERE proof_reveal_tx_id=$1")
        .bind(&proof).instrument("withdrawal_test_source").execute(storage).await.unwrap();
    proof
}

async fn imported(
    requests: Vec<WithdrawalRequest>,
) -> (ConnectionPool<Verifier>, CompleteWithdrawalBatch, Vec<u8>) {
    // test_pool creates a separate PostgreSQL database; these are real commits,
    // not one encompassing test transaction that would hide two-writer races.
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await.unwrap();
    let batch = batch(1, requests);
    let proof = source(&mut storage, &batch).await;
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &batch)
        .await
        .unwrap();
    (pool, batch, proof)
}

fn observation(
    request: &WithdrawalRequest,
    input_id: u8,
    inclusion: Option<WithdrawalInclusion>,
) -> WithdrawalObservation {
    let prevout = input(input_id);
    let script = request.receiver.script_pubkey();
    let amount = Amount::from_sat(9_500);
    WithdrawalObservation {
        transaction: Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: prevout.0,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::default(),
            }],
            output: vec![TxOut {
                value: amount,
                script_pubkey: script.clone(),
            }],
        },
        prevouts: vec![prevout],
        withdrawals: vec![ObservedWithdrawal {
            vout: 0,
            reference: request.id.clone(),
            script_pubkey: script,
            amount,
        }],
        inclusion,
    }
}

async fn canonical(storage: &mut Connection<'_, Verifier>, height: u32, hash: BlockHash) {
    sqlx::query("DELETE FROM via_btc_headers WHERE number=$1 AND hash<>$2")
        .bind(i64::from(height))
        .bind(hash.to_string())
        .instrument("fixture_replace_header")
        .execute(&mut *storage)
        .await
        .unwrap();
    sqlx::query("INSERT INTO via_btc_headers(number,hash,parent_hash) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(i64::from(height)).bind(hash.to_string()).bind(BlockHash::all_zeros().to_string())
        .instrument("fixture_canonical_header").execute(&mut *storage).await.unwrap();
    storage.via_l1_block_dal().insert_l1_block(i64::from(height), hash.to_string()).await.unwrap();
}

async fn prune_headers(storage: &mut Connection<'_, Verifier>, through: i64) {
    sqlx::query("DELETE FROM via_btc_headers WHERE number>$1")
        .bind(through)
        .instrument("fixture_remove_headers")
        .execute(&mut *storage)
        .await
        .unwrap();
    sqlx::query("DELETE FROM via_l1_blocks WHERE number>$1")
        .bind(through)
        .instrument("fixture_remove_legacy_headers")
        .execute(storage)
        .await
        .unwrap();
}

#[tokio::test]
async fn scan_observation_excludes_admission_until_commit_or_rollback() -> anyhow::Result<()> {
    use std::time::Duration;

    for commit in [false, true] {
        let (pool, batch, _) = imported(vec![request(1)]).await;
        let mut scanner = pool.connection().await?;
        let mut outer = scanner.start_transaction().await?;
        let scanner_pid: i32 =
            sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(outer.conn()).await?;
        outer.via_indexer_dal().lock_btc_watch().await?;
        outer.via_indexer_dal().lock_withdrawal_invalidation().await?;
        outer
            .via_withdrawal_dal()
            .record_withdrawal_observation(
                wallet().as_bytes(),
                &observation(&batch.withdrawals[0], 4, None),
            )
            .await?;
        let mut signer = pool.connection().await?;
        let signer_pid: i32 =
            sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(signer.conn()).await?;
        let admission = tokio::spawn(async move {
            signer
                .via_withdrawal_dal()
                .admit_withdrawal_attempt(
                    wallet().as_bytes(),
                    &[1; 32],
                    b"scan-race",
                    &batch.withdrawals,
                    &[input(4)],
                )
                .await
        });
        let mut monitor = pool.connection().await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let blocked: bool = sqlx::query_scalar("SELECT $1 = ANY(pg_blocking_pids($2))")
                    .bind(scanner_pid)
                    .bind(signer_pid)
                    .fetch_one(monitor.conn())
                    .await?;
                if blocked {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        if commit {
            outer.commit().await?;
        } else {
            outer.rollback().await?;
        }
        let admitted = tokio::time::timeout(Duration::from_secs(5), admission).await??;
        assert_eq!(admitted.is_ok(), !commit);
    }
    Ok(())
}

#[tokio::test]
async fn import_and_observation_orders_converge_without_overwriting_gross() {
    for observed_first in [false, true] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        let batch = batch(1, vec![request(1)]);
        let proof = source(&mut storage, &batch).await;
        let observation = observation(&batch.withdrawals[0], 4, None);
        if observed_first {
            storage
                .via_withdrawal_dal()
                .record_withdrawal_observation(wallet().as_bytes(), &observation)
                .await
                .unwrap();
        }
        storage
            .via_withdrawal_dal()
            .import_complete_batch(wallet().as_bytes(), &proof, &batch)
            .await
            .unwrap();
        if !observed_first {
            assert_eq!(
                storage
                    .via_withdrawal_dal()
                    .list_eligible_withdrawals(wallet().as_bytes(), 660, 10)
                    .await
                    .unwrap(),
                batch.withdrawals
            );
        }
        storage
            .via_withdrawal_dal()
            .record_withdrawal_observation(wallet().as_bytes(), &observation)
            .await
            .unwrap();
        storage
            .via_withdrawal_dal()
            .import_complete_batch(wallet().as_bytes(), &proof, &batch)
            .await
            .unwrap();
        storage
            .via_withdrawal_dal()
            .record_withdrawal_observation(wallet().as_bytes(), &observation)
            .await
            .unwrap();
        let expected = storage
            .via_withdrawal_dal()
            .load_expected_withdrawals(wallet().as_bytes(), &[request(1).id])
            .await
            .unwrap();
        assert_eq!(expected[0].amount.to_sat(), 10_000);
        assert!(storage
            .via_withdrawal_dal()
            .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            storage
                .via_withdrawal_dal()
                .pending_withdrawal_observations(wallet().as_bytes())
                .await
                .unwrap(),
            vec![observation]
        );
    }
}

#[tokio::test]
async fn conflicting_import_commits_quarantine_even_when_returning_error() {
    let (pool, batch, proof) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    let mut changed = batch.clone();
    changed.withdrawals[0].amount = Amount::from_sat(30_000);
    assert!(storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &changed)
        .await
        .is_err());
    drop(storage);
    let mut reopened = pool.connection().await.unwrap();
    reopened
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &batch)
        .await
        .unwrap();
    assert!(reopened
        .via_withdrawal_dal()
        .load_expected_withdrawals(wallet().as_bytes(), &[request(1).id])
        .await
        .is_err());
    assert!(reopened
        .via_withdrawal_dal()
        .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
        .await
        .unwrap()
        .is_empty());
    let row = sqlx::query("SELECT evidence FROM via_withdrawal_conflicts WHERE kind='batch'")
        .map(|row| row)
        .instrument("withdrawal_test_conflict")
        .fetch_one(&mut reopened)
        .await
        .unwrap();
    assert_eq!(
        row.try_get::<Value, _>("evidence").unwrap(),
        serde_json::to_value(changed).unwrap()
    );
}

#[tokio::test]
async fn concurrent_request_and_input_reservations_have_one_winner() {
    for same_request in [false, true] {
        let (pool, _, _) = imported(vec![request(1), request(2)]).await;
        let mut a = pool.connection().await.unwrap();
        let mut b = pool.connection().await.unwrap();
        let requests_a = [request(1)];
        let requests_b = [request(if same_request { 1 } else { 2 })];
        let inputs_a = [input(4)];
        let inputs_b = [input(if same_request { 5 } else { 4 })];
        let bridge = wallet();
        let mut dal_a = a.via_withdrawal_dal();
        let mut dal_b = b.via_withdrawal_dal();
        let (a_result, b_result) = tokio::join!(
            dal_a.admit_withdrawal_attempt(
                bridge.as_bytes(),
                &[1; 32],
                b"a",
                &requests_a,
                &inputs_a
            ),
            dal_b.admit_withdrawal_attempt(
                bridge.as_bytes(),
                &[2; 32],
                b"b",
                &requests_b,
                &inputs_b
            )
        );
        assert_eq!(
            usize::from(a_result.is_ok()) + usize::from(b_result.is_ok()),
            1
        );
        let winner = if a_result.is_ok() { [1; 32] } else { [2; 32] };
        assert_eq!(
            a.via_withdrawal_dal()
                .get_withdrawal_attempt(bridge.as_bytes(), &winner)
                .await
                .unwrap()
                .unwrap()
                .state,
            WithdrawalAttemptState::Admitted
        );
    }
}

#[tokio::test]
async fn observation_racing_admission_prevents_subsequent_signing_marker() {
    let (pool, _, _) = imported(vec![request(1)]).await;
    let mut a = pool.connection().await.unwrap();
    let mut b = pool.connection().await.unwrap();
    let bridge = wallet();
    let requests = [request(1)];
    let inputs = [input(4)];
    let observed = observation(&request(1), 4, None);
    let mut dal_a = a.via_withdrawal_dal();
    let mut dal_b = b.via_withdrawal_dal();
    let (admission, observed_result) = tokio::join!(
        dal_a.admit_withdrawal_attempt(bridge.as_bytes(), &[1; 32], b"a", &requests, &inputs),
        dal_b.record_withdrawal_observation(bridge.as_bytes(), &observed)
    );
    observed_result.unwrap();
    if admission.is_ok() {
        a.via_withdrawal_dal()
            .persist_withdrawal_nonces(bridge.as_bytes(), &[1; 32], b"a", b"nonces")
            .await
            .unwrap();
    }
    assert!(a
        .via_withdrawal_dal()
        .mark_withdrawal_may_have_signed(bridge.as_bytes(), &[1; 32], b"a")
        .await
        .is_err());
    assert_eq!(
        b.via_withdrawal_dal()
            .pending_withdrawal_observations(bridge.as_bytes())
            .await
            .unwrap(),
        vec![observed]
    );
}

#[tokio::test]
async fn stale_snapshot_and_external_input_hold_are_independent() {
    let (pool, batch, _) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    let mut stale = batch.withdrawals.clone();
    stale[0].amount = Amount::from_sat(10_001);
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(wallet().as_bytes(), &[1; 32], b"a", &stale, &[input(4)])
        .await
        .is_err());
    let mut unrecognized = observation(&request(99), 4, None);
    unrecognized.withdrawals.clear();
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(wallet().as_bytes(), &unrecognized)
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            wallet().as_bytes(),
            &[1; 32],
            b"a",
            &batch.withdrawals,
            &[input(4)]
        )
        .await
        .is_err());
    let admitted = storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            wallet().as_bytes(),
            &[2; 32],
            b"b",
            &batch.withdrawals,
            &[input(5)],
        )
        .await
        .unwrap();
    assert_eq!(admitted.state, WithdrawalAttemptState::Admitted);
}

#[tokio::test]
async fn recovery_releases_only_pre_marker_attempts_and_caches_exact_public_results() {
    let (pool, batch, _) = imported(vec![request(1), request(2)]).await;
    let mut storage = pool.connection().await.unwrap();
    let bridge = wallet();
    storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[1; 32],
            b"a",
            &batch.withdrawals[..1],
            &[input(4)],
        )
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .persist_withdrawal_nonces(bridge.as_bytes(), &[1; 32], b"a", b"nonce-a")
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .retire_incomplete_withdrawal_attempts(bridge.as_bytes())
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .mark_withdrawal_may_have_signed(bridge.as_bytes(), &[1; 32], b"a")
        .await
        .is_err());
    storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[2; 32],
            b"b",
            &batch.withdrawals[..1],
            &[input(4)],
        )
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .persist_withdrawal_nonces(bridge.as_bytes(), &[2; 32], b"b", b"nonce-b")
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .persist_withdrawal_nonces(bridge.as_bytes(), &[2; 32], b"b", b"changed")
        .await
        .is_err());
    storage
        .via_withdrawal_dal()
        .mark_withdrawal_may_have_signed(bridge.as_bytes(), &[2; 32], b"b")
        .await
        .unwrap();
    drop(storage);
    let mut storage = pool.connection().await.unwrap();
    storage
        .via_withdrawal_dal()
        .retire_incomplete_withdrawal_attempts(bridge.as_bytes())
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[3; 32],
            b"c",
            &batch.withdrawals[..1],
            &[input(4)]
        )
        .await
        .is_err());
    let retired = storage
        .via_withdrawal_dal()
        .get_withdrawal_attempt(bridge.as_bytes(), &[2; 32])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retired.state, WithdrawalAttemptState::Retired);
    assert_eq!(retired.public_nonces, Some(b"nonce-b".to_vec()));
    storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[4; 32],
            b"d",
            &batch.withdrawals[1..],
            &[input(5)],
        )
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .persist_withdrawal_nonces(bridge.as_bytes(), &[4; 32], b"d", b"nonce-d")
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .mark_withdrawal_may_have_signed(bridge.as_bytes(), &[4; 32], b"d")
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .persist_withdrawal_signatures(bridge.as_bytes(), &[4; 32], b"d", b"signature-d")
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .retire_withdrawal_attempt(bridge.as_bytes(), &[4; 32])
        .await
        .is_err());
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_recoverable_withdrawal_attempts(bridge.as_bytes())
            .await
            .unwrap()[0]
            .state,
        WithdrawalAttemptState::Signed
    );
    let finalized = consensus::serialize(&observation(&request(2), 5, None).transaction);
    storage
        .via_withdrawal_dal()
        .finalize_withdrawal_attempt(bridge.as_bytes(), &[4; 32], b"d", &finalized)
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .retire_incomplete_withdrawal_attempts(bridge.as_bytes())
        .await
        .unwrap();
    let recoverable = storage
        .via_withdrawal_dal()
        .list_recoverable_withdrawal_attempts(bridge.as_bytes())
        .await
        .unwrap();
    assert_eq!(recoverable.len(), 1);
    assert_eq!(recoverable[0].state, WithdrawalAttemptState::Finalized);
    assert_eq!(
        recoverable[0].public_signatures,
        Some(b"signature-d".to_vec())
    );
    assert_eq!(recoverable[0].finalized_transaction, Some(finalized));
    assert!(storage
        .via_withdrawal_dal()
        .retire_withdrawal_attempt(bridge.as_bytes(), &[4; 32])
        .await
        .is_err());
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_recoverable_withdrawal_attempts(bridge.as_bytes())
            .await
            .unwrap(),
        recoverable
    );
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .held_withdrawal_inputs(bridge.as_bytes())
            .await
            .unwrap()
            .into_iter()
            .collect::<HashSet<_>>(),
        HashSet::from([input(4).0, input(5).0])
    );
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[5; 32],
            b"e",
            &batch.withdrawals[1..],
            &[input(5)]
        )
        .await
        .is_err());
}

#[tokio::test]
async fn fulfillment_requires_canonical_inclusion_and_reorg_never_releases_payment_risk() {
    let (pool, batch, proof) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    let block = BlockHash::from_byte_array([11; 32]);
    let mut observed = observation(
        &request(1),
        4,
        Some(WithdrawalInclusion {
            block_hash: block,
            block_height: 10,
        }),
    );
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(wallet().as_bytes(), &observed)
        .await
        .unwrap();
    assert!(!storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &observed, &batch.withdrawals, 11, 2)
        .await
        .unwrap());
    canonical(&mut storage, 10, BlockHash::from_byte_array([10; 32])).await;
    canonical(&mut storage, 11, BlockHash::from_byte_array([12; 32])).await;
    assert!(!storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &observed, &batch.withdrawals, 11, 2)
        .await
        .unwrap());
    prune_headers(&mut storage, 9).await;
    canonical(&mut storage, 10, block).await;
    canonical(&mut storage, 11, BlockHash::from_byte_array([12; 32])).await;
    assert!(!storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &observed, &batch.withdrawals, 11, 3)
        .await
        .unwrap());
    assert!(storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &observed, &batch.withdrawals, 11, 2)
        .await
        .unwrap());
    assert!(storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &observed, &batch.withdrawals, 11, 2)
        .await
        .unwrap());
    storage
        .via_withdrawal_dal()
        .invalidate_withdrawals_from_l1_height(10)
        .await
        .unwrap();
    prune_headers(&mut storage, 9).await;
    assert!(!storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &observed, &batch.withdrawals, 11, 2)
        .await
        .unwrap());
    let row =
        sqlx::query("SELECT count(*) AS active FROM via_withdrawal_fulfillments WHERE active")
            .map(|row| row)
            .instrument("withdrawal_test_revocation")
            .fetch_one(&mut storage)
            .await
            .unwrap();
    assert_eq!(row.try_get::<i64, _>("active").unwrap(), 0);
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &batch)
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
        .await
        .unwrap()
        .is_empty());
    let new_block = BlockHash::from_byte_array([13; 32]);
    observed.inclusion = Some(WithdrawalInclusion {
        block_hash: new_block,
        block_height: 12,
    });
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(wallet().as_bytes(), &observed)
        .await
        .unwrap();
    canonical(&mut storage, 12, new_block).await;
    canonical(&mut storage, 13, BlockHash::from_byte_array([14; 32])).await;
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .pending_withdrawal_observations(wallet().as_bytes())
            .await
            .unwrap(),
        vec![observed.clone()]
    );
    assert!(storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &observed, &batch.withdrawals, 13, 2)
        .await
        .unwrap());
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            wallet().as_bytes(),
            &[1; 32],
            b"repayment",
            &batch.withdrawals,
            &[input(6)]
        )
        .await
        .is_err());
}

#[tokio::test]
async fn unrelated_source_uncertainty_blocks_otherwise_valid_fulfillment() {
    let (pool, batch, proof) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    let block = BlockHash::from_byte_array([10; 32]);
    canonical(&mut storage, 10, block).await;
    let observed = observation(
        &request(1),
        4,
        Some(WithdrawalInclusion {
            block_hash: block,
            block_height: 10,
        }),
    );
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(wallet().as_bytes(), &observed)
        .await
        .unwrap();
    hold(
        &mut storage,
        wallet().as_bytes(),
        &request(1).id,
        "uncertain",
        &[66; 32],
    )
    .await
    .unwrap();
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &batch)
        .await
        .unwrap();
    assert!(!storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &observed, &batch.withdrawals, 10, 1)
        .await
        .unwrap());
    assert!(storage
        .via_withdrawal_dal()
        .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn short_reference_collision_after_fulfillment_never_relabels_full_origin() {
    let (pool, first, _) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    let block = BlockHash::from_byte_array([10; 32]);
    canonical(&mut storage, 10, block).await;
    let observed = observation(
        &request(1),
        4,
        Some(WithdrawalInclusion {
            block_hash: block,
            block_height: 10,
        }),
    );
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(wallet().as_bytes(), &observed)
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &observed, &first.withdrawals, 10, 1)
        .await
        .unwrap());
    let mut colliding = request(2);
    colliding.id = request(1).id;
    let second = batch(2, vec![colliding]);
    let proof = source(&mut storage, &second).await;
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &second)
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .load_expected_withdrawals(wallet().as_bytes(), &[request(1).id])
        .await
        .is_err());
    assert!(storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &observed, &second.withdrawals, 10, 1)
        .await
        .is_err());
    let row =
        sqlx::query("SELECT origin_txid,origin_index,active FROM via_withdrawal_fulfillments")
            .map(|row| row)
            .instrument("withdrawal_test_full_identity")
            .fetch_one(&mut storage)
            .await
            .unwrap();
    assert_eq!(
        row.try_get::<Vec<u8>, _>("origin_txid").unwrap(),
        request(1).l2_tx_hash.as_bytes()
    );
    assert_eq!(row.try_get::<i32, _>("origin_index").unwrap(), 0);
    assert!(!row.try_get::<bool, _>("active").unwrap());
}

#[tokio::test]
async fn wallet_rotation_and_domain_changes_cannot_reset_existing_holds() {
    let (pool, batch, _) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    let observed = observation(&request(1), 4, None);
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(wallet().as_bytes(), &observed)
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .verify_withdrawal_wallet(wallet().as_bytes())
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .verify_withdrawal_wallet(&[0, 32, 9])
        .await
        .is_err());
    let mut other_domain = batch.clone();
    other_domain.batch_number = 2;
    other_domain.blob_id = "blob-2".to_owned();
    other_domain.chain_id = 999;
    let proof = source(&mut storage, &other_domain).await;
    assert!(storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &other_domain)
        .await
        .is_err());
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .withdrawal_authority_chain_id(wallet().as_bytes())
            .await
            .unwrap(),
        270
    );
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .pending_withdrawal_observations(wallet().as_bytes())
            .await
            .unwrap(),
        vec![observed]
    );
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            wallet().as_bytes(),
            &[1; 32],
            b"a",
            &batch.withdrawals,
            &[input(6)]
        )
        .await
        .is_err());
}

#[tokio::test]
async fn signer_process_lock_excludes_other_connections_and_never_reenters() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut first = pool.connection().await.unwrap();
    let mut second = pool.connection().await.unwrap();
    assert!(first
        .via_withdrawal_dal()
        .acquire_withdrawal_signer_lock(wallet().as_bytes())
        .await
        .unwrap());
    assert!(!first
        .via_withdrawal_dal()
        .acquire_withdrawal_signer_lock(wallet().as_bytes())
        .await
        .unwrap());
    assert!(!second
        .via_withdrawal_dal()
        .acquire_withdrawal_signer_lock(wallet().as_bytes())
        .await
        .unwrap());
    assert!(first
        .via_withdrawal_dal()
        .check_withdrawal_signer_lock(wallet().as_bytes())
        .await
        .unwrap());
    assert!(!second
        .via_withdrawal_dal()
        .check_withdrawal_signer_lock(wallet().as_bytes())
        .await
        .unwrap());
    first
        .via_withdrawal_dal()
        .release_withdrawal_signer_lock(wallet().as_bytes())
        .await
        .unwrap();
    assert!(!first
        .via_withdrawal_dal()
        .check_withdrawal_signer_lock(wallet().as_bytes())
        .await
        .unwrap());
    assert!(second
        .via_withdrawal_dal()
        .acquire_withdrawal_signer_lock(wallet().as_bytes())
        .await
        .unwrap());
    second
        .via_withdrawal_dal()
        .release_withdrawal_signer_lock(wallet().as_bytes())
        .await
        .unwrap();
}

#[tokio::test]
async fn marker_rechecks_source_and_rejects_uncommitted_outer_transactions() {
    let (pool, batch, proof) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    {
        let mut outer = storage.start_transaction().await.unwrap();
        assert!(outer
            .via_withdrawal_dal()
            .admit_withdrawal_attempt(
                wallet().as_bytes(),
                &[1; 32],
                b"a",
                &batch.withdrawals,
                &[input(4)]
            )
            .await
            .is_err());
        outer.rollback().await.unwrap();
    }
    assert!(storage
        .via_withdrawal_dal()
        .get_withdrawal_attempt(wallet().as_bytes(), &[1; 32])
        .await
        .unwrap()
        .is_none());
    storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            wallet().as_bytes(),
            &[1; 32],
            b"a",
            &batch.withdrawals,
            &[input(4)],
        )
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .persist_withdrawal_nonces(wallet().as_bytes(), &[1; 32], b"a", b"nonces")
        .await
        .unwrap();
    sqlx::query(
        "UPDATE via_votable_transactions SET is_finalized=FALSE WHERE proof_reveal_tx_id=$1",
    )
    .bind(&proof)
    .instrument("withdrawal_test_source_revocation")
    .execute(&mut storage)
    .await
    .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .mark_withdrawal_may_have_signed(wallet().as_bytes(), &[1; 32], b"a")
        .await
        .is_err());
    assert!(storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &batch)
        .await
        .is_err());
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .get_withdrawal_attempt(wallet().as_bytes(), &[1; 32])
            .await
            .unwrap()
            .unwrap()
            .state,
        WithdrawalAttemptState::Admitted
    );
}

#[tokio::test]
async fn conflicting_observation_keeps_original_bytes_and_holds_both_references() {
    let (pool, batch, _) = imported(vec![request(1), request(2)]).await;
    let mut storage = pool.connection().await.unwrap();
    let hash = BlockHash::from_byte_array([10; 32]);
    canonical(&mut storage, 10, hash).await;
    let original = observation(
        &request(1),
        4,
        Some(WithdrawalInclusion {
            block_hash: hash,
            block_height: 10,
        }),
    );
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(wallet().as_bytes(), &original)
        .await
        .unwrap();
    let mut conflicting = original.clone();
    conflicting.withdrawals[0].reference = request(2).id;
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(wallet().as_bytes(), &conflicting)
        .await
        .unwrap();
    drop(storage);
    let mut storage = pool.connection().await.unwrap();
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(wallet().as_bytes(), &original)
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .pending_withdrawal_observations(wallet().as_bytes())
        .await
        .unwrap()
        .is_empty());
    assert!(!storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(
            wallet().as_bytes(),
            &original,
            &batch.withdrawals[..1],
            10,
            1
        )
        .await
        .unwrap());
    assert!(storage
        .via_withdrawal_dal()
        .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
        .await
        .unwrap()
        .is_empty());
    let retained = sqlx::query("SELECT evidence FROM via_withdrawal_observations")
        .map(|row| row)
        .instrument("withdrawal_test_retained_conflict")
        .fetch_one(&mut storage)
        .await
        .unwrap();
    assert_eq!(
        retained.try_get::<Value, _>("evidence").unwrap(),
        observation_evidence(&original).unwrap()
    );
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            wallet().as_bytes(),
            &[1; 32],
            b"a",
            &batch.withdrawals[1..],
            &[input(5)]
        )
        .await
        .is_err());
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .load_expected_withdrawals(wallet().as_bytes(), &[request(2).id, request(1).id])
            .await
            .unwrap(),
        vec![request(2), request(1)]
    );
}

#[tokio::test]
async fn competing_candidate_payments_never_aggregate_into_fulfillment() {
    let (pool, batch, _) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    let hash = BlockHash::from_byte_array([11; 32]);
    canonical(&mut storage, 10, hash).await;
    let first = observation(
        &request(1),
        4,
        Some(WithdrawalInclusion {
            block_hash: hash,
            block_height: 10,
        }),
    );
    let second = observation(
        &request(1),
        5,
        Some(WithdrawalInclusion {
            block_hash: hash,
            block_height: 10,
        }),
    );
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(wallet().as_bytes(), &first)
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &first, &batch.withdrawals, 10, 1)
        .await
        .unwrap());
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(wallet().as_bytes(), &second)
        .await
        .unwrap();
    assert!(!storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &first, &batch.withdrawals, 10, 1)
        .await
        .unwrap());
    assert!(!storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(wallet().as_bytes(), &second, &batch.withdrawals, 10, 1)
        .await
        .unwrap());
    let active =
        sqlx::query("SELECT count(*) AS count FROM via_withdrawal_fulfillments WHERE active")
            .map(|row| row)
            .instrument("withdrawal_test_candidate_revocation")
            .fetch_one(&mut storage)
            .await
            .unwrap();
    assert_eq!(active.try_get::<i64, _>("count").unwrap(), 0);
}

#[tokio::test]
async fn complete_empty_import_marks_source_without_inventing_requests() {
    let (pool, empty, proof) = imported(vec![]).await;
    let mut storage = pool.connection().await.unwrap();
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &empty)
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .get_finalized_block_and_non_processed_withdrawal(wallet().as_bytes(), 1)
        .await
        .unwrap()
        .is_none());
    let second = batch(2, vec![request(1)]);
    let second_proof = source(&mut storage, &second).await;
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_finalized_blocks_with_no_bridge_withdrawal(wallet().as_bytes())
            .await
            .unwrap(),
        vec![(2, "blob-2".to_owned(), second_proof)]
    );
}

async fn invalidate_withdrawal_suffix(storage: &mut Connection<'_, Verifier>, from: u32) {
    let mut reorg = storage.start_transaction().await.unwrap();
    reorg.via_withdrawal_dal().invalidate_withdrawals_from_l1_height(from).await.unwrap();
    prune_headers(&mut reorg, i64::from(from) - 1).await;
    reorg.commit().await.unwrap();
}

#[tokio::test]
async fn soft_bitcoin_reorg_keeps_older_unsigned_source_admissible() {
    let (pool, batch, proof) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    canonical(&mut storage, 10, BlockHash::from_byte_array([10; 32])).await;
    invalidate_withdrawal_suffix(&mut storage, 10).await;
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
            .await
            .unwrap(),
        batch.withdrawals
    );
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &batch)
        .await
        .unwrap();
    let admitted = storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            wallet().as_bytes(),
            &[1; 32],
            b"after-soft-reorg",
            &batch.withdrawals,
            &[input(4)],
        )
        .await
        .unwrap();
    assert_eq!(admitted.state, WithdrawalAttemptState::Admitted);
}

#[tokio::test]
async fn exact_source_revalidation_restores_only_unsigned_unheld_requests() {
    let (pool, batch, proof) = imported((1..=6).map(request).collect()).await;
    let mut storage = pool.connection().await.unwrap();
    let bridge = wallet();
    storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[1; 32],
            b"signed-risk",
            &batch.withdrawals[1..2],
            &[input(4)],
        )
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .persist_withdrawal_nonces(bridge.as_bytes(), &[1; 32], b"signed-risk", b"nonces")
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .mark_withdrawal_may_have_signed(bridge.as_bytes(), &[1; 32], b"signed-risk")
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(bridge.as_bytes(), &observation(&request(3), 5, None))
        .await
        .unwrap();
    hold(
        &mut storage,
        bridge.as_bytes(),
        &request(4).id,
        "conflict",
        &[44; 32],
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO via_withdrawal_legacy_holds(reference) VALUES($1)")
        .bind(request(5).id)
        .instrument("withdrawal_test_legacy_revalidation")
        .execute(&mut storage)
        .await
        .unwrap();
    // A hold attributed to another source is not this source's to release.
    hold(
        &mut storage,
        bridge.as_bytes(),
        &request(6).id,
        "uncertain",
        &[66; 32],
    )
    .await
    .unwrap();
    let mut reorg = storage.start_transaction().await.unwrap();
    reorg
        .via_withdrawal_dal()
        .invalidate_withdrawal_batches_from(1)
        .await
        .unwrap();
    sqlx::query("DELETE FROM via_votable_transactions WHERE proof_reveal_tx_id=$1")
        .bind(&proof)
        .instrument("withdrawal_test_delete_source")
        .execute(&mut reorg)
        .await
        .unwrap();
    reorg.commit().await.unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .import_complete_batch(bridge.as_bytes(), &proof, &batch)
        .await
        .is_err());
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[2; 32],
            b"stale",
            &batch.withdrawals[..1],
            &[input(6)]
        )
        .await
        .is_err());
    source(&mut storage, &batch).await;
    // Merely restoring the source row cannot bypass complete-source revalidation.
    assert!(storage
        .via_withdrawal_dal()
        .list_eligible_withdrawals(bridge.as_bytes(), 0, 10)
        .await
        .unwrap()
        .is_empty());
    storage
        .via_withdrawal_dal()
        .import_complete_batch(bridge.as_bytes(), &proof, &batch)
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .retire_incomplete_withdrawal_attempts(bridge.as_bytes())
        .await
        .unwrap();
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_eligible_withdrawals(bridge.as_bytes(), 0, 10)
            .await
            .unwrap(),
        vec![request(1)]
    );
    // Signed-risk request and input reservations both survive, independently.
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[2; 32],
            b"replacement",
            &batch.withdrawals[1..2],
            &[input(6)]
        )
        .await
        .is_err());
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[2; 32],
            b"reuse-input",
            &batch.withdrawals[..1],
            &[input(4)]
        )
        .await
        .is_err());
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[2; 32],
            b"observed-input",
            &batch.withdrawals[..1],
            &[input(5)]
        )
        .await
        .is_err());
    for request in &batch.withdrawals[2..] {
        assert!(storage
            .via_withdrawal_dal()
            .admit_withdrawal_attempt(
                bridge.as_bytes(),
                &[2; 32],
                b"held",
                std::slice::from_ref(request),
                &[input(6)]
            )
            .await
            .is_err());
    }
    let mut stale = batch.withdrawals[..1].to_vec();
    stale[0].amount = Amount::from_sat(10_001);
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[2; 32],
            b"changed-snapshot",
            &stale,
            &[input(6)]
        )
        .await
        .is_err());
    let admitted = storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[2; 32],
            b"revalidated",
            &batch.withdrawals[..1],
            &[input(6)],
        )
        .await
        .unwrap();
    assert_eq!(admitted.state, WithdrawalAttemptState::Admitted);
}

#[tokio::test]
async fn source_batch_invalidation_leaves_earlier_unsigned_requests_eligible() {
    let (pool, first, _) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    let second = batch(2, vec![request(2)]);
    let second_proof = source(&mut storage, &second).await;
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &second_proof, &second)
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .invalidate_withdrawal_batches_from(2)
        .await
        .unwrap();
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
            .await
            .unwrap(),
        first.withdrawals
    );
    assert!(storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            wallet().as_bytes(),
            &[1; 32],
            b"invalidated",
            &second.withdrawals,
            &[input(4)]
        )
        .await
        .is_err());
    let admitted = storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            wallet().as_bytes(),
            &[1; 32],
            b"unaffected",
            &first.withdrawals,
            &[input(4)],
        )
        .await
        .unwrap();
    assert_eq!(admitted.state, WithdrawalAttemptState::Admitted);
}

#[tokio::test]
async fn rebroadcast_inclusion_tracks_canonical_hash_and_height_without_fulfillment() {
    let (pool, _, _) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    let bridge = wallet();
    let mut observed = observation(&request(1), 4, None);
    let txid = observed.transaction.compute_txid().to_byte_array();
    assert!(!storage
        .via_withdrawal_dal()
        .observed_withdrawal_transaction_has_canonical_inclusion(bridge.as_bytes(), &txid)
        .await
        .unwrap());
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(bridge.as_bytes(), &observed)
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .observed_withdrawal_transaction_exists(bridge.as_bytes(), &txid)
        .await
        .unwrap());
    assert!(!storage
        .via_withdrawal_dal()
        .observed_withdrawal_transaction_has_canonical_inclusion(bridge.as_bytes(), &txid)
        .await
        .unwrap());
    let hash = BlockHash::from_byte_array([10; 32]);
    observed.inclusion = Some(WithdrawalInclusion {
        block_hash: hash,
        block_height: 10,
    });
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(bridge.as_bytes(), &observed)
        .await
        .unwrap();
    canonical(&mut storage, 9, hash).await;
    canonical(&mut storage, 10, BlockHash::from_byte_array([11; 32])).await;
    assert!(!storage
        .via_withdrawal_dal()
        .observed_withdrawal_transaction_has_canonical_inclusion(bridge.as_bytes(), &txid)
        .await
        .unwrap());
    prune_headers(&mut storage, 8).await;
    canonical(&mut storage, 10, hash).await;
    assert!(storage
        .via_withdrawal_dal()
        .observed_withdrawal_transaction_has_canonical_inclusion(bridge.as_bytes(), &txid)
        .await
        .unwrap());
    invalidate_withdrawal_suffix(&mut storage, 10).await;
    assert!(!storage
        .via_withdrawal_dal()
        .observed_withdrawal_transaction_has_canonical_inclusion(bridge.as_bytes(), &txid)
        .await
        .unwrap());
    let replacement = BlockHash::from_byte_array([12; 32]);
    canonical(&mut storage, 10, replacement).await;
    assert!(!storage
        .via_withdrawal_dal()
        .observed_withdrawal_transaction_has_canonical_inclusion(bridge.as_bytes(), &txid)
        .await
        .unwrap());
    observed.inclusion = Some(WithdrawalInclusion {
        block_hash: replacement,
        block_height: 10,
    });
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(bridge.as_bytes(), &observed)
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .observed_withdrawal_transaction_has_canonical_inclusion(bridge.as_bytes(), &txid)
        .await
        .unwrap());
    assert!(storage
        .via_withdrawal_dal()
        .list_eligible_withdrawals(bridge.as_bytes(), 0, 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn migration_preserves_legacy_evidence_and_fences_old_binaries() {
    use sqlx::Executor;

    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await.unwrap();
    let mut tx = storage.start_transaction().await.unwrap();
    tx.conn()
        .execute(
            "CREATE SCHEMA withdrawal_migration_fixture;
         SET LOCAL search_path TO withdrawal_migration_fixture;
         CREATE TABLE via_withdrawals(id TEXT PRIMARY KEY,value BIGINT);
         CREATE TABLE via_bridge_withdrawals(id BIGINT PRIMARY KEY);
         CREATE TABLE via_l1_batch_bridge_withdrawals(proof_reveal_tx_id BYTEA PRIMARY KEY);
         CREATE TABLE via_votable_transactions(id BIGINT PRIMARY KEY);
         CREATE TABLE via_votes(id BIGINT PRIMARY KEY);
         INSERT INTO via_withdrawals VALUES('00000000000000000001',1234);
         INSERT INTO via_bridge_withdrawals VALUES(1);",
        )
        .await
        .unwrap();
    tx.conn()
        .execute(include_str!(
            "../../migrations/20260924000000_withdrawal_lifecycle.up.sql"
        ))
        .await
        .unwrap();
    let retained = sqlx::query("SELECT value FROM via_withdrawals WHERE id='00000000000000000001'")
        .map(|row| row)
        .instrument("withdrawal_test_legacy_evidence")
        .fetch_one(&mut tx)
        .await
        .unwrap();
    assert_eq!(retained.try_get::<i64, _>("value").unwrap(), 1234);
    let quarantine = sqlx::query("SELECT reference FROM via_withdrawal_legacy_holds")
        .map(|row| row)
        .instrument("withdrawal_test_legacy_hold")
        .fetch_one(&mut tx)
        .await
        .unwrap();
    assert_eq!(
        quarantine.try_get::<String, _>("reference").unwrap(),
        "00000000000000000001"
    );
    let authorities = sqlx::query("SELECT count(*) AS count FROM via_withdrawal_expected")
        .map(|row| row)
        .instrument("withdrawal_test_no_legacy_promotion")
        .fetch_one(&mut tx)
        .await
        .unwrap();
    assert_eq!(authorities.try_get::<i64, _>("count").unwrap(), 0);
    for old_write in [
        "UPDATE via_withdrawals SET value=9999",
        "INSERT INTO via_bridge_withdrawals VALUES(2)",
        "TRUNCATE via_l1_batch_bridge_withdrawals",
    ] {
        let mut attempt = tx.start_transaction().await.unwrap();
        assert!(attempt.conn().execute(old_write).await.is_err());
        attempt.rollback().await.unwrap();
    }
    let mut rollback = tx.start_transaction().await.unwrap();
    assert!(rollback
        .conn()
        .execute(include_str!(
            "../../migrations/20260924000000_withdrawal_lifecycle.down.sql"
        ))
        .await
        .is_err());
    rollback.rollback().await.unwrap();
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn exposed_proposal_survives_divergent_signer_progress_and_coordinator_restart() {
    for startup_retirement in [false, true] {
        let (coordinator, batch, _) = imported(vec![request(1), request(2)]).await;
        let (signer, _, _) = imported(batch.withdrawals.clone()).await;
        let bridge = wallet();
        let mut local = coordinator.connection().await.unwrap();
        let mut remote = signer.connection().await.unwrap();
        let proposal = local
            .via_withdrawal_dal()
            .expose_withdrawal_proposal(
                bridge.as_bytes(),
                &[1; 32],
                b"proposal",
                &batch.withdrawals[..1],
                &[input(4)],
            )
            .await
            .unwrap();
        assert_eq!(proposal.state, WithdrawalAttemptState::Admitted);
        assert_eq!(
            local
                .via_withdrawal_dal()
                .expose_withdrawal_proposal(
                    bridge.as_bytes(),
                    &[1; 32],
                    b"proposal",
                    &batch.withdrawals[..1],
                    &[input(4)]
                )
                .await
                .unwrap(),
            proposal
        );
        assert!(local
            .via_withdrawal_dal()
            .expose_withdrawal_proposal(
                bridge.as_bytes(),
                &[1; 32],
                b"changed",
                &batch.withdrawals[..1],
                &[input(4)]
            )
            .await
            .is_err());
        assert!(local
            .via_withdrawal_dal()
            .expose_withdrawal_proposal(
                bridge.as_bytes(),
                &[1; 32],
                b"proposal",
                &batch.withdrawals[1..],
                &[input(4)]
            )
            .await
            .is_err());
        assert!(local
            .via_withdrawal_dal()
            .expose_withdrawal_proposal(
                bridge.as_bytes(),
                &[1; 32],
                b"proposal",
                &batch.withdrawals[..1],
                &[input(5)]
            )
            .await
            .is_err());
        // Exposure leaves the coordinator's own nonce generation legal.
        local
            .via_withdrawal_dal()
            .persist_withdrawal_nonces(bridge.as_bytes(), &[1; 32], b"proposal", b"local-nonces")
            .await
            .unwrap();
        remote
            .via_withdrawal_dal()
            .admit_withdrawal_attempt(
                bridge.as_bytes(),
                &[1; 32],
                b"proposal",
                &batch.withdrawals[..1],
                &[input(4)],
            )
            .await
            .unwrap();
        remote
            .via_withdrawal_dal()
            .persist_withdrawal_nonces(bridge.as_bytes(), &[1; 32], b"proposal", b"remote-nonces")
            .await
            .unwrap();
        remote
            .via_withdrawal_dal()
            .mark_withdrawal_may_have_signed(bridge.as_bytes(), &[1; 32], b"proposal")
            .await
            .unwrap();
        drop(local);
        let mut local = coordinator.connection().await.unwrap();
        if startup_retirement {
            local
                .via_withdrawal_dal()
                .retire_incomplete_withdrawal_attempts(bridge.as_bytes())
                .await
                .unwrap();
        } else {
            local
                .via_withdrawal_dal()
                .retire_withdrawal_attempt(bridge.as_bytes(), &[1; 32])
                .await
                .unwrap();
        }
        remote
            .via_withdrawal_dal()
            .retire_incomplete_withdrawal_attempts(bridge.as_bytes())
            .await
            .unwrap();
        for storage in [&mut local, &mut remote] {
            assert_eq!(
                storage
                    .via_withdrawal_dal()
                    .list_eligible_withdrawals(bridge.as_bytes(), 0, 10)
                    .await
                    .unwrap(),
                vec![request(2)]
            );
            assert_eq!(
                storage
                    .via_withdrawal_dal()
                    .held_withdrawal_inputs(bridge.as_bytes())
                    .await
                    .unwrap(),
                vec![input(4).0]
            );
            assert!(storage
                .via_withdrawal_dal()
                .admit_withdrawal_attempt(
                    bridge.as_bytes(),
                    &[2; 32],
                    b"repeat-request",
                    &batch.withdrawals[..1],
                    &[input(5)]
                )
                .await
                .is_err());
            assert!(storage
                .via_withdrawal_dal()
                .admit_withdrawal_attempt(
                    bridge.as_bytes(),
                    &[2; 32],
                    b"repeat-input",
                    &batch.withdrawals[1..],
                    &[input(4)]
                )
                .await
                .is_err());
            storage
                .via_withdrawal_dal()
                .admit_withdrawal_attempt(
                    bridge.as_bytes(),
                    &[2; 32],
                    b"independent",
                    &batch.withdrawals[1..],
                    &[input(5)],
                )
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn held_inputs_union_preserves_exposure_and_observations_but_releases_unsigned() {
    let (pool, batch, _) = imported(vec![request(1), request(2), request(3)]).await;
    let mut storage = pool.connection().await.unwrap();
    let bridge = wallet();
    storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[1; 32],
            b"unsigned",
            &batch.withdrawals[..1],
            &[input(4)],
        )
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[2; 32],
            b"exposed",
            &batch.withdrawals[1..2],
            &[input(5)],
        )
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .expose_withdrawal_proposal(
            bridge.as_bytes(),
            &[2; 32],
            b"exposed",
            &batch.withdrawals[1..2],
            &[input(5)],
        )
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(bridge.as_bytes(), &observation(&request(2), 5, None))
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(bridge.as_bytes(), &observation(&request(3), 6, None))
        .await
        .unwrap();
    let held = storage
        .via_withdrawal_dal()
        .held_withdrawal_inputs(bridge.as_bytes())
        .await
        .unwrap();
    assert_eq!(held.len(), 3);
    assert_eq!(
        held.into_iter().collect::<HashSet<_>>(),
        HashSet::from([input(4).0, input(5).0, input(6).0])
    );
    storage
        .via_withdrawal_dal()
        .retire_incomplete_withdrawal_attempts(bridge.as_bytes())
        .await
        .unwrap();
    drop(storage);
    let mut storage = pool.connection().await.unwrap();
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .held_withdrawal_inputs(bridge.as_bytes())
            .await
            .unwrap()
            .into_iter()
            .collect::<HashSet<_>>(),
        HashSet::from([input(5).0, input(6).0])
    );
    assert!(storage
        .via_withdrawal_dal()
        .expose_withdrawal_proposal(
            bridge.as_bytes(),
            &[1; 32],
            b"unsigned",
            &batch.withdrawals[..1],
            &[input(4)]
        )
        .await
        .is_err());
    storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[3; 32],
            b"released",
            &batch.withdrawals[..1],
            &[input(4)],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn recovery_listing_excludes_canonical_payments_and_restores_them_after_reorg() {
    let (pool, batch, _) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    let bridge = wallet();
    storage
        .via_withdrawal_dal()
        .admit_withdrawal_attempt(
            bridge.as_bytes(),
            &[1; 32],
            b"proposal",
            &batch.withdrawals,
            &[input(4)],
        )
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .persist_withdrawal_nonces(bridge.as_bytes(), &[1; 32], b"proposal", b"nonces")
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .mark_withdrawal_may_have_signed(bridge.as_bytes(), &[1; 32], b"proposal")
        .await
        .unwrap();
    storage
        .via_withdrawal_dal()
        .persist_withdrawal_signatures(bridge.as_bytes(), &[1; 32], b"proposal", b"signatures")
        .await
        .unwrap();
    let mut observed = observation(&request(1), 4, None);
    storage
        .via_withdrawal_dal()
        .finalize_withdrawal_attempt(
            bridge.as_bytes(),
            &[1; 32],
            b"proposal",
            &consensus::serialize(&observed.transaction),
        )
        .await
        .unwrap();
    let recovery = storage
        .via_withdrawal_dal()
        .list_recoverable_withdrawal_attempts(bridge.as_bytes())
        .await
        .unwrap();
    assert_eq!(recovery[0].state, WithdrawalAttemptState::Finalized);
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(bridge.as_bytes(), &observed)
        .await
        .unwrap();
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_recoverable_withdrawal_attempts(bridge.as_bytes())
            .await
            .unwrap(),
        recovery
    );
    let hash = BlockHash::from_byte_array([10; 32]);
    observed.inclusion = Some(WithdrawalInclusion {
        block_hash: hash,
        block_height: 10,
    });
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(bridge.as_bytes(), &observed)
        .await
        .unwrap();
    canonical(&mut storage, 10, hash).await;
    assert!(storage
        .via_withdrawal_dal()
        .list_recoverable_withdrawal_attempts(bridge.as_bytes())
        .await
        .unwrap()
        .is_empty());
    prune_headers(&mut storage, 9).await;
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_recoverable_withdrawal_attempts(bridge.as_bytes())
            .await
            .unwrap(),
        recovery
    );
}

fn nonpayable(id: u64) -> via_verifier_types::withdrawal::NonPayableWithdrawal {
    let request = request(id);
    via_verifier_types::withdrawal::NonPayableWithdrawal {
        id: request.id,
        l2_tx_hash: request.l2_tx_hash,
        l2_tx_log_index: request.l2_tx_log_index,
        l2_sender: request.l2_sender,
        amount: request.amount,
        raw_receiver: vec![0xff],
        reason: via_verifier_types::withdrawal::NonPayableWithdrawalReason::InvalidUtf8,
    }
}

#[tokio::test]
async fn nonpayable_origins_remain_negative_across_replays_and_cross_batch_relabeling() {
    for negative_first in [true, false] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        let bridge = wallet();
        let mut first = batch(
            1,
            if negative_first {
                vec![]
            } else {
                vec![request(1)]
            },
        );
        if negative_first {
            first.nonpayable.push(nonpayable(1));
        }
        let proof = source(&mut storage, &first).await;
        storage
            .via_withdrawal_dal()
            .import_complete_batch(bridge.as_bytes(), &proof, &first)
            .await
            .unwrap();
        storage
            .via_withdrawal_dal()
            .import_complete_batch(bridge.as_bytes(), &proof, &first)
            .await
            .unwrap();
        assert_eq!(
            storage
                .via_withdrawal_dal()
                .list_eligible_withdrawals(bridge.as_bytes(), 0, 10)
                .await
                .unwrap(),
            first.withdrawals
        );
        assert!(storage
            .via_withdrawal_dal()
            .list_finalized_blocks_with_no_bridge_withdrawal(bridge.as_bytes())
            .await
            .unwrap()
            .is_empty());
        // Same full origin cannot move between payable and non-payable classes.
        let mut changed = batch(
            2,
            if negative_first {
                vec![request(1)]
            } else {
                vec![]
            },
        );
        if !negative_first {
            changed.nonpayable.push(nonpayable(1));
        }
        let changed_proof = source(&mut storage, &changed).await;
        assert!(storage
            .via_withdrawal_dal()
            .import_complete_batch(bridge.as_bytes(), &changed_proof, &changed)
            .await
            .is_err());
        drop(storage);
        let mut storage = pool.connection().await.unwrap();
        assert!(storage
            .via_withdrawal_dal()
            .load_expected_withdrawals(bridge.as_bytes(), &[request(1).id])
            .await
            .is_err());
        assert!(storage
            .via_withdrawal_dal()
            .list_eligible_withdrawals(bridge.as_bytes(), 0, 10)
            .await
            .unwrap()
            .is_empty());
        let retained =
            sqlx::query("SELECT evidence FROM via_withdrawal_batches WHERE batch_number=1")
                .map(|row| row)
                .instrument("withdrawal_test_nonpayable_retained")
                .fetch_one(&mut storage)
                .await
                .unwrap();
        assert_eq!(
            retained.try_get::<Value, _>("evidence").unwrap(),
            serde_json::to_value(&first).unwrap()
        );
        let later = batch(3, vec![request(3)]);
        let later_proof = source(&mut storage, &later).await;
        storage
            .via_withdrawal_dal()
            .import_complete_batch(bridge.as_bytes(), &later_proof, &later)
            .await
            .unwrap();
        assert_eq!(
            storage
                .via_withdrawal_dal()
                .list_eligible_withdrawals(bridge.as_bytes(), 0, 10)
                .await
                .unwrap(),
            later.withdrawals
        );
        storage
            .via_withdrawal_dal()
            .admit_withdrawal_attempt(
                bridge.as_bytes(),
                &[3; 32],
                b"unrelated",
                &later.withdrawals,
                &[input(4)],
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn nonpayable_import_rejects_duplicate_full_origins_and_malformed_references_atomically() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await.unwrap();
    let mut candidate = batch(1, vec![request(1)]);
    candidate.nonpayable.push(nonpayable(1));
    let proof = source(&mut storage, &candidate).await;
    assert!(storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &candidate)
        .await
        .is_err());
    candidate.withdrawals.clear();
    candidate.nonpayable.push(nonpayable(1));
    assert!(storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &candidate)
        .await
        .is_err());
    candidate.nonpayable.pop();
    candidate.nonpayable[0].id = "not-a-reference".to_owned();
    assert!(storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &candidate)
        .await
        .is_err());
    candidate.nonpayable[0] = nonpayable(1);
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &candidate)
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
        .await
        .unwrap()
        .is_empty());
    assert!(storage
        .via_withdrawal_dal()
        .load_expected_withdrawals(wallet().as_bytes(), &[request(1).id])
        .await
        .is_err());
}

#[tokio::test]
async fn nonpayable_reference_collision_revokes_only_ambiguous_payment_authority() {
    let (pool, first, _) = imported(vec![request(1), request(3)]).await;
    let mut storage = pool.connection().await.unwrap();
    let hash = BlockHash::from_byte_array([10; 32]);
    canonical(&mut storage, 10, hash).await;
    let observed = observation(
        &request(1),
        4,
        Some(WithdrawalInclusion {
            block_hash: hash,
            block_height: 10,
        }),
    );
    storage
        .via_withdrawal_dal()
        .record_withdrawal_observation(wallet().as_bytes(), &observed)
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(
            wallet().as_bytes(),
            &observed,
            &first.withdrawals[..1],
            10,
            1
        )
        .await
        .unwrap());
    let mut second = batch(2, vec![]);
    let mut negative = nonpayable(2);
    negative.id = request(1).id;
    second.nonpayable.push(negative);
    let proof = source(&mut storage, &second).await;
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &second)
        .await
        .unwrap();
    assert!(storage
        .via_withdrawal_dal()
        .load_expected_withdrawals(wallet().as_bytes(), &[request(1).id])
        .await
        .is_err());
    assert!(storage
        .via_withdrawal_dal()
        .fulfill_withdrawal_observation(
            wallet().as_bytes(),
            &observed,
            &first.withdrawals[..1],
            10,
            1
        )
        .await
        .is_err());
    let active =
        sqlx::query("SELECT count(*) AS count FROM via_withdrawal_fulfillments WHERE active")
            .map(|row| row)
            .instrument("withdrawal_test_nonpayable_collision")
            .fetch_one(&mut storage)
            .await
            .unwrap();
    assert_eq!(active.try_get::<i64, _>("count").unwrap(), 0);
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
            .await
            .unwrap(),
        vec![request(3)]
    );
}

async fn proof_at(
    storage: &mut Connection<'_, Verifier>,
    batch: &CompleteWithdrawalBatch,
    inclusion: Option<(u32, BlockHash)>,
) -> anyhow::Result<()> {
    let proof = H256::from_low_u64_be(u64::from(batch.batch_number));
    storage
        .via_votes_dal()
        .insert_votable_transaction(
            batch.batch_number,
            proof,
            proof,
            "fixture".to_owned(),
            proof,
            format!("proof-{}", batch.batch_number),
            format!("pubdata-{}", batch.batch_number),
            batch.blob_id.clone(),
            inclusion,
        )
        .await.map(|_| ())
}

async fn legacy_vote(
    storage: &mut Connection<'_, Verifier>,
    id: i64,
    voter: &str,
    approved: bool,
    inclusion: Option<(u32, BlockHash)>,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO via_votes(votable_transaction_id,verifier_address,vote,source_l1_block_number,source_l1_block_hash) VALUES($1,$2,$3,$4,$5)")
        .bind(id).bind(voter).bind(approved).bind(inclusion.map(|(h,_)|i64::from(h)))
        .bind(inclusion.map(|(_,hash)|hash.to_string())).instrument("legacy_vote_fact").execute(storage).await?;
    Ok(())
}

async fn recorded_finality(storage: &mut Connection<'_, Verifier>, id: i64) {
    sqlx::query("UPDATE via_votable_transactions SET is_finalized=TRUE WHERE id=$1")
        .bind(id)
        .instrument("legacy_finality_fact")
        .execute(storage)
        .await
        .unwrap();
}

async fn retained_votes(storage: &mut Connection<'_, Verifier>) -> Vec<(String, bool)> {
    sqlx::query_as("SELECT verifier_address,vote FROM via_votes ORDER BY verifier_address")
        .instrument("retained_vote_facts")
        .fetch_all(storage)
        .await
        .unwrap()
}

async fn prune_votes_at(storage: &mut Connection<'_, Verifier>, cut: i64) {
    let mut reorg = storage.start_transaction().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(WITHDRAWAL_INVALIDATION_LOCK_KEY)
        .instrument("withdrawal_test_vote_reorg_gate")
        .execute(&mut reorg)
        .await
        .unwrap();
    prune_headers(&mut reorg, cut).await;
    via_btc_storage::ReaderDal::new(&mut reorg)
        .delete_common_suffix(cut.try_into().unwrap())
        .await
        .unwrap();
    reorg.commit().await.unwrap();
}

async fn legacy_vote_history(
    inclusion: Option<(u32, BlockHash)>,
) -> (Connection<'static, Verifier>, CompleteWithdrawalBatch, i64) {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut db = pool.connection().await.unwrap();
    for height in [10, 15, 20] {
        canonical(&mut db, height, BlockHash::from_byte_array([height as u8; 32])).await;
    }
    reader_context(&mut db, BlockHash::all_zeros(), 20, BlockHash::from_byte_array([20; 32])).await;
    let candidate = batch(1, vec![request(1)]);
    proof_at(&mut db, &candidate, inclusion).await.unwrap();
    let id = db
        .via_votes_dal()
        .verify_votable_transaction(1, H256::from_low_u64_be(1), true)
        .await
        .unwrap();
    sqlx::query("UPDATE via_votable_transactions SET proof_header_id=(SELECT id FROM via_btc_headers WHERE number=$2),finalized_header_id=NULL WHERE id=$1")
        .bind(id).bind(inclusion.map(|(h,_)|i64::from(h))).instrument("legacy_history_proof_binding").execute(&mut db).await.unwrap();
    (db, candidate, id)
}

#[tokio::test]
async fn legacy_vote_reorg_cannot_supply_current_finalization_receipt() {
    let proof_hash = BlockHash::from_byte_array([10; 32]);
    let retained_hash = BlockHash::from_byte_array([15; 32]);
    let reverted_hash = BlockHash::from_byte_array([20; 32]);
    let (mut db, candidate, id) = legacy_vote_history(Some((10, proof_hash))).await;
    let proof = H256::from_low_u64_be(1);
    legacy_vote(&mut db, id, "reverted", true, Some((20, reverted_hash))).await.unwrap();
    legacy_vote(&mut db, id, "retained", true, Some((15, retained_hash))).await.unwrap();
    // One transaction cannot occur twice on the selected Bitcoin chain.
    let conflict = proof_at(&mut db, &candidate, Some((20, reverted_hash))).await.unwrap_err();
    assert_eq!(
        conflict.downcast_ref::<via_btc_client::indexer::replay::ReplayConflict>().unwrap().field,
        "duplicate_canonical_inclusion"
    );
    recorded_finality(&mut db, id).await;
    prune_votes_at(&mut db, 15).await;
    assert_eq!(retained_votes(&mut db).await, vec![("retained".into(), true)]);
    let replacement = BlockHash::from_byte_array([21; 32]);
    canonical(&mut db, 20, replacement).await;
    legacy_vote(&mut db, id, "reverted", true, Some((20, replacement))).await.unwrap();
    assert!(db.via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), proof.as_bytes(), &candidate)
        .await.is_err());
    assert_eq!(
        db.via_withdrawal_dal()
            .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
            .await
            .unwrap(),
        vec![]
    );
}

#[tokio::test]
async fn vote_reorg_never_rehabilitates_unknown_or_noncanonical_provenance() {
    for case in [
        "legacy",
        "unknown_vote",
        "poisoned",
        "stale_proof",
        "stale_vote",
    ] {
        let proof_hash = BlockHash::from_byte_array([10; 32]);
        let retained_hash = BlockHash::from_byte_array([15; 32]);
        let reverted_hash = BlockHash::from_byte_array([20; 32]);
        let (mut db, candidate, id) = legacy_vote_history((case != "legacy").then_some((10, proof_hash))).await;
        let proof = H256::from_low_u64_be(1);
        legacy_vote(&mut db,
                id,
                "retained",
                true,
                (case != "unknown_vote").then_some((15, retained_hash)),
            ).await
            .unwrap();
        legacy_vote(&mut db, id, "reverted", true, Some((20, reverted_hash))).await
            .unwrap();
        if case == "poisoned" {
            // Complete-looking anchors must not upgrade previously lost coverage.
            sqlx::query("UPDATE via_votable_transactions SET source_l1_block_number=NULL,source_l1_block_hash=NULL WHERE id=$1")
                .bind(id).instrument("withdrawal_test_poisoned_source").execute(&mut db).await.unwrap();
        }
        if case == "stale_proof" || case == "stale_vote" {
            sqlx::query("UPDATE via_btc_headers SET hash=$2 WHERE number=$1")
                .bind(if case == "stale_proof" {
                    10_i64
                } else {
                    15_i64
                })
                .bind(BlockHash::from_byte_array([99; 32]).to_string())
                .instrument("withdrawal_test_changed_retained_anchor")
                .execute(&mut db)
                .await
                .unwrap();
        }
        recorded_finality(&mut db, id).await;
        prune_votes_at(&mut db, 15).await;
        let retained = if case == "unknown_vote" { vec![] } else { vec![("retained".into(), true)] };
        assert_eq!(retained_votes(&mut db).await, retained, "{case}");
        assert!(
            db.via_withdrawal_dal()
                .import_complete_batch(wallet().as_bytes(), proof.as_bytes(), &candidate)
                .await
                .is_err(),
            "{case}"
        );
    }
}

#[tokio::test]
async fn legacy_source_and_unknown_vote_cannot_acquire_withdrawal_authority() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut db = pool.connection().await.unwrap();
    let legacy_batch = batch(1, vec![request(1)]);
    let proof = H256::from_low_u64_be(1);
    let hash = BlockHash::from_byte_array([10; 32]);
    canonical(&mut db, 10, hash).await;
    proof_at(&mut db, &legacy_batch, None).await.unwrap();
    proof_at(&mut db, &legacy_batch, Some((10, hash)))
        .await
        .unwrap();
    let id = db
        .via_votes_dal()
        .verify_votable_transaction(1, proof, true)
        .await
        .unwrap();
    legacy_vote(&mut db, id, "fresh", true, Some((10, hash))).await.unwrap();
    recorded_finality(&mut db, id).await;
    assert!(db
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), proof.as_bytes(), &legacy_batch)
        .await
        .is_err());
    assert!(db
        .via_withdrawal_dal()
        .list_finalized_blocks_with_no_bridge_withdrawal(wallet().as_bytes())
        .await
        .unwrap()
        .is_empty());
    assert!(db
        .via_withdrawal_dal()
        .get_finalized_block_and_non_processed_withdrawal(wallet().as_bytes(), 1)
        .await
        .unwrap()
        .is_none());

    let known = batch(2, vec![request(2)]);
    let known_proof = source(&mut db, &known).await;
    let known_id = db
        .via_votes_dal()
        .get_votable_transaction_id(&known_proof)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE via_votable_transactions SET finalized_header_id=NULL WHERE id=$1")
        .bind(known_id)
        .instrument("fixture_legacy_finalization")
        .execute(&mut db)
        .await
        .unwrap();
    legacy_vote(&mut db, known_id, "unknown", true, None).await.unwrap();
    legacy_vote(&mut db, known_id, "fresh", true, Some((10, hash))).await.unwrap();
    assert!(db
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &known_proof, &known)
        .await
        .is_err());
}

#[tokio::test]
async fn stale_scans_cannot_register_legacy_proof() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut db = pool.connection().await.unwrap();
    let old = BlockHash::from_byte_array([10; 32]);
    let current = BlockHash::from_byte_array([11; 32]);
    canonical(&mut db, 10, current).await;
    assert!(proof_at(&mut db, &batch(1, vec![]), Some((10, old)))
        .await
        .is_err());
    assert!(db
        .via_votes_dal()
        .get_votable_transaction_id(H256::from_low_u64_be(1).as_bytes())
        .await
        .unwrap()
        .is_none());
    proof_at(&mut db, &batch(1, vec![]), Some((10, current)))
        .await
        .unwrap();
}

#[tokio::test]
async fn missing_or_changed_source_blocks_import_replay_and_signing() {
    for replacement in [None, Some(BlockHash::from_byte_array([201; 32]))] {
        let (pool, batch, proof) = imported(vec![request(1)]).await;
        let mut db = pool.connection().await.unwrap();
        db.via_withdrawal_dal()
            .admit_withdrawal_attempt(
                wallet().as_bytes(),
                &[2; 32],
                b"admitted",
                &batch.withdrawals,
                &[input(5)],
            )
            .await
            .unwrap();
        db.via_withdrawal_dal()
            .persist_withdrawal_nonces(wallet().as_bytes(), &[2; 32], b"admitted", b"nonces")
            .await
            .unwrap();
        prune_headers(&mut db,-1).await;
        if let Some(hash) = replacement {
            canonical(&mut db, 0, hash).await;
        }
        assert!(db
            .via_withdrawal_dal()
            .import_complete_batch(wallet().as_bytes(), &proof, &batch)
            .await
            .is_err());
        assert!(db
            .via_withdrawal_dal()
            .load_expected_withdrawals(wallet().as_bytes(), &[request(1).id])
            .await
            .is_err());
        assert!(db
            .via_withdrawal_dal()
            .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
            .await
            .unwrap()
            .is_empty());
        assert!(db
            .via_withdrawal_dal()
            .admit_withdrawal_attempt(
                wallet().as_bytes(),
                &[1; 32],
                b"stale",
                &batch.withdrawals,
                &[input(4)]
            )
            .await
            .is_err());
        assert!(db
            .via_withdrawal_dal()
            .mark_withdrawal_may_have_signed(wallet().as_bytes(), &[2; 32], b"admitted")
            .await
            .is_err());
    }
}
