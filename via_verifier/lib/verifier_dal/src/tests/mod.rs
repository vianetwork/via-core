use rand::random;
use zksync_db_connection::{connection::Connection, connection_pool::ConnectionPool};
use zksync_types::H256;

use crate::{via_store_mode_dal::StoreMode, Verifier, VerifierDal};

// Helper functions for testing
async fn create_test_connection() -> Connection<'static, Verifier> {
    let connection_pool = ConnectionPool::<Verifier>::test_pool().await;
    connection_pool.connection().await.unwrap()
}

fn mock_via_vote() -> (u32, H256, String, bool) {
    (
        1, // l1_batch_number
        H256::random(),
        "0x1234567890123456789012345678901234567890".to_string(), // verifier_address
        random::<bool>(),
    )
}

#[tokio::test]
async fn test_via_vote_workflow() {
    let mut storage = create_test_connection().await;

    // Create test data
    let (l1_batch_number, proof_reveal_tx_id, verifier_address, vote) = mock_via_vote();

    // First insert a votable transaction
    storage
        .via_votes_dal()
        .insert_votable_transaction(
            l1_batch_number,
            H256::random(),
            H256::random(),
            "test_da_id".to_string(),
            proof_reveal_tx_id,
            "test_blob_id".to_string(),
            "test_pubdata_tx_id".to_string(),
            "test_pubdata_blob_id".to_string(),
            None,
        )
        .await
        .unwrap();
    let votable_transaction_id = 1;

    // Test inserting a vote
    storage
        .via_votes_dal()
        .insert_vote(votable_transaction_id, &verifier_address, vote, None)
        .await
        .unwrap();
    storage
        .via_votes_dal()
        .verify_votable_transaction(i64::from(l1_batch_number), proof_reveal_tx_id, vote)
        .await
        .unwrap();

    // Test getting vote count
    let (_, ok_votes, total_votes) = storage
        .via_votes_dal()
        .get_vote_count(votable_transaction_id)
        .await
        .unwrap();

    assert_eq!(total_votes, 1);
    assert_eq!(ok_votes, if vote { 1 } else { 0 });

    // Test finalizing transaction
    let is_finalized = storage
        .via_votes_dal()
        .finalize_transaction_if_needed(votable_transaction_id, 0.5, 1)
        .await
        .unwrap();

    assert_eq!(is_finalized, vote);
}

#[tokio::test]
async fn test_get_first_not_verified_l1_batch_in_canonical_inscription_chain() {
    let mut storage = create_test_connection().await;

    let invalid_l1_batch_id = 3;
    let mut prev_l1_batch_hash = H256::random();

    // Insert 4 votable transactions, the first 2 transactions are finalized.
    for i in 1..5 {
        let l1_batch_number = i;
        let proof_reveal_tx_id = H256::random();
        let verifier_address = "0x1234567890123456789012345678901234567890".to_string();
        let votable_transaction_id = i64::from(i);
        let l1_batch_hash = H256::random();
        storage
            .via_votes_dal()
            .insert_votable_transaction(
                l1_batch_number,
                l1_batch_hash,
                prev_l1_batch_hash,
                "test_da_id".to_string(),
                proof_reveal_tx_id,
                format!("test_blob_id_{i}").to_string(),
                format!("test_pubdata_tx_id_{i}").to_string(),
                format!("test_pubdata_blob_id_{i}").to_string(),
                None,
            )
            .await
            .unwrap();
        prev_l1_batch_hash = l1_batch_hash;

        if i >= invalid_l1_batch_id {
            continue;
        }

        storage
            .via_votes_dal()
            .insert_vote(votable_transaction_id, &verifier_address, true, None)
            .await
            .unwrap();
        storage
            .via_votes_dal()
            .verify_votable_transaction(i64::from(l1_batch_number), proof_reveal_tx_id, true)
            .await
            .unwrap();
        storage
            .via_votes_dal()
            .finalize_transaction_if_needed(votable_transaction_id, 1.0, 1)
            .await
            .unwrap();
    }

    let res = storage
        .via_votes_dal()
        .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
        .await
        .unwrap();
    assert!(res.is_some());
    // We expect that the next valid batch to process is batch 3
    assert_eq!(res.unwrap().0, 3);
}

