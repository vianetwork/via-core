use zksync_db_connection::{connection::Connection, connection_pool::ConnectionPool};
use zksync_types::H256;

use crate::{via_store_mode_dal::StoreMode, Verifier, VerifierDal};

// Helper functions for testing
async fn create_test_connection() -> Connection<'static, Verifier> {
    let connection_pool = ConnectionPool::<Verifier>::test_pool().await;
    connection_pool.connection().await.unwrap()
}

const TEST_VERIFIER: &str = "fixture-verifier";

#[tokio::test]
async fn observed_votes_and_local_approval_have_separate_owners() {
    let mut storage = create_test_connection().await;
    let proof = H256::random();
    let id = insert_test_proof(&mut storage, 1, H256::random(), H256::zero(), proof).await;
    let mut tx = storage.start_transaction().await.unwrap();
    let (inclusion,raw): (i64,Vec<u8>)=sqlx::query_as("SELECT i.id,e.proof_txid FROM via_proof_verdicts v JOIN via_proof_inclusions i ON i.id=v.reader_proof_inclusion_id JOIN via_proof_evidence e ON e.id=i.proof_id WHERE v.id=$1")
        .bind(id).fetch_one(tx.conn()).await.unwrap();
    use bitcoin::{hashes::Hash, BlockHash, Txid};
    let identity = Txid::from_byte_array([42; 32]);
    tx.via_votes_dal()
        .record_reader_vote(
            identity,
            Txid::from_slice(&raw).unwrap(),
            TEST_VERIFIER,
            true,
            &serde_json::json!({"fixture":true}),
            via_reorg::chain::MessageRef {
                block: via_reorg::chain::BlockRef {
                    height: 1,
                    block_hash: BlockHash::from_byte_array([7; 32]),
                },
                carrier_txid: identity,
                tx_index: 100,
                message_index: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        tx.via_votes_dal().reader_tally(inclusion, [2, 0, 0], 1.0).await.unwrap().finalized,
        Some(true)
    );
    assert!(
        !tx.via_votes_dal().finalize_transaction_if_needed(id, 1.0).await.unwrap(),
        "Bitcoin quorum alone cannot create a local verdict"
    );
    let inputs = tx.via_votes_dal().verification_snapshot(id, 0).await.unwrap().unwrap();
    tx.via_votes_dal().mark_proof_verified(id, &inputs).await.unwrap();
    assert!(tx.via_votes_dal().finalize_transaction_if_needed(id, 1.0).await.unwrap());
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn next_verification_follows_positive_parent_and_keeps_rejected_evidence() {
    let mut storage = create_test_connection().await;
    let mut previous = H256::zero();
    let mut ids = Vec::new();
    for batch in 1..=4 {
        let hash = H256::random();
        let proof = H256::random();
        let id = insert_test_proof(&mut storage, batch, hash, previous, proof).await;
        ids.push(id);
        previous = hash;
        if batch <= 2 {
            approve_test_proof(&mut storage, proof).await;
        }
    }
    assert_eq!(
        storage
            .via_votes_dal()
            .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
            .await
            .unwrap()
            .unwrap()
            .0,
        3
    );
    // Historical negative verdicts remain facts. A child cannot use one as a positive parent.
    sqlx::query("UPDATE via_proof_verdicts SET l1_batch_status=FALSE WHERE id=$1")
        .bind(ids[2])
        .execute(storage.conn())
        .await
        .unwrap();
    assert!(storage
        .via_votes_dal()
        .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
        .await
        .unwrap()
        .is_none());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM via_proof_verdicts")
        .fetch_one(storage.conn())
        .await
        .unwrap();
    assert_eq!(count, 4);
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
    approve_test_proof(&mut storage, proof_reveal_tx_id).await;

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
    let old_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(storage.conn()).await.unwrap();
    let dev = store_mode(true, "regtest", "g");
    storage.via_store_mode_dal().ensure_proof_verification_mode(&dev).await.unwrap();
    let proof_reveal_tx_id = H256::random();
    let id = complete_test_verdict(&mut storage, proof_reveal_tx_id).await;

    let mut marked = storage.start_transaction().await.unwrap();
    marked.via_votes_dal().mark_unverified_dev(id).await.unwrap();
    marked.commit().await.unwrap();
    let mut missing = storage.start_transaction().await.unwrap();
    assert!(missing.via_votes_dal().mark_unverified_dev(id + 1).await.is_err());
    missing.rollback().await.unwrap();
    drop(storage);
    drop(pool);

    // A new pool cannot reuse the old process's physical database connection.
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
    approve_test_proof(storage, proof).await
}

async fn approve_test_proof(storage: &mut Connection<'_, Verifier>, proof: H256) -> i64 {
    let mut tx = storage.start_transaction().await.unwrap();
    let id =
        tx.via_votes_dal().get_votable_transaction_id(proof.as_bytes()).await.unwrap().unwrap();
    let inputs = tx.via_votes_dal().verification_snapshot(id, 0).await.unwrap().unwrap();
    tx.via_votes_dal().mark_proof_verified(id, &inputs).await.unwrap();
    tx.commit().await.unwrap();
    id
}

async fn insert_test_votable_transaction(storage: &mut Connection<'_, Verifier>, proof: H256) {
    insert_test_proof(storage, 1, H256::random(), H256::random(), proof).await;
}

async fn insert_test_proof(
    storage: &mut Connection<'_, Verifier>,
    batch: u32,
    hash: H256,
    parent: H256,
    proof: H256,
) -> i64 {
    use bitcoin::{hashes::Hash, Txid};
    let mut tx = storage.start_transaction().await.unwrap();
    tx.via_indexer_dal().lock_source_writer().await.unwrap();
    let header = via_test_utils::reader_db::first_source(tx.conn(), 7).await.unwrap();
    via_test_utils::reader_db::bootstrap_verifiers(tx.conn(), &[TEST_VERIFIER.into()], 0)
        .await
        .unwrap();
    let index: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(tx_index),0)+1 FROM via_proof_inclusions")
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let mut raw = proof.0;
    raw.reverse();
    let identity = Txid::from_byte_array(raw);
    let origin = via_reorg::chain::MessageRef {
        block: header.into(),
        carrier_txid: identity,
        tx_index: u32::try_from(index).unwrap(),
        message_index: 0,
    };
    let inclusion = tx
        .via_votes_dal()
        .record_reader_proof(
            identity,
            batch,
            &serde_json::json!({"batch_hash":hash,"parent":parent}),
            origin,
        )
        .await
        .unwrap();
    tx.via_votes_dal()
        .record_votable_transaction(
            batch,
            hash,
            parent,
            "fixture".into(),
            proof,
            format!("proof-{proof}"),
            format!("batch-tx-{proof}"),
            format!("batch-{proof}"),
            inclusion,
            crate::via_votes_dal::ProofMembership::Canonical,
        )
        .await
        .unwrap();
    tx.via_indexer_dal().init_indexer_metadata("via_btc_watch", 1).await.unwrap();
    let id =
        tx.via_votes_dal().get_votable_transaction_id(proof.as_bytes()).await.unwrap().unwrap();
    tx.commit().await.unwrap();
    id
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
    let mut tx = storage.start_transaction().await.unwrap();
    assert!(tx.via_votes_dal().mark_unverified_dev(id).await.is_err());
    tx.rollback().await.unwrap();
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
    let id = approve_test_proof(&mut first_tx, proof).await;
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
    approve_test_proof(&mut admission_tx, proof_reveal_tx_id).await;
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
async fn legacy_store_mode_downgrade_refuses_reader_format_without_writes() {
    let mut storage = create_test_connection().await;
    let strict = store_mode(false, "regtest", "g");
    storage.via_store_mode_dal().ensure_proof_verification_mode(&strict).await.unwrap();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(m) FROM via_verifier_store_mode m WHERE id=1")
            .fetch_one(storage.conn())
            .await
            .unwrap();
    let mut tx = storage.start_transaction().await.unwrap();
    assert!(sqlx::raw_sql(STORE_MODE_DOWN).execute(tx.conn()).await.is_err());
    tx.rollback().await.unwrap();
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(m) FROM via_verifier_store_mode m WHERE id=1")
            .fetch_one(storage.conn())
            .await
            .unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn verification_watermarks_exclude_rejections_and_forks_but_are_not_a_prefix() {
    let mut storage = create_test_connection().await;
    assert_eq!(storage.via_votes_dal().get_verification_watermarks().await.unwrap(), (0, 0));
    for batch in 1..=5_u32 {
        insert_test_proof(&mut storage, batch, H256::random(), H256::random(), H256::random())
            .await;
    }
    // A negative local result and a positive result on a rejected fork are distinct.
    sqlx::query("UPDATE via_proof_verdicts SET l1_batch_status = FALSE WHERE l1_batch_number = 1")
        .execute(storage.conn())
        .await
        .unwrap();
    sqlx::query("UPDATE via_proof_verdicts SET l1_batch_status = TRUE WHERE l1_batch_number = 5")
        .execute(storage.conn())
        .await
        .unwrap();
    sqlx::query("INSERT INTO via_proof_finalization(proof_inclusion_id,verifier_set_id,decision) SELECT i.id,i.verifier_set_id,FALSE FROM via_proof_inclusions i JOIN via_proof_verdicts v ON v.reader_proof_inclusion_id=i.id WHERE v.l1_batch_number=5")
        .execute(storage.conn()).await.unwrap();
    assert_eq!(storage.via_votes_dal().get_verification_watermarks().await.unwrap(), (4, 0));
    // Legacy and development approvals count despite the unresolved lower batch.
    sqlx::query("UPDATE via_proof_verdicts SET l1_batch_status = TRUE WHERE l1_batch_number = 3")
        .execute(storage.conn())
        .await
        .unwrap();
    assert_eq!(storage.via_votes_dal().get_verification_watermarks().await.unwrap(), (4, 3));
    sqlx::query("UPDATE via_proof_verdicts SET l1_batch_status = TRUE, unverified_dev = TRUE WHERE l1_batch_number = 4")
        .execute(storage.conn()).await.unwrap();
    assert_eq!(storage.via_votes_dal().get_verification_watermarks().await.unwrap(), (4, 4));
}

#[tokio::test]
async fn strict_rollback_and_reapply_preserve_historical_verdicts() {
    let mut storage = create_test_connection().await;
    let proof = H256::random();
    insert_test_votable_transaction(&mut storage, proof).await;
    approve_test_proof(&mut storage, proof).await;
    let strict = store_mode(false, "regtest", "g");
    assert!(storage.via_store_mode_dal().ensure_proof_verification_mode(&strict).await.unwrap());
    let mut rollback = storage.start_transaction().await.unwrap();
    assert!(sqlx::raw_sql(STORE_MODE_DOWN).execute(rollback.conn()).await.is_err());
    rollback.rollback().await.unwrap();
    assert!(!storage.via_store_mode_dal().ensure_proof_verification_mode(&strict).await.unwrap());
    let persisted: (bool, bool, i64) = sqlx::query_as(
        "SELECT v.l1_batch_status, v.unverified_dev, m.designated_after_votable_id \
         FROM via_votable_transactions v CROSS JOIN via_verifier_store_mode m WHERE v.id = 1",
    )
    .fetch_one(storage.conn())
    .await
    .unwrap();
    assert_eq!(persisted, (true, false, 1));
}

#[tokio::test]
async fn retained_incarnations_split_quorums_and_capture_prefix_changes() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use bitcoin::{hashes::Hash, Txid};
    use serde_json::json;
    use via_reorg::chain::MessageRef;
    use zksync_types::{
        protocol_version::{ProtocolSemanticVersion, VersionPatch},
        ProtocolVersionId,
    };
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut connection = pool.connection().await?;
    let version = ProtocolSemanticVersion::new(ProtocolVersionId::latest(), VersionPatch(0));
    let mut tx = connection.start_transaction().await?;
    tx.via_protocol_versions_dal()
        .save_protocol_version(version, &[1; 32], &[2; 32], &[3; 32], &[4; 32])
        .await?;
    tx.via_protocol_versions_dal()
        .mark_via_bootstrap_version(version, &[1; 32], &[2; 32], &[3; 32], &[4; 32])
        .await?;
    tx.via_protocol_versions_dal().mark_upgrade_as_executed(&[3; 32]).await?;
    let header = via_test_utils::reader_db::first_source(tx.conn(), 1).await?;
    via_test_utils::reader_db::bootstrap_verifiers(
        tx.conn(),
        &["a".into(), "b".into(), "c".into()],
        0,
    )
    .await?;
    let mut origins = Vec::new();
    let mut verdicts = Vec::new();
    for i in 1..=3u8 {
        let proof = Txid::from_byte_array([i + 10; 32]);
        let origin = MessageRef {
            block: header.into(),
            carrier_txid: proof,
            tx_index: u32::from(i),
            message_index: 0,
        };
        let batch = if i < 3 { 1 } else { 2 };
        let payload = json!({"fixture":i});
        let inclusion =
            tx.via_votes_dal().record_reader_proof(proof, batch, &payload, origin).await?;
        let mut display = proof.to_byte_array();
        display.reverse();
        tx.via_votes_dal()
            .record_votable_transaction(
                batch,
                H256::repeat_byte(if i < 3 { 1 } else { 2 }),
                H256::repeat_byte(1),
                "fixture".into(),
                H256(display),
                format!("proof-{i}"),
                format!("batch-tx-{i}"),
                format!("batch-{i}"),
                inclusion,
                crate::via_votes_dal::ProofMembership::Canonical,
            )
            .await?;
        let id = tx
            .via_votes_dal()
            .get_votable_transaction_id(&display)
            .await?
            .context("new proof missing")?;
        origins.push((proof, origin, inclusion, payload));
        verdicts.push(id);
    }
    for (i, (proof_index, voter)) in [(0, "a"), (1, "a"), (1, "b")].into_iter().enumerate() {
        let identity = Txid::from_byte_array([30 + i as u8; 32]);
        let origin = MessageRef {
            block: header.into(),
            carrier_txid: identity,
            tx_index: 10 + i as u32,
            message_index: 0,
        };
        tx.via_votes_dal()
            .record_reader_vote(
                identity,
                origins[proof_index].0,
                voter,
                true,
                &json!({"voter":voter}),
                origin,
            )
            .await?;
    }
    assert_eq!(
        tx.via_votes_dal().reader_tally(origins[0].2, [2, 0, 0], 0.66).await?.finalized,
        None
    );
    assert_eq!(
        tx.via_votes_dal().reader_tally(origins[1].2, [2, 0, 0], 0.66).await?.finalized,
        Some(true)
    );
    for id in &verdicts[..2] {
        let snapshot =
            tx.via_votes_dal().verification_snapshot(*id, 0).await?.context("snapshot missing")?;
        tx.via_votes_dal().mark_proof_verified(*id, &snapshot).await?;
    }
    tx.via_indexer_dal().init_indexer_metadata("via_btc_watch", 1).await?;
    tx.via_votes_dal().recompute_reader_finalization(1, 0.66).await?;
    let decisions: Vec<(i64, Option<bool>)> =
        sqlx::query_as("SELECT id,is_finalized FROM via_votable_transactions ORDER BY id")
            .fetch_all(tx.conn())
            .await?;
    assert_eq!(decisions[0].1, None);
    assert_eq!(decisions[1].1, Some(true));
    let signed = tx
        .via_btc_sender_dal()
        .via_save_btc_inscriptions_request("VoteOnchain".into(), vec![1], 0)
        .await?;
    let unsigned = tx
        .via_btc_sender_dal()
        .via_save_btc_inscriptions_request("VoteOnchain".into(), vec![2], 0)
        .await?;
    for (verdict, request) in [(verdicts[0], signed.id), (verdicts[1], unsigned.id)] {
        tx.via_block_dal().insert_vote_l1_batch_inscription_request_id(verdict,request,zksync_types::via_verifier_btc_inscription_operations::ViaVerifierBtcInscriptionRequestType::VoteOnchain).await?;
        assert!(tx.via_btc_sender_dal().inscription_request_is_eligible(request).await?);
    }
    tx.via_btc_sender_dal()
        .insert_inscription_request_history(
            Txid::from_byte_array([71; 32]).to_string(),
            Txid::from_byte_array([72; 32]).to_string(),
            signed.id,
            vec![3],
            vec![4],
            1,
            1,
        )
        .await?;
    assert_eq!(tx.via_btc_sender_dal().get_inflight_inscriptions().await?.len(), 1);
    assert_eq!(
        tx.via_btc_sender_dal()
            .list_new_inscription_request(10)
            .await?
            .iter()
            .map(|request| request.id)
            .collect::<Vec<_>>(),
        vec![unsigned.id]
    );
    tx.commit().await?;
    let before = connection
        .via_votes_dal()
        .verification_snapshot(verdicts[2], 1)
        .await?
        .context("pending snapshot missing")?;
    assert!(before["deposit_prefix"].as_array().unwrap().is_empty());
    let mut tx = connection.start_transaction().await?;
    tx.via_indexer_dal().lock_source_writer().await?;
    sqlx::query("INSERT INTO via_transactions(priority_id,tx_id,receiver,value,calldata,canonical_tx_hash,l1_block_number) VALUES(1,$1,'fixture',1,'',$1,1)").bind([44u8;32].as_slice()).execute(tx.conn()).await?;
    tx.commit().await?;
    let after = connection
        .via_votes_dal()
        .verification_snapshot(verdicts[2], 1)
        .await?
        .context("changed snapshot missing")?;
    assert_ne!(before, after, "negative/prefix read failed to invalidate");
    let mut tx = connection.start_transaction().await?;
    tx.via_votes_dal().revoke_reader_inclusions_after(0).await?;
    let (proof, origin, old, payload) = &origins[2];
    let new = tx.via_votes_dal().record_reader_proof(*proof, 2, payload, *origin).await?;
    assert_ne!(*old, new, "same-block reacceptance reused its revoked incarnation");
    let retained: i64 =
        sqlx::query_scalar("SELECT count(*) FROM via_proof_verdicts WHERE l1_batch_status=TRUE")
            .fetch_one(tx.conn())
            .await?;
    assert_eq!(retained, 2, "recovery erased completed verdicts");
    assert!(!tx.via_btc_sender_dal().inscription_request_is_eligible(signed.id).await?);
    assert!(!tx.via_btc_sender_dal().inscription_request_is_eligible(unsigned.id).await?);
    assert_eq!(
        tx.via_btc_sender_dal().get_inflight_inscriptions().await?.len(),
        1,
        "orphaning erased physical inflight risk"
    );
    assert!(tx.via_btc_sender_dal().list_new_inscription_request(10).await?.is_empty());
    tx.commit().await?;
    Ok(())
}

#[tokio::test]
async fn deposit_queue_replay_and_unbatched_undo_keep_stable_outpoint() -> anyhow::Result<()> {
    use bitcoin::{hashes::Hash, BlockHash, Txid};
    use via_reorg::chain::{Header, MessageRef};
    use crate::via_transactions_dal::L1ToL2Transaction;
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut connection = pool.connection().await?;
    let mut tx = connection.start_transaction().await?;
    tx.via_indexer_dal().lock_source_writer().await?;
    let header = via_test_utils::reader_db::first_source(tx.conn(), 1).await?;
    let carrier = Txid::from_byte_array([55; 32]);
    let operation = H256::repeat_byte(7);
    let origin =
        MessageRef { block: header.into(), carrier_txid: carrier, tx_index: 1, message_index: 0 };
    let payload = serde_json::json!({"receiver":"fixture","value":10,"calldata":[]});
    tx.via_transactions_dal()
        .record_deposit_evidence(carrier, 0, &payload, origin, operation)
        .await?;
    let queued = || L1ToL2Transaction {
        priority_id: 1,
        tx_id: H256::repeat_byte(55),
        receiver: "fixture".into(),
        value: 10,
        calldata: vec![],
        canonical_tx_hash: operation,
        l1_block_number: 1,
    };
    tx.via_transactions_dal().insert_transaction(queued()).await?;
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(t) FROM via_transactions t")
            .fetch_one(tx.conn())
            .await?;
    tx.via_transactions_dal().insert_transaction(queued()).await?;
    let after: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(t) FROM via_transactions t")
        .fetch_one(tx.conn())
        .await?;
    assert_eq!(before, after);
    let mut conflicting = tx.start_transaction().await?;
    assert!(conflicting
        .via_transactions_dal()
        .insert_transaction(L1ToL2Transaction { receiver: "different".into(), ..queued() })
        .await
        .is_err());
    conflicting.rollback().await?;
    tx.via_transactions_dal().delete_transactions(0).await?;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM via_transactions").fetch_one(tx.conn()).await?;
    assert_eq!(count, 0);
    let next = Header {
        height: 2,
        block_hash: BlockHash::from_byte_array([2; 32]),
        parent_hash: header.block_hash,
    };
    via_test_utils::reader_db::header(tx.conn(), next, true).await?;
    let next_operation = H256::repeat_byte(8);
    tx.via_transactions_dal()
        .record_deposit_evidence(
            carrier,
            0,
            &payload,
            MessageRef { block: next.into(), ..origin },
            next_operation,
        )
        .await?;
    tx.via_transactions_dal()
        .insert_transaction(L1ToL2Transaction {
            priority_id: 2,
            canonical_tx_hash: next_operation,
            l1_block_number: 2,
            ..queued()
        })
        .await?;
    assert!(tx.via_transactions_dal().insert_transaction(queued()).await.is_err());
    let evidence: i64 = sqlx::query_scalar("SELECT count(*) FROM via_deposit_evidence")
        .fetch_one(tx.conn())
        .await?;
    let inclusions: i64 = sqlx::query_scalar("SELECT count(*) FROM via_deposit_inclusions")
        .fetch_one(tx.conn())
        .await?;
    assert_eq!((evidence, inclusions), (1, 2));
    tx.commit().await?;
    Ok(())
}
