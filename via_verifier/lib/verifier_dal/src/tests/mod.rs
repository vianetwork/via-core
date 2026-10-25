use bitcoin::{hashes::Hash, BlockHash};
use via_btc_client::{
    indexer::replay::{DepositOrigin, DepositPayload, ReplayConflict},
    types::L1ToL2MessageInput,
};
use zksync_db_connection::instrument::InstrumentExt;
use zksync_types::{Address, l1::via_l1::ViaL1Deposit};
use crate::via_transactions_dal::L1ToL2Transaction;
use zksync_db_connection::{connection::Connection, connection_pool::ConnectionPool};
use zksync_types::H256;

use crate::{via_store_mode_dal::StoreMode, Verifier, VerifierDal};

async fn recorded_finality(storage: &mut Connection<'_, Verifier>, id: i64, approved: bool) {
    sqlx::query("UPDATE via_votable_transactions SET is_finalized=$2 WHERE id=$1")
        .bind(id)
        .bind(approved)
        .instrument("recorded_legacy_finality")
        .execute(storage)
        .await
        .unwrap();
}

async fn verified_history(
    storage: &mut Connection<'_, Verifier>,
    number: u32,
    proof: H256,
    id: i64,
    approved: bool,
) {
    storage
        .via_votes_dal()
        .verify_votable_transaction(i64::from(number), proof, approved)
        .await
        .unwrap();
    recorded_finality(storage, id, approved).await;
}

async fn index_history(
    storage: &mut Connection<'_, Verifier>,
    number: u32,
    hash: H256,
    parent: H256,
    proof: H256,
    tag: &str,
) {
    storage
        .via_votes_dal()
        .insert_votable_transaction(
            number,
            hash,
            parent,
            "test_da_id".into(),
            proof,
            format!("test_blob_id_{tag}"),
            format!("test_pubdata_tx_id_{tag}"),
            format!("test_pubdata_blob_id_{tag}"),
            None,
        )
        .await
        .unwrap();
}

// Helper functions for testing
async fn create_test_connection() -> Connection<'static, Verifier> {
    let connection_pool = ConnectionPool::<Verifier>::test_pool().await;
    connection_pool.connection().await.unwrap()
}