#[tokio::test]
async fn test_get_first_not_verified_l1_batch_in_canonical_inscription_chain_when_invalid_batch() {
    let mut storage = create_test_connection().await;

    let invalid_l1_batch_id = 3;
    let mut prev_l1_batch_hash = H256::random();

    // Store the l1_batch_number=2 "l1_batch_hash" to reuse it when resend a valid l1_batch_number=3
    let mut prev_l1_batch_hash_number_2 = H256::random();

    let mut votable_transaction_id: i64 = 0;
    let mut indexed = Vec::new();
    // Index all 4 batches before any verdict, as the watcher runs ahead of the verifier.
    for i in 1..5 {
        let l1_batch_number = i;
        let proof_reveal_tx_id = H256::random();
        let l1_batch_hash = H256::random();
        votable_transaction_id += 1;

        storage
            .via_votes_dal()
            .insert_votable_transaction(
                l1_batch_number,
                l1_batch_hash,
                prev_l1_batch_hash,
                "test_da_id".to_string(),
                proof_reveal_tx_id,
                format!("test_blob_id_{i}").to_string(),
                format!("test_pubdata_tx_id_{i}").to_string(),
                format!("test_pubdata_blob_id_{i}").to_string(),
                None,
            )
            .await
            .unwrap();

        if i == invalid_l1_batch_id {
            prev_l1_batch_hash_number_2 = prev_l1_batch_hash;
        }
        prev_l1_batch_hash = l1_batch_hash;
        indexed.push((votable_transaction_id, proof_reveal_tx_id));
    }

    // Finalize batches 1 and 2, then reject batch 3.
    // The rejection also invalidates indexed batch 4, so a replacement batch 4 can be indexed.
    for (l1_batch_number, (id, proof_reveal_tx_id)) in (1..=invalid_l1_batch_id).zip(indexed) {
        let verifier_address = "0x1234567890123456789012345678901234567890".to_string();
        let vote = l1_batch_number != invalid_l1_batch_id;

        storage
            .via_votes_dal()
            .insert_vote(id, &verifier_address, vote, None)
            .await
            .unwrap();
        storage
            .via_votes_dal()
            .verify_votable_transaction(i64::from(l1_batch_number), proof_reveal_tx_id, vote)
            .await
            .unwrap();
        storage
            .via_votes_dal()
            .finalize_transaction_if_needed(id, 1.0, 1)
            .await
            .unwrap();
    }

    let res = storage
        .via_votes_dal()
        .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
        .await
        .unwrap();
    assert!(res.is_none());

    let rejected_l1_batch = storage
        .via_votes_dal()
        .get_first_rejected_l1_batch()
        .await
        .unwrap();
    assert!(rejected_l1_batch.is_some());
    assert_eq!(rejected_l1_batch.unwrap().0, i64::from(invalid_l1_batch_id));

    // A reinserted child of a rejected batch remains ineligible while pending.
    let mut reinserted = storage.start_transaction().await.unwrap();
    sqlx::query(
        "UPDATE via_votable_transactions SET l1_batch_status = NULL, is_finalized = NULL WHERE l1_batch_number = 4",
    )
    .execute(reinserted.conn())
    .await
    .unwrap();
    assert!(reinserted
        .via_votes_dal()
        .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
        .await
        .unwrap()
        .is_none());
    reinserted.rollback().await.unwrap();

    prev_l1_batch_hash = prev_l1_batch_hash_number_2;
    let expected_not_processed_l1_batch_votable_tx_id = 5;

    for i in 3..6 {
        let l1_batch_number = i;
        let proof_reveal_tx_id = H256::random();
        let verifier_address = "0x1234567890123456789012345678901234567890".to_string();
        let l1_batch_hash = H256::random();
        votable_transaction_id += 1;

        storage
            .via_votes_dal()
            .insert_votable_transaction(
                l1_batch_number,
                l1_batch_hash,
                prev_l1_batch_hash,
                "test_da_id".to_string(),
                proof_reveal_tx_id,
                format!("test_blob_id_{i}_fix").to_string(),
                format!("test_pubdata_tx_id_{i}_fix").to_string(),
                format!("test_pubdata_blob_id_{i}_fix").to_string(),
                None,
            )
            .await
            .unwrap();
        if i == expected_not_processed_l1_batch_votable_tx_id {
            break;
        }

        prev_l1_batch_hash = l1_batch_hash;
        let vote = true;

        storage
            .via_votes_dal()
            .insert_vote(votable_transaction_id, &verifier_address, vote, None)
            .await
            .unwrap();

        storage
            .via_votes_dal()
            .verify_votable_transaction(i64::from(l1_batch_number), proof_reveal_tx_id, vote)
            .await
            .unwrap();

        storage
            .via_votes_dal()
            .finalize_transaction_if_needed(votable_transaction_id, 1.0, 1)
            .await
            .unwrap();

        // storage
        //     .via_bridge_dal()
        //     .update_bridge_tx(
        //         &proof_reveal_tx_id.as_bytes().to_vec(),
        //         0,
        //         H256::zero().as_bytes(),
        //     )
        //     .await
        //     .unwrap();
    }

    let res = storage
        .via_votes_dal()
        .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
        .await
        .unwrap();

    // We expect that the next valid batch to process is batch 5
    assert_eq!(
        res.unwrap().0,
        i64::from(expected_not_processed_l1_batch_votable_tx_id)
    );

    // Delete previous invalid transactions
    storage
        .via_votes_dal()
        .delete_invalid_votable_transactions_if_exists()
        .await
        .unwrap();

    let rejected_l1_batch = storage
        .via_votes_dal()
        .get_first_rejected_l1_batch()
        .await
        .unwrap();
    assert!(rejected_l1_batch.is_none());
}

