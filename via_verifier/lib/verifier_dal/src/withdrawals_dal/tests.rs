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

async fn source(
    storage: &mut Connection<'_, Verifier>,
    batch: &CompleteWithdrawalBatch,
) -> Vec<u8> {
    let proof_hash = H256::from_low_u64_be(u64::from(batch.batch_number));
    let proof = proof_hash.as_bytes().to_vec();
    let source_hash = BlockHash::from_byte_array([200; 32]);
    canonical(storage, 1, source_hash).await;
    proof_at(storage, batch, Some((1, source_hash))).await.unwrap();
    let id = approve_fixture(storage, proof_hash).await.unwrap();
    sqlx::query("INSERT INTO via_proof_finalization(proof_inclusion_id,verifier_set_id,decision) SELECT i.id,i.verifier_set_id,TRUE FROM via_proof_inclusions i JOIN via_proof_verdicts v ON v.reader_proof_inclusion_id=i.id WHERE v.id=$1 ON CONFLICT(proof_inclusion_id) DO UPDATE SET decision=TRUE")
        .bind(id).execute(storage.conn()).await.unwrap();
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

async fn remove_canonical_after(storage: &mut Connection<'_, Verifier>, height: i64) {
    let mut tx = storage.start_transaction().await.unwrap();
    tx.via_indexer_dal().lock_source_writer().await.unwrap();
    tx.via_l1_block_dal().delete_l1_blocks(height).await.unwrap();
    tx.commit().await.unwrap();
}

async fn canonical(storage: &mut Connection<'_, Verifier>, height: u32, hash: BlockHash) {
    let mut tx = storage.start_transaction().await.unwrap();
    tx.via_indexer_dal().lock_source_writer().await.unwrap();
    tx.via_l1_block_dal()
        .accept_header(via_reorg::chain::Header {
            height,
            block_hash: hash,
            parent_hash: BlockHash::all_zeros(),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
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
    remove_canonical_after(&mut storage, 9).await;
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
    remove_canonical_after(&mut storage, 9).await;
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
        "UPDATE via_proof_finalization SET decision=FALSE WHERE proof_inclusion_id IN(SELECT reader_proof_inclusion_id FROM via_proof_verdicts WHERE proof_reveal_tx_id=$1)",
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

#[tokio::test]
async fn soft_bitcoin_reorg_keeps_older_unsigned_source_admissible() {
    let (pool, batch, proof) = imported(vec![request(1)]).await;
    let mut storage = pool.connection().await.unwrap();
    canonical(&mut storage, 10, BlockHash::from_byte_array([10; 32])).await;
    let mut reorg = storage.start_transaction().await.unwrap();
    reorg
        .via_withdrawal_dal()
        .invalidate_withdrawals_from_l1_height(10)
        .await
        .unwrap();
    reorg.via_l1_block_dal().delete_l1_blocks(9).await.unwrap();
    reorg.commit().await.unwrap();
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
    sqlx::query("UPDATE via_proof_inclusions SET revoked_at=NOW() WHERE id IN(SELECT reader_proof_inclusion_id FROM via_proof_verdicts WHERE proof_reveal_tx_id=$1)")
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
    include_observation(&mut storage, bridge.as_bytes(), &mut observed, 10, hash).await;
    canonical(&mut storage, 9, BlockHash::from_byte_array([9; 32])).await;
    canonical(&mut storage, 10, BlockHash::from_byte_array([11; 32])).await;
    assert!(!storage
        .via_withdrawal_dal()
        .observed_withdrawal_transaction_has_canonical_inclusion(bridge.as_bytes(), &txid)
        .await
        .unwrap());
    remove_canonical_after(&mut storage, 8).await;
    canonical(&mut storage, 10, hash).await;
    assert!(storage
        .via_withdrawal_dal()
        .observed_withdrawal_transaction_has_canonical_inclusion(bridge.as_bytes(), &txid)
        .await
        .unwrap());
    let mut reorg = storage.start_transaction().await.unwrap();
    reorg
        .via_withdrawal_dal()
        .invalidate_withdrawals_from_l1_height(10)
        .await
        .unwrap();
    reorg.via_l1_block_dal().delete_l1_blocks(9).await.unwrap();
    reorg.commit().await.unwrap();
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
    include_observation(&mut storage, bridge.as_bytes(), &mut observed, 10, hash).await;
    canonical(&mut storage, 10, hash).await;
    assert!(storage
        .via_withdrawal_dal()
        .list_recoverable_withdrawal_attempts(bridge.as_bytes())
        .await
        .unwrap()
        .is_empty());
    remove_canonical_after(&mut storage, 9).await;
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

async fn require_canonical_source(
    storage: &mut Connection<'_, Verifier>,
    height: u32,
    hash: BlockHash,
) -> anyhow::Result<()> {
    let canonical: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM via_l1_blocks WHERE number=$1 AND hash=$2)",
    )
    .bind(i64::from(height))
    .bind(hash.as_byte_array().as_slice())
    .fetch_one(storage.conn())
    .await?;
    anyhow::ensure!(canonical, "fixture source is noncanonical");
    Ok(())
}

async fn include_observation(
    storage: &mut Connection<'_, Verifier>,
    wallet: &[u8],
    observed: &mut WithdrawalObservation,
    height: u32,
    hash: BlockHash,
) {
    observed.inclusion = Some(WithdrawalInclusion { block_hash: hash, block_height: height });
    storage.via_withdrawal_dal().record_withdrawal_observation(wallet, observed).await.unwrap();
}

async fn proof_at(
    storage: &mut Connection<'_, Verifier>,
    batch: &CompleteWithdrawalBatch,
    inclusion: Option<(u32, BlockHash)>,
) -> anyhow::Result<()> {
    let proof = H256::from_low_u64_be(u64::from(batch.batch_number));
    let mut tx = storage.start_transaction().await?;
    tx.via_indexer_dal().lock_source_writer().await?;
    let Some((height, hash)) = inclusion else {
        sqlx::query("INSERT INTO via_proof_verdicts(l1_batch_number,l1_batch_hash,prev_l1_batch_hash,proof_reveal_tx_id,proof_blob_id,pubdata_reveal_tx_id,pubdata_blob_id,da_identifier)
            SELECT $1,$2,$2,$2,$3,$4,$5,'legacy-fixture' WHERE NOT EXISTS(SELECT 1 FROM via_proof_verdicts WHERE proof_reveal_tx_id=$2)")
            .bind(i64::from(batch.batch_number)).bind(proof.as_bytes()).bind(format!("proof-{}",batch.batch_number)).bind(format!("pubdata-{}",batch.batch_number)).bind(&batch.blob_id).execute(tx.conn()).await?;
        tx.commit().await?;
        return Ok(());
    };
    require_canonical_source(&mut tx, height, hash).await?;
    let existing: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM via_wallets WHERE role='verifiers')")
            .fetch_one(tx.conn())
            .await?;
    if !existing {
        via_test_utils::reader_db::bootstrap_verifiers(
            tx.conn(),
            &["retained".into(), "reverted".into()],
            0,
        )
        .await?;
    }
    let mut bytes = proof.0;
    bytes.reverse();
    let identity = Txid::from_byte_array(bytes);
    let origin = via_reorg::chain::MessageRef {
        block: via_reorg::chain::BlockRef { height, block_hash: hash },
        carrier_txid: identity,
        tx_index: batch.batch_number,
        message_index: 0,
    };
    let id = tx
        .via_votes_dal()
        .record_reader_proof(
            identity,
            batch.batch_number,
            &serde_json::json!({"blob":batch.blob_id}),
            origin,
        )
        .await?;
    tx.via_votes_dal()
        .record_votable_transaction(
            batch.batch_number,
            proof,
            proof,
            "fixture".into(),
            proof,
            format!("proof-{}", batch.batch_number),
            format!("pubdata-{}", batch.batch_number),
            batch.blob_id.clone(),
            id,
            crate::via_votes_dal::ProofMembership::Canonical,
        )
        .await?;
    tx.via_indexer_dal().init_indexer_metadata("via_btc_watch", height).await?;
    tx.via_indexer_dal().update_last_processed_l1_block("via_btc_watch", height.max(100)).await?;
    tx.commit().await?;
    Ok(())
}

async fn approve_fixture(
    storage: &mut Connection<'_, Verifier>,
    proof: H256,
) -> anyhow::Result<i64> {
    let mut tx = storage.start_transaction().await?;
    let id = tx
        .via_votes_dal()
        .get_votable_transaction_id(proof.as_bytes())
        .await?
        .ok_or_else(|| anyhow::anyhow!("fixture proof is not current"))?;
    let inputs = tx.via_votes_dal().verification_snapshot(id, 0).await?.unwrap();
    tx.via_votes_dal().mark_proof_verified(id, &inputs).await?;
    tx.commit().await?;
    Ok(id)
}

async fn vote_at(
    storage: &mut Connection<'_, Verifier>,
    id: i64,
    voter: &str,
    approved: bool,
    inclusion: Option<(u32, BlockHash)>,
) -> anyhow::Result<()> {
    let mut tx = storage.start_transaction().await?;
    tx.via_indexer_dal().lock_source_writer().await?;
    let Some((height, hash)) = inclusion else {
        sqlx::query("INSERT INTO via_legacy_votes(votable_transaction_id,verifier_address,vote) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(id).bind(voter).bind(approved).execute(tx.conn()).await?;
        tx.commit().await?;
        return Ok(());
    };
    require_canonical_source(&mut tx, height, hash).await?;
    let proof: Vec<u8> =
        sqlx::query_scalar("SELECT proof_reveal_tx_id FROM via_proof_verdicts WHERE id=$1")
            .bind(id)
            .fetch_one(tx.conn())
            .await?;
    let mut proof = proof;
    proof.reverse();
    let proof = Txid::from_slice(&proof)?;
    let identity = Txid::from_byte_array(
        bitcoin::hashes::sha256::Hash::hash(format!("{id}:{voter}:{approved}").as_bytes())
            .to_byte_array(),
    );
    let index:i64=sqlx::query_scalar("SELECT COALESCE((SELECT MIN(i.tx_index) FROM via_vote_inclusions i JOIN via_vote_evidence e ON e.id=i.vote_id WHERE e.txid=$1 AND i.block_hash=$2),(SELECT COALESCE(MAX(tx_index),0)+100 FROM via_vote_inclusions WHERE block_hash=$2))")
        .bind(identity.as_byte_array().as_slice()).bind(hash.as_byte_array().as_slice()).fetch_one(tx.conn()).await?;
    let origin = via_reorg::chain::MessageRef {
        block: via_reorg::chain::BlockRef { height, block_hash: hash },
        carrier_txid: identity,
        tx_index: u32::try_from(index)?,
        message_index: 0,
    };
    tx.via_votes_dal()
        .record_reader_vote(
            identity,
            proof,
            voter,
            approved,
            &serde_json::json!({"fixture":true}),
            origin,
        )
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn observed_vote_counts(
    storage: &mut Connection<'_, Verifier>,
    id: i64,
) -> anyhow::Result<(i64, i64, i64)> {
    Ok(sqlx::query_as("SELECT COUNT(*) FILTER(WHERE NOT e.approved),COUNT(*) FILTER(WHERE e.approved),COUNT(*) FROM via_vote_evidence e JOIN via_vote_inclusions i ON i.vote_id=e.id JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash JOIN via_proof_inclusions p ON p.id=(SELECT reader_proof_inclusion_id FROM via_proof_verdicts WHERE id=$1) JOIN via_proof_evidence proof ON proof.id=p.proof_id WHERE e.proof_txid=proof.proof_txid AND i.revoked_at IS NULL")
        .bind(id).fetch_one(storage.conn()).await?)
}

// jscpd:ignore-start Each owning crate exercises the production DAL finalizer in a real transaction; shared test-utils cannot depend back on DAL.
async fn finalize_fixture(
    storage: &mut Connection<'_, Verifier>,
    id: i64,
    threshold: f64,
) -> anyhow::Result<bool> {
    let mut tx = storage.start_transaction().await?;
    let result = tx.via_votes_dal().finalize_transaction_if_needed(id, threshold).await?;
    tx.commit().await?;
    Ok(result)
}
// jscpd:ignore-end

async fn vote_sources(storage: &mut Connection<'_, Verifier>) -> [BlockHash; 3] {
    let sources = [10, 15, 20].map(|height| BlockHash::from_byte_array([height; 32]));
    for (height, hash) in [10, 15, 20].into_iter().zip(sources) {
        canonical(storage, height, hash).await;
    }
    sources
}

async fn prune_votes_at(storage: &mut Connection<'_, Verifier>, cut: i64) {
    let mut reorg = storage.start_transaction().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(WITHDRAWAL_INVALIDATION_LOCK_KEY)
        .instrument("withdrawal_test_vote_reorg_gate")
        .execute(&mut reorg)
        .await
        .unwrap();
    reorg
        .via_l1_block_dal()
        .delete_l1_blocks(cut)
        .await
        .unwrap();
    reorg
        .via_votes_dal()
        .revert_votes_after_l1_block(cut)
        .await
        .unwrap();
    reorg.commit().await.unwrap();
}

#[tokio::test]
async fn vote_reorg_rebuilds_tip_from_out_of_order_canonical_votes() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut db = pool.connection().await.unwrap();
    let [proof_hash, retained_hash, reverted_hash] = vote_sources(&mut db).await;
    let candidate = batch(1, vec![request(1)]);
    proof_at(&mut db, &candidate, Some((10, proof_hash)))
        .await
        .unwrap();
    let proof = H256::from_low_u64_be(1);
    let id = approve_fixture(&mut db, proof).await.unwrap();
    vote_at(&mut db, id, "reverted", true, Some((20, reverted_hash))).await.unwrap();
    vote_at(&mut db, id, "retained", true, Some((15, retained_hash))).await.unwrap();
    // A duplicate does not move either immutable inclusion anchor.
    proof_at(&mut db, &candidate, Some((10, proof_hash))).await.unwrap();
    vote_at(&mut db, id, "retained", false, Some((20, reverted_hash))).await.unwrap();
    assert_eq!(
        db.via_votes_dal()
            .get_l1_batch_number_affected_by_proof_reorg(15)
            .await
            .unwrap(),
        None
    );
    assert!(finalize_fixture(&mut db, id, 1.0).await.unwrap());
    prune_votes_at(&mut db, 15).await;
    assert_eq!(
        observed_vote_counts(&mut db,id).await.unwrap(),
        (0, 1, 1)
    );
    assert!(!finalize_fixture(&mut db, id, 1.0).await.unwrap());
    assert_eq!(
        db.via_votes_dal()
            .get_l1_batch_number_affected_by_source_reorg(14)
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        db.via_votes_dal()
            .get_l1_batch_number_affected_by_source_reorg(15)
            .await
            .unwrap(),
        None
    );
    let replacement = BlockHash::from_byte_array([21; 32]);
    canonical(&mut db, 20, replacement).await;
    vote_at(&mut db, id, "reverted", true, Some((20, replacement))).await.unwrap();
    // No second zk-verification is needed to finalize the retained proof.
    assert!(finalize_fixture(&mut db, id, 1.0).await.unwrap());
    db.via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), proof.as_bytes(), &candidate)
        .await
        .unwrap();
    assert_eq!(
        db.via_withdrawal_dal()
            .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
            .await
            .unwrap(),
        candidate.withdrawals
    );
}

#[tokio::test]
async fn vote_reorg_never_rehabilitates_unknown_or_noncanonical_provenance() {
    for case in ["legacy", "unknown_vote", "stale_proof", "stale_vote"] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut db = pool.connection().await.unwrap();
        let [proof_hash, retained_hash, removed_hash] = vote_sources(&mut db).await;
        let candidate = batch(1, vec![request(1)]);
        let proof = H256::from_low_u64_be(1);
        proof_at(&mut db, &candidate, (case != "legacy").then_some((10, proof_hash)))
            .await
            .unwrap();
        let id = if case == "legacy" {
            sqlx::query_scalar("UPDATE via_proof_verdicts SET l1_batch_status=TRUE WHERE proof_reveal_tx_id=$1 RETURNING id").bind(proof.as_bytes()).fetch_one(db.conn()).await.unwrap()
        } else {
            approve_fixture(&mut db, proof).await.unwrap()
        };
        vote_at(
            &mut db,
            id,
            "retained",
            true,
            (case != "unknown_vote").then_some((15, retained_hash)),
        )
        .await
        .unwrap();
        vote_at(&mut db, id, "reverted", true, Some((20, removed_hash))).await.unwrap();
        if case == "stale_proof" || case == "stale_vote" {
            let height = if case == "stale_proof" { 10i64 } else { 15i64 };
            let mut tx = db.start_transaction().await.unwrap();
            tx.via_indexer_dal().lock_source_writer().await.unwrap();
            sqlx::query("DELETE FROM via_l1_blocks WHERE number=$1")
                .bind(height)
                .execute(tx.conn())
                .await
                .unwrap();
            tx.commit().await.unwrap();
            canonical(&mut db, height as u32, BlockHash::from_byte_array([99; 32])).await;
        }
        prune_votes_at(&mut db, 15).await;
        assert!(!finalize_fixture(&mut db, id, 1.0).await.unwrap(), "{case}");
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
async fn source_reorg_boundary_covers_proof_and_latest_inserted_vote() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut db = pool.connection().await.unwrap();
    let hash = BlockHash::from_byte_array([10; 32]);
    let later = BlockHash::from_byte_array([20; 32]);
    canonical(&mut db, 10, hash).await;
    canonical(&mut db, 20, later).await;
    proof_at(&mut db, &batch(2, vec![]), Some((10, hash)))
        .await
        .unwrap();
    assert_eq!(
        db.via_votes_dal()
            .get_l1_batch_number_affected_by_source_reorg(9)
            .await
            .unwrap(),
        Some(2)
    );
    assert_eq!(
        db.via_votes_dal()
            .get_l1_batch_number_affected_by_source_reorg(10)
            .await
            .unwrap(),
        None
    );
    let id = db
        .via_votes_dal()
        .get_votable_transaction_id(H256::from_low_u64_be(2).as_bytes())
        .await
        .unwrap()
        .unwrap();
    vote_at(&mut db, id, "first", true, Some((20, later))).await.unwrap();
    vote_at(&mut db, id, "older", false, Some((10, hash))).await.unwrap();
    // Duplicate observations do not replace a retained vote or erase coverage.
    vote_at(&mut db, id, "first", false, None).await.unwrap();
    assert_eq!(
        observed_vote_counts(&mut db,id).await.unwrap(),
        (1, 1, 2)
    );
    assert_eq!(
        db.via_votes_dal()
            .get_l1_batch_number_affected_by_source_reorg(19)
            .await
            .unwrap(),
        Some(2)
    );
    assert_eq!(
        db.via_votes_dal()
            .get_l1_batch_number_affected_by_source_reorg(20)
            .await
            .unwrap(),
        None
    );
    proof_at(&mut db, &batch(1, vec![]), Some((20, later)))
        .await
        .unwrap();
    assert_eq!(
        db.via_votes_dal()
            .get_l1_batch_number_affected_by_source_reorg(19)
            .await
            .unwrap(),
        Some(1)
    );
}

#[tokio::test]
async fn unproved_votes_do_not_count_or_poison_separately_proved_authority() {
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
    let id = approve_fixture(&mut db, proof).await.unwrap();
    vote_at(&mut db, id, "fresh", true, Some((10, hash))).await.unwrap();
    assert!(!finalize_fixture(&mut db, id, 1.0).await.unwrap());
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
    vote_at(&mut db, known_id, "unknown", true, None).await.unwrap();
    vote_at(&mut db, known_id, "fresh", true, Some((10, hash))).await.unwrap();
    assert_eq!(observed_vote_counts(&mut db, known_id).await.unwrap(), (0, 1, 1));
    db.via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &known_proof, &known)
        .await
        .unwrap();
}

#[tokio::test]
async fn stale_scans_and_stale_parent_cannot_write_source_votes() {
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
    let id = db
        .via_votes_dal()
        .get_votable_transaction_id(H256::from_low_u64_be(1).as_bytes())
        .await
        .unwrap()
        .unwrap();
    assert!(vote_at(&mut db, id, "stale", true, Some((10, old))).await.is_err());
    remove_canonical_after(&mut db, 9).await;
    canonical(&mut db, 20, old).await;
    vote_at(&mut db, id, "newer", true, Some((20, old))).await.unwrap();
    assert!(db
        .via_votes_dal()
        .get_votable_transaction_id(H256::from_low_u64_be(1).as_bytes())
        .await
        .unwrap()
        .is_none());
    assert_eq!(observed_vote_counts(&mut db, id).await.unwrap(), (0, 1, 1));
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
        remove_canonical_after(&mut db,-1).await;
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