#[tokio::test]
async fn proof_replay_preserves_completed_result_and_distinguishes_same_batch_proofs(
) -> anyhow::Result<()> {
    let mut db = create_test_connection().await;
    for number in [1, 2] {
        db.via_votes_dal()
            .insert_votable_transaction(
                1,
                H256::repeat_byte(7),
                H256::zero(),
                "da".into(),
                H256::from_low_u64_be(number),
                format!("proof-{number}"),
                "same-pubdata-tx".into(),
                "same-pubdata-blob".into(),
                None,
            )
            .await?;
    }
    let proof = H256::from_low_u64_be(1);
    let id = db.via_votes_dal().get_votable_transaction_id(proof.as_bytes()).await?.unwrap();
    db.via_votes_dal().verify_votable_transaction(1, proof, true).await?;
    db.via_votes_dal()
        .insert_votable_transaction(
            1,
            H256::repeat_byte(7),
            H256::zero(),
            "da".into(),
            proof,
            "proof-1".into(),
            "same-pubdata-tx".into(),
            "same-pubdata-blob".into(),
            None,
        )
        .await?;
    let error = db
        .via_votes_dal()
        .insert_votable_transaction(
            1,
            H256::repeat_byte(7),
            H256::zero(),
            "da".into(),
            proof,
            "changed-proof".into(),
            "same-pubdata-tx".into(),
            "same-pubdata-blob".into(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error.downcast_ref::<ReplayConflict>().unwrap().field, "proof_blob_id");
    assert_eq!(db.via_votes_dal().get_votable_transaction_id(proof.as_bytes()).await?, Some(id));
    let status = sqlx::query_scalar::<_, bool>(
        "SELECT l1_batch_status FROM via_votable_transactions WHERE id=$1",
    )
    .bind(id)
    .instrument("kept_proof_replay_result")
    .fetch_one(&mut db)
    .await?;
    assert!(status);
    let count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM via_votable_transactions")
        .instrument("distinct_same_batch_proofs")
        .fetch_one(&mut db)
        .await?;
    assert_eq!(count, 2);
    Ok(())
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
        let votable_transaction_id = i64::from(i);
        let l1_batch_hash = H256::random();
        index_history(&mut storage, l1_batch_number, l1_batch_hash, prev_l1_batch_hash, proof_reveal_tx_id, &format!("{i}")).await;
        prev_l1_batch_hash = l1_batch_hash;

        if i >= invalid_l1_batch_id {
            continue;
        }

        verified_history(&mut storage, l1_batch_number, proof_reveal_tx_id, votable_transaction_id, true).await;
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

        index_history(&mut storage, l1_batch_number, l1_batch_hash, prev_l1_batch_hash, proof_reveal_tx_id, &format!("{i}")).await;

        if i == invalid_l1_batch_id {
            prev_l1_batch_hash_number_2 = prev_l1_batch_hash;
        }
        prev_l1_batch_hash = l1_batch_hash;
        indexed.push((votable_transaction_id, proof_reveal_tx_id));
    }

    // Finalize batches 1 and 2, then reject batch 3.
    // The rejection also invalidates indexed batch 4, so a replacement batch 4 can be indexed.
    for (l1_batch_number, (id, proof_reveal_tx_id)) in (1..=invalid_l1_batch_id).zip(indexed) {
        let vote = l1_batch_number != invalid_l1_batch_id;

        verified_history(&mut storage, l1_batch_number, proof_reveal_tx_id, id, vote).await;
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
        let l1_batch_hash = H256::random();
        votable_transaction_id += 1;

        index_history(&mut storage, l1_batch_number, l1_batch_hash, prev_l1_batch_hash, proof_reveal_tx_id, &format!("{i}_fix")).await;
        if i == expected_not_processed_l1_batch_votable_tx_id {
            break;
        }

        prev_l1_batch_hash = l1_batch_hash;
        let vote = true;


        verified_history(&mut storage, l1_batch_number, proof_reveal_tx_id, votable_transaction_id, vote).await;

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

    let rejected_l1_batch = storage
        .via_votes_dal()
        .get_first_rejected_l1_batch()
        .await
        .unwrap();
    assert!(rejected_l1_batch.is_some());
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

    let reopened_strict =
        dal.ensure_proof_verification_mode(&store_mode(false, "regtest", "regtest-genesis")).await;
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
    assert_eq!(persisted, (true, "regtest".into(), "regtest-genesis".into()));
}

#[tokio::test]
async fn strict_store_rejects_development_mode() {
    let mut storage = create_test_connection().await;
    let mut dal = storage.via_store_mode_dal();

    dal.ensure_proof_verification_mode(&store_mode(false, "regtest", "g")).await.unwrap();
    assert!(dal.ensure_proof_verification_mode(&store_mode(true, "regtest", "g")).await.is_err());
}

#[tokio::test]
async fn development_designation_refuses_a_store_with_verdicts() {
    let mut storage = create_test_connection().await;
    let proof_reveal_tx_id = H256::random();
    insert_test_votable_transaction(&mut storage, proof_reveal_tx_id).await;
    storage.via_votes_dal().verify_votable_transaction(1, proof_reveal_tx_id, true).await.unwrap();

    let designated = storage
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&store_mode(true, "regtest", "g"))
        .await;
    assert!(designated.is_err());
    // Refusal rolled its designation back, leaving store free for strict designation.
    // That designation reports adopted legacy verdict once, and restart does not report it again.
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
    let old_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(storage.conn()).await.unwrap();
    let dev = store_mode(true, "regtest", "g");
    storage.via_store_mode_dal().ensure_proof_verification_mode(&dev).await.unwrap();
    let proof_reveal_tx_id = H256::random();
    let id = complete_test_verdict(&mut storage, proof_reveal_tx_id).await;

    storage.via_votes_dal().mark_unverified_dev(id).await.unwrap();
    drop(storage);
    drop(pool);

    // New pool cannot reuse old process's physical database connection.
    let restarted_pool = ConnectionPool::<Verifier>::singleton(database_url).build().await.unwrap();
    let mut storage = restarted_pool.connection().await.unwrap();
    let new_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(storage.conn()).await.unwrap();
    assert_ne!(old_pid, new_pid);
    storage.via_store_mode_dal().ensure_proof_verification_mode(&dev).await.unwrap();
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

async fn complete_test_verdict(storage: &mut Connection<'_, Verifier>, proof: H256) -> i64 {
    insert_test_votable_transaction(storage, proof).await;
    storage.via_votes_dal().verify_votable_transaction(1, proof, true).await.unwrap()
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
    let id = complete_test_verdict(&mut storage, proof_reveal_tx_id).await;
    storage.via_votes_dal().mark_unverified_dev(id).await.unwrap();
    assert!(storage.via_votes_dal().mark_unverified_dev(id + 1).await.is_err());

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
    let id = complete_test_verdict(&mut storage, proof_reveal_tx_id).await;
    assert!(storage.via_votes_dal().mark_unverified_dev(id).await.is_err());
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
    let second_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(second.conn()).await.unwrap();
    let winner = store_mode(true, "regtest", "g");
    let same_mode = second_mode == winner;
    let mut first_tx = first.start_transaction().await.unwrap();
    first_tx.via_store_mode_dal().ensure_proof_verification_mode(&winner).await.unwrap();
    let contender = tokio::spawn(async move {
        second.via_store_mode_dal().ensure_proof_verification_mode(&second_mode).await
    });
    wait_for_database_lock(&mut observer, second_pid).await;
    let proof = H256::random();
    insert_test_votable_transaction(&mut first_tx, proof).await;
    let id = first_tx.via_votes_dal().verify_votable_transaction(1, proof, true).await.unwrap();
    first_tx.via_votes_dal().mark_unverified_dev(id).await.unwrap();
    first_tx.commit().await.unwrap();
    let result =
        tokio::time::timeout(std::time::Duration::from_secs(10), contender).await.unwrap().unwrap();
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
    let rollback_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(rollback.conn()).await.unwrap();
    let mut admission_tx = admission.start_transaction().await.unwrap();
    admission_tx
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&store_mode(true, "regtest", "g"))
        .await
        .unwrap();
    let rollback_task = tokio::spawn(async move {
        let mut tx = rollback.start_transaction().await.unwrap();
        let result = sqlx::Executor::execute(tx.conn(), STORE_MODE_DOWN).await;
        // Rejected migration must roll back, not leave failed transaction open.
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
    admission_tx.via_votes_dal().mark_unverified_dev(id).await.unwrap();
    admission_tx.commit().await.unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_secs(10), rollback_task)
        .await
        .unwrap()
        .unwrap()
        .is_err());
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
    let admission_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(admission.conn()).await.unwrap();
    let mut rollback_tx = rollback.start_transaction().await.unwrap();
    sqlx::raw_sql(STORE_MODE_DOWN).execute(rollback_tx.conn()).await.unwrap();
    let admission_task = tokio::spawn(async move {
        admission
            .via_store_mode_dal()
            .ensure_proof_verification_mode(&store_mode(true, "regtest", "g"))
            .await
    });
    wait_for_database_lock(&mut observer, admission_pid).await;
    rollback_tx.commit().await.unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_secs(10), admission_task)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    // Reapplying schema cannot expose development result with its origin erased.
    let mut restore = observer.start_transaction().await.unwrap();
    sqlx::raw_sql(STORE_MODE_UP).execute(restore.conn()).await.unwrap();
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
    assert_eq!(storage.via_votes_dal().get_verification_watermarks().await.unwrap(), (0, 0));
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
    // Negative local result and positive result on rejected fork are distinct.
    sqlx::query(
        "UPDATE via_votable_transactions SET l1_batch_status = FALSE WHERE l1_batch_number = 1",
    )
    .execute(storage.conn())
    .await
    .unwrap();
    sqlx::query("UPDATE via_votable_transactions SET l1_batch_status = TRUE, is_finalized = FALSE WHERE l1_batch_number = 5")
        .execute(storage.conn()).await.unwrap();
    assert_eq!(storage.via_votes_dal().get_verification_watermarks().await.unwrap(), (4, 0));
    // Legacy and development approvals count despite unresolved lower batch.
    sqlx::query("UPDATE via_votable_transactions SET l1_batch_status = TRUE, is_finalized = TRUE WHERE l1_batch_number = 3")
        .execute(storage.conn()).await.unwrap();
    assert_eq!(storage.via_votes_dal().get_verification_watermarks().await.unwrap(), (4, 3));
    sqlx::query("UPDATE via_votable_transactions SET l1_batch_status = TRUE, unverified_dev = TRUE WHERE l1_batch_number = 4")
        .execute(storage.conn()).await.unwrap();
    assert_eq!(storage.via_votes_dal().get_verification_watermarks().await.unwrap(), (4, 4));
}