fn store_mode(dev: bool, network: &str, genesis: &str) -> StoreMode {
    StoreMode {
        proof_verification_dev_mode: dev,
        bitcoin_network: network.to_string(),
        bitcoin_genesis_hash: genesis.to_string(),
    }
}

#[tokio::test]
async fn store_mode_is_designated_once_and_enforced() {
    let mut storage = create_test_connection().await;
    let mut dal = storage.via_store_mode_dal();
    let dev = store_mode(true, "regtest", "regtest-genesis");

    assert!(dal
        .ensure_proof_verification_mode(&store_mode(true, "testnet4", "t4-genesis"))
        .await
        .is_err());
    dal.ensure_proof_verification_mode(&dev).await.unwrap();
    dal.ensure_proof_verification_mode(&dev).await.unwrap();

    let reopened_strict = dal
        .ensure_proof_verification_mode(&store_mode(false, "regtest", "regtest-genesis"))
        .await;
    assert!(reopened_strict.is_err());
    assert!(dal
        .ensure_proof_verification_mode(&store_mode(true, "regtest", "other-genesis"))
        .await
        .is_err());
    let persisted: (bool, String, String) = sqlx::query_as(
        "SELECT proof_verification_dev_mode, bitcoin_network, bitcoin_genesis_hash \
         FROM via_verifier_store_mode WHERE id = 1",
    )
    .fetch_one(storage.conn())
    .await
    .unwrap();
    assert_eq!(
        persisted,
        (true, "regtest".into(), "regtest-genesis".into())
    );
}

#[tokio::test]
async fn strict_store_rejects_development_mode() {
    let mut storage = create_test_connection().await;
    let mut dal = storage.via_store_mode_dal();

    dal.ensure_proof_verification_mode(&store_mode(false, "regtest", "g"))
        .await
        .unwrap();
    assert!(dal
        .ensure_proof_verification_mode(&store_mode(true, "regtest", "g"))
        .await
        .is_err());
}

#[tokio::test]
async fn development_designation_refuses_a_store_with_verdicts() {
    let mut storage = create_test_connection().await;
    let proof_reveal_tx_id = H256::random();
    insert_test_votable_transaction(&mut storage, proof_reveal_tx_id).await;
    storage
        .via_votes_dal()
        .verify_votable_transaction(1, proof_reveal_tx_id, true)
        .await
        .unwrap();

    let designated = storage
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&store_mode(true, "regtest", "g"))
        .await;
    assert!(designated.is_err());
    // The refusal rolled its designation back, so the store can still be designated strict.
    // That designation reports the adopted legacy verdict once, and a restart does not report it again.
    let strict = store_mode(false, "regtest", "g");
    let mut dal = storage.via_store_mode_dal();
    assert!(dal.ensure_proof_verification_mode(&strict).await.unwrap());
    assert!(!dal.ensure_proof_verification_mode(&strict).await.unwrap());
}