#[tokio::test]
async fn strict_rollback_and_reapply_preserve_historical_verdicts() {
    let mut storage = create_test_connection().await;
    let proof = H256::random();
    insert_test_votable_transaction(&mut storage, proof).await;
    storage.via_votes_dal().verify_votable_transaction(1, proof, true).await.unwrap();
    let strict = store_mode(false, "regtest", "g");
    assert!(storage.via_store_mode_dal().ensure_proof_verification_mode(&strict).await.unwrap());
    let mut rollback = storage.start_transaction().await.unwrap();
    sqlx::raw_sql(STORE_MODE_DOWN).execute(rollback.conn()).await.unwrap();
    rollback.commit().await.unwrap();
    let mut restore = storage.start_transaction().await.unwrap();
    sqlx::raw_sql(STORE_MODE_UP).execute(restore.conn()).await.unwrap();
    restore.commit().await.unwrap();
    assert!(storage.via_store_mode_dal().ensure_proof_verification_mode(&strict).await.unwrap());
    let persisted: (bool, bool, i64) = sqlx::query_as(
        "SELECT v.l1_batch_status, v.unverified_dev, m.designated_after_votable_id \
         FROM via_votable_transactions v CROSS JOIN via_verifier_store_mode m WHERE v.id = 1",
    )
    .fetch_one(storage.conn())
    .await
    .unwrap();
    assert_eq!(persisted, (true, false, 1));
}