#[tokio::test]
async fn development_store_restarts_after_its_own_verdicts() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let database_url = pool.database_url().clone();
    let mut storage = pool.connection().await.unwrap();
    let old_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(storage.conn())
        .await
        .unwrap();
    let dev = store_mode(true, "regtest", "g");
    storage
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&dev)
        .await
        .unwrap();
    let proof_reveal_tx_id = H256::random();
    insert_test_votable_transaction(&mut storage, proof_reveal_tx_id).await;
    let id = storage
        .via_votes_dal()
        .verify_votable_transaction(1, proof_reveal_tx_id, true)
        .await
        .unwrap();

    storage
        .via_votes_dal()
        .mark_unverified_dev(id)
        .await
        .unwrap();
    drop(storage);
    drop(pool);

    // A new pool cannot reuse the old process's physical database connection.
    let restarted_pool = ConnectionPool::<Verifier>::singleton(database_url)
        .build()
        .await
        .unwrap();
    let mut storage = restarted_pool.connection().await.unwrap();
    let new_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(storage.conn())
        .await
        .unwrap();
    assert_ne!(old_pid, new_pid);
    storage
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&dev)
        .await
        .unwrap();
    assert!(storage
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&store_mode(false, "regtest", "g"))
        .await
        .is_err());
    let persisted: (bool, bool) = sqlx::query_as(
        "SELECT v.l1_batch_status, v.unverified_dev FROM via_votable_transactions v WHERE v.id = $1",
    )
    .bind(id)
    .fetch_one(storage.conn())
    .await
    .unwrap();
    assert_eq!(persisted, (true, true));
}

async fn insert_test_votable_transaction(
    storage: &mut Connection<'_, Verifier>,
    proof_reveal_tx_id: H256,
) {
    storage
        .via_votes_dal()
        .insert_votable_transaction(
            1,
            H256::random(),
            H256::random(),
            "test_da_id".to_string(),
            proof_reveal_tx_id,
            "test_blob_id".to_string(),
            "test_pubdata_tx_id".to_string(),
            "test_pubdata_blob_id".to_string(),
            None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn unverified_dev_marker_is_recorded_with_the_verdict() {
    let mut storage = create_test_connection().await;
    storage
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&store_mode(true, "regtest", "g"))
        .await
        .unwrap();
    let proof_reveal_tx_id = H256::random();
    insert_test_votable_transaction(&mut storage, proof_reveal_tx_id).await;
    let id = storage
        .via_votes_dal()
        .verify_votable_transaction(1, proof_reveal_tx_id, true)
        .await
        .unwrap();
    storage
        .via_votes_dal()
        .mark_unverified_dev(id)
        .await
        .unwrap();
    assert!(storage
        .via_votes_dal()
        .mark_unverified_dev(id + 1)
        .await
        .is_err());

    let marked: bool =
        sqlx::query_scalar("SELECT unverified_dev FROM via_votable_transactions WHERE id = $1")
            .bind(id)
            .fetch_one(storage.conn())
            .await
            .unwrap();
    assert!(marked);
}

#[tokio::test]
async fn strict_store_refuses_the_unverified_dev_marker() {
    let mut storage = create_test_connection().await;
    storage
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&store_mode(false, "regtest", "g"))
        .await
        .unwrap();
    let proof_reveal_tx_id = H256::random();
    insert_test_votable_transaction(&mut storage, proof_reveal_tx_id).await;
    let id = storage
        .via_votes_dal()
        .verify_votable_transaction(1, proof_reveal_tx_id, true)
        .await
        .unwrap();
    assert!(storage
        .via_votes_dal()
        .mark_unverified_dev(id)
        .await
        .is_err());
}

async fn wait_for_database_lock(observer: &mut Connection<'_, Verifier>, pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = $1 AND NOT granted)",
            )
            .bind(pid)
            .fetch_one(observer.conn())
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("database operation never reached its conflicting lock");
}

async fn concurrent_store_admission(second_mode: StoreMode) {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut first = pool.connection().await.unwrap();
    let mut second = pool.connection().await.unwrap();
    let mut observer = pool.connection().await.unwrap();
    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(second.conn())
        .await
        .unwrap();
    let winner = store_mode(true, "regtest", "g");
    let same_mode = second_mode == winner;
    let mut first_tx = first.start_transaction().await.unwrap();
    first_tx
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&winner)
        .await
        .unwrap();
    let contender = tokio::spawn(async move {
        second
            .via_store_mode_dal()
            .ensure_proof_verification_mode(&second_mode)
            .await
    });
    wait_for_database_lock(&mut observer, second_pid).await;
    let proof = H256::random();
    insert_test_votable_transaction(&mut first_tx, proof).await;
    let id = first_tx
        .via_votes_dal()
        .verify_votable_transaction(1, proof, true)
        .await
        .unwrap();
    first_tx
        .via_votes_dal()
        .mark_unverified_dev(id)
        .await
        .unwrap();
    first_tx.commit().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), contender)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.is_ok(), same_mode);
    let designation: (bool, String, String) = sqlx::query_as(
        "SELECT proof_verification_dev_mode, bitcoin_network, bitcoin_genesis_hash \
         FROM via_verifier_store_mode WHERE id = 1",
    )
    .fetch_one(observer.conn())
    .await
    .unwrap();
    assert_eq!(designation, (true, "regtest".into(), "g".into()));
    let approval: (bool, bool) = sqlx::query_as(
        "SELECT l1_batch_status, unverified_dev FROM via_votable_transactions WHERE id = $1",
    )
    .bind(id)
    .fetch_one(observer.conn())
    .await
    .unwrap();
    assert_eq!(approval, (true, true));
}

#[tokio::test]
async fn concurrent_same_mode_first_starts_are_both_admitted() {
    concurrent_store_admission(store_mode(true, "regtest", "g")).await;
}

#[tokio::test]
async fn concurrent_conflicting_first_starts_preserve_one_winner() {
    concurrent_store_admission(store_mode(false, "regtest", "g")).await;
}

const STORE_MODE_DOWN: &str =
    include_str!("../../migrations/20260925000000_proof_verification_store_mode.down.sql");
const STORE_MODE_UP: &str =
    include_str!("../../migrations/20260925000000_proof_verification_store_mode.up.sql");

#[tokio::test]
async fn rollback_waits_for_admission_and_preserves_development_approval() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut admission = pool.connection().await.unwrap();
    let mut rollback = pool.connection().await.unwrap();
    let mut observer = pool.connection().await.unwrap();
    let proof_reveal_tx_id = H256::random();
    insert_test_votable_transaction(&mut admission, proof_reveal_tx_id).await;
    let rollback_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(rollback.conn())
        .await
        .unwrap();
    let mut admission_tx = admission.start_transaction().await.unwrap();
    admission_tx
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&store_mode(true, "regtest", "g"))
        .await
        .unwrap();
    let rollback_task = tokio::spawn(async move {
        let mut tx = rollback.start_transaction().await.unwrap();
        let result = sqlx::Executor::execute(tx.conn(), STORE_MODE_DOWN).await;
        // A rejected migration must roll back, not leave a failed transaction open.
        drop(tx);
        result
    });
    wait_for_database_lock(&mut observer, rollback_pid).await;
    let id: i64 =
        sqlx::query_scalar("SELECT id FROM via_votable_transactions WHERE proof_reveal_tx_id = $1")
            .bind(proof_reveal_tx_id.as_bytes())
            .fetch_one(admission_tx.conn())
            .await
            .unwrap();
    admission_tx
        .via_votes_dal()
        .verify_votable_transaction(1, proof_reveal_tx_id, true)
        .await
        .unwrap();
    admission_tx
        .via_votes_dal()
        .mark_unverified_dev(id)
        .await
        .unwrap();
    admission_tx.commit().await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(10), rollback_task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let persisted: (bool, bool, bool) = sqlx::query_as(
        "SELECT m.proof_verification_dev_mode, v.l1_batch_status, v.unverified_dev \
         FROM via_verifier_store_mode m CROSS JOIN via_votable_transactions v \
         WHERE m.id = 1 AND v.id = $1",
    )
    .bind(id)
    .fetch_one(observer.conn())
    .await
    .unwrap();
    assert_eq!(persisted, (true, true, true));
    assert!(observer
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&store_mode(false, "regtest", "g"))
        .await
        .is_err());
}