fn deposit_transaction(height: u64, receiver: Address, vout: usize) -> L1ToL2Transaction {
    let deposit = ViaL1Deposit {
        l2_receiver_address: receiver,
        amount: 50_000,
        calldata: vec![],
        l1_block_number: height,
        tx_index: 1,
        output_vout: vout,
    };
    L1ToL2Transaction {
        priority_id: deposit.priority_id().0 as i64,
        l1_block_number: height as i64,
        tx_id: H256::repeat_byte(8),
        receiver: receiver.to_string(),
        value: 50_000,
        calldata: vec![],
        canonical_tx_hash: deposit.l1_tx().unwrap().hash(),
    }
}

async fn completed_deposit_fixture(
) -> anyhow::Result<(Connection<'static, Verifier>, DepositOrigin, DepositPayload)> {
    let mut db = create_test_connection().await;
    let old = deposit_transaction(10, Address::repeat_byte(7), 0);
    let origin = DepositOrigin { height: 10, hash: BlockHash::from_byte_array([10; 32]), vout: 0 };
    let payload = DepositPayload {
        input: (
            L1ToL2MessageInput {
                receiver_l2_address: Address::repeat_byte(7),
                l2_contract_address: Address::zero(),
                call_data: vec![],
            },
            50_000,
        ),
        public_key: vec![2; 33],
        signature: vec![3; 64],
        sender_script: None,
    };
    assert!(db.via_transactions_dal().insert_transaction(old, &origin, &payload).await?);
    sqlx::query("UPDATE via_transactions SET status=TRUE,l1_batch_number=9,bitcoin_height=NULL,bitcoin_hash=NULL,bitcoin_vout=NULL,bitcoin_payload=NULL")
        .instrument("seed_completed_legacy_deposit").execute(&mut db).await?;
    Ok((db, origin, payload))
}