#[tokio::test]
async fn rollback_winning_before_admission_cannot_erase_a_development_result() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut rollback = pool.connection().await.unwrap();
    let mut admission = pool.connection().await.unwrap();
    let mut observer = pool.connection().await.unwrap();
    let admission_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(admission.conn())
        .await
        .unwrap();
    let mut rollback_tx = rollback.start_transaction().await.unwrap();
    sqlx::raw_sql(STORE_MODE_DOWN)
        .execute(rollback_tx.conn())
        .await
        .unwrap();
    let admission_task = tokio::spawn(async move {
        admission
            .via_store_mode_dal()
            .ensure_proof_verification_mode(&store_mode(true, "regtest", "g"))
            .await
    });
    wait_for_database_lock(&mut observer, admission_pid).await;
    rollback_tx.commit().await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(10), admission_task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    // Reapplying the schema cannot expose a development result with its origin erased.
    let mut restore = observer.start_transaction().await.unwrap();
    sqlx::raw_sql(STORE_MODE_UP)
        .execute(restore.conn())
        .await
        .unwrap();
    restore.commit().await.unwrap();
    observer
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&store_mode(false, "regtest", "g"))
        .await
        .unwrap();
    let verdicts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM via_votable_transactions WHERE l1_batch_status IS NOT NULL",
    )
    .fetch_one(observer.conn())
    .await
    .unwrap();
    assert_eq!(verdicts, 0);
}

#[tokio::test]
async fn verification_watermarks_exclude_rejections_and_forks_but_are_not_a_prefix() {
    let mut storage = create_test_connection().await;
    assert_eq!(
        storage
            .via_votes_dal()
            .get_verification_watermarks()
            .await
            .unwrap(),
        (0, 0)
    );
    for batch in 1..=5_u32 {
        storage
            .via_votes_dal()
            .insert_votable_transaction(
                batch,
                H256::random(),
                H256::random(),
                "fixture".into(),
                H256::random(),
                format!("proof_{batch}"),
                format!("tx_{batch}"),
                format!("blob_{batch}"),
                None,
            )
            .await
            .unwrap();
    }
    // A negative local result and a positive result on a rejected fork are distinct.
    sqlx::query(
        "UPDATE via_votable_transactions SET l1_batch_status = FALSE WHERE l1_batch_number = 1",
    )
    .execute(storage.conn())
    .await
    .unwrap();
    sqlx::query("UPDATE via_votable_transactions SET l1_batch_status = TRUE, is_finalized = FALSE WHERE l1_batch_number = 5")
        .execute(storage.conn()).await.unwrap();
    assert_eq!(
        storage
            .via_votes_dal()
            .get_verification_watermarks()
            .await
            .unwrap(),
        (4, 0)
    );
    // Legacy and development approvals count despite the unresolved lower batch.
    sqlx::query("UPDATE via_votable_transactions SET l1_batch_status = TRUE, is_finalized = TRUE WHERE l1_batch_number = 3")
        .execute(storage.conn()).await.unwrap();
    assert_eq!(
        storage
            .via_votes_dal()
            .get_verification_watermarks()
            .await
            .unwrap(),
        (4, 3)
    );
    sqlx::query("UPDATE via_votable_transactions SET l1_batch_status = TRUE, unverified_dev = TRUE WHERE l1_batch_number = 4")
        .execute(storage.conn()).await.unwrap();
    assert_eq!(
        storage
            .via_votes_dal()
            .get_verification_watermarks()
            .await
            .unwrap(),
        (4, 4)
    );
}

#[tokio::test]
async fn strict_rollback_and_reapply_preserve_historical_verdicts() {
    let mut storage = create_test_connection().await;
    let proof = H256::random();
    insert_test_votable_transaction(&mut storage, proof).await;
    storage
        .via_votes_dal()
        .verify_votable_transaction(1, proof, true)
        .await
        .unwrap();
    let strict = store_mode(false, "regtest", "g");
    assert!(storage
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&strict)
        .await
        .unwrap());
    let mut rollback = storage.start_transaction().await.unwrap();
    sqlx::raw_sql(STORE_MODE_DOWN)
        .execute(rollback.conn())
        .await
        .unwrap();
    rollback.commit().await.unwrap();
    let mut restore = storage.start_transaction().await.unwrap();
    sqlx::raw_sql(STORE_MODE_UP)
        .execute(restore.conn())
        .await
        .unwrap();
    restore.commit().await.unwrap();
    assert!(storage
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&strict)
        .await
        .unwrap());
    let persisted: (bool, bool, i64) = sqlx::query_as(
        "SELECT v.l1_batch_status, v.unverified_dev, m.designated_after_votable_id \
         FROM via_votable_transactions v CROSS JOIN via_verifier_store_mode m WHERE v.id = 1",
    )
    .fetch_one(storage.conn())
    .await
    .unwrap();
    assert_eq!(persisted, (true, false, 1));
}