#[tokio::test]
async fn verifier_remine_preserves_execution_and_rejects_conflicts() -> anyhow::Result<()> {
    let (mut db, mut origin, mut payload) = completed_deposit_fixture().await?;
    let old = deposit_transaction(10, Address::repeat_byte(7), 0);
    let original = (old.priority_id, old.canonical_tx_hash, old.receiver);
    origin.height = 11;
    origin.hash = BlockHash::from_byte_array([11; 32]);
    let mut same_abbreviation = [8; 20];
    same_abbreviation[..2].fill(7);
    same_abbreviation[18..].fill(7);
    for (receiver, field) in [
        (Address::repeat_byte(9), "receiver"),
        (Address::from(same_abbreviation), "retained_execution"),
    ] {
        payload.input.0.receiver_l2_address = receiver;
        let error = db
            .via_transactions_dal()
            .insert_transaction(deposit_transaction(11, receiver, 0), &origin, &payload)
            .await
            .unwrap_err();
        assert_eq!(error.downcast_ref::<ReplayConflict>().unwrap().field, field);
    }
    payload.input.0.receiver_l2_address = Address::repeat_byte(7);
    assert!(
        !db.via_transactions_dal()
            .insert_transaction(
                deposit_transaction(11, Address::repeat_byte(7), 0),
                &origin,
                &payload
            )
            .await?
    );
    payload.public_key[0] ^= 1;
    let error = db
        .via_transactions_dal()
        .insert_transaction(deposit_transaction(11, Address::repeat_byte(7), 0), &origin, &payload)
        .await
        .unwrap_err();
    assert_eq!(error.downcast_ref::<ReplayConflict>().unwrap().field, "public_key");
    let row = sqlx::query("SELECT * FROM via_transactions")
        .instrument("retained_completed_deposit")
        .fetch_optional(&mut db)
        .await?
        .unwrap();
    use sqlx::Row;
    assert_eq!(row.get::<i64, _>("priority_id"), original.0);
    assert_eq!(row.get::<Vec<u8>, _>("canonical_tx_hash"), original.1.as_bytes());
    assert_eq!(row.get::<String, _>("receiver"), original.2);
    assert_eq!(row.get::<i64, _>("l1_block_number"), 10);
    assert_eq!(row.get::<i64, _>("bitcoin_height"), 11);
    assert_eq!(row.get::<i64, _>("l1_batch_number"), 9);
    assert!(row.get::<bool, _>("status"));
    Ok(())
}

#[tokio::test]
async fn verifier_reset_keeps_assigned_deposit() -> anyhow::Result<()> {
    let (mut db, mut origin, payload) = completed_deposit_fixture().await?;
    origin.vout = 1;
    assert!(
        db.via_transactions_dal()
            .insert_transaction(
                deposit_transaction(10, Address::repeat_byte(7), 1),
                &origin,
                &payload
            )
            .await?
    );
    let mut reset = db.start_transaction().await?;
    reset.via_indexer_dal().delete_reader_suffix(0).await?;
    reset.commit().await?;
    let retained = sqlx::query_scalar::<_, bool>(
        "SELECT count(*)=1 AND bool_and(status AND l1_batch_number=9) FROM via_transactions",
    )
    .instrument("retained_completed_assignment")
    .fetch_one(&mut db)
    .await?;
    assert!(retained);
    Ok(())
}
