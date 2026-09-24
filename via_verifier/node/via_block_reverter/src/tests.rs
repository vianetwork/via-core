use bitcoin::{hashes::Hash, Address, Amount, Network, ScriptBuf, WPubkeyHash};
use via_verifier_types::withdrawal::{CompleteWithdrawalBatch, WithdrawalRequest};
use zksync_db_connection::instrument::InstrumentExt;
use zksync_types::{Address as L2Address, H256};

use super::*;

fn wallet() -> ScriptBuf {
    ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([7; 20]))
}

fn source_hash(height: i64) -> bitcoin::BlockHash {
    bitcoin::BlockHash::from_byte_array([u8::try_from(height).unwrap(); 32])
}

async fn seed_verifiers(storage: &mut Connection<'_, Verifier>, count: u8) -> Vec<String> {
    use zksync_types::via_wallet::{SystemWalletsDetails, WalletInfo, WalletRole};

    let verifiers: Vec<_> = (0..count)
        .map(|index| {
            Address::from_script(
                &ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([30 + index; 20])),
                Network::Regtest,
            )
            .unwrap()
        })
        .collect();
    let bridge = Address::from_script(&wallet(), Network::Regtest).unwrap();
    let wallets = SystemWalletsDetails(
        WalletRole::all_roles()
            .iter()
            .map(|&role| {
                let addresses = if role == WalletRole::Verifier {
                    verifiers.clone()
                } else {
                    vec![bridge.clone()]
                };
                (
                    role,
                    WalletInfo {
                        addresses,
                        txid: bitcoin::Txid::all_zeros(),
                    },
                )
            })
            .collect(),
    );
    storage
        .via_wallet_dal()
        .insert_wallets(&wallets, 9)
        .await
        .unwrap();
    verifiers.iter().map(ToString::to_string).collect()
}

async fn import_batch(
    storage: &mut Connection<'_, Verifier>,
    number: u32,
    deposit_height: Option<i64>,
    source_height: i64,
) -> (CompleteWithdrawalBatch, Vec<u8>) {
    let hash = H256::from_low_u64_be(u64::from(number));
    let batch = CompleteWithdrawalBatch {
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
        withdrawals: vec![WithdrawalRequest {
            id: format!("{number:020x}"),
            receiver: Address::from_script(
                &ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([8; 20])),
                Network::Regtest,
            )
            .unwrap(),
            amount: Amount::from_sat(10_000),
            l2_sender: L2Address::repeat_byte(3),
            l2_tx_hash: hash,
            l2_tx_log_index: 0,
        }],
        nonpayable: vec![],
    };
    let proof = hash.as_bytes().to_vec();
    storage
        .via_l1_block_dal()
        .insert_l1_block(source_height, source_hash(source_height).to_string())
        .await
        .unwrap();
    sqlx::query("INSERT INTO via_votable_transactions(l1_batch_number,l1_batch_hash,prev_l1_batch_hash,proof_blob_id,proof_reveal_tx_id,pubdata_blob_id,pubdata_reveal_tx_id,da_identifier,is_finalized,l1_batch_status,source_l1_block_number,source_l1_block_hash,proof_l1_block_number,proof_l1_block_hash) VALUES($1,$2,$2,$3,$2,$4,$5,'fixture',TRUE,TRUE,$6,$7,$6,$7)")
        .bind(i64::from(number))
        .bind(&proof)
        .bind(format!("proof-{number}"))
        .bind(&batch.blob_id)
        .bind(format!("pubdata-{number}"))
        .bind(source_height)
        .bind(source_hash(source_height).to_string())
        .instrument("reverter_test_source")
        .execute(storage)
        .await
        .unwrap();
    if let Some(deposit_height) = deposit_height {
        storage
            .via_transactions_dal()
            .insert_transaction(
                i64::from(number),
                hash,
                "fixture".into(),
                10_000,
                vec![],
                hash,
                deposit_height,
            )
            .await
            .unwrap();
        storage
            .via_transactions_dal()
            .update_transaction(&hash, true, i64::from(number))
            .await
            .unwrap();
    }
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &batch)
        .await
        .unwrap();
    (batch, proof)
}

#[tokio::test]
async fn hard_reorg_recomputes_first_affected_batch_before_removing_sources() {
    // One affected batch catches the inclusive boundary; several catch legacy
    // metadata that names the greatest affected batch instead of the first.
    for last_affected in [2_u32, 4] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        let (predecessor, predecessor_proof) = import_batch(&mut storage, 1, Some(9), 9).await;
        let mut affected = Vec::new();
        for number in 2..=last_affected {
            affected.push(
                import_batch(&mut storage, number, Some(10 + i64::from(number - 2)), 9).await,
            );
        }
        assert_eq!(
            storage
                .via_withdrawal_dal()
                .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
                .await
                .unwrap(),
            std::iter::once(&predecessor)
                .chain(affected.iter().map(|(batch, _)| batch))
                .flat_map(|batch| batch.withdrawals.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            storage
                .via_transactions_dal()
                .get_l1_batch_number_affected_by_reorg(9)
                .await
                .unwrap(),
            Some(2)
        );
        for height in 9..=12 {
            storage
                .via_l1_block_dal()
                .insert_l1_block(height, source_hash(height).to_string())
                .await
                .unwrap();
        }
        storage
            .via_indexer_dal()
            .init_indexer_metadata("via_btc_watch", 12)
            .await
            .unwrap();
        storage
            .via_l1_block_dal()
            .insert_reorg_metadata(10, i64::from(last_affected))
            .await
            .unwrap();

        let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
        reverter.loop_iteration(&mut storage).await.unwrap();

        assert_eq!(
            storage
                .via_withdrawal_dal()
                .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
                .await
                .unwrap(),
            predecessor.withdrawals
        );
        assert!(storage
            .via_votes_dal()
            .proof_reveal_tx_exists(&predecessor_proof)
            .await
            .unwrap());
        assert!(storage
            .via_transactions_dal()
            .transaction_exists_with_txid(&H256::from_low_u64_be(1))
            .await
            .unwrap());
        for (batch, proof) in &affected {
            assert!(!storage
                .via_votes_dal()
                .proof_reveal_tx_exists(proof)
                .await
                .unwrap());
            assert!(!storage
                .via_transactions_dal()
                .transaction_exists_with_txid(&H256::from_low_u64_be(u64::from(batch.batch_number)))
                .await
                .unwrap());
            assert!(storage
                .via_withdrawal_dal()
                .import_complete_batch(wallet().as_bytes(), proof, batch)
                .await
                .is_err());
        }
        assert_eq!(
            storage
                .via_l1_block_dal()
                .list_l1_blocks(9, 10)
                .await
                .unwrap(),
            vec![(9, source_hash(9).to_string())]
        );
        assert_eq!(
            storage
                .via_indexer_dal()
                .get_last_processed_l1_block("via_btc_watch")
                .await
                .unwrap(),
            9
        );
        assert_eq!(
            storage
                .via_l1_block_dal()
                .has_reorg_in_progress()
                .await
                .unwrap(),
            None
        );
        // A stale import failure must not revoke the surviving predecessor.
        assert_eq!(
            storage
                .via_withdrawal_dal()
                .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
                .await
                .unwrap(),
            predecessor.withdrawals
        );
    }
}

#[tokio::test]
async fn reorg_without_affected_deposits_preserves_source_authority() {
    // A soft event preserves authority; a hard event with missing transaction
    // evidence cannot safely infer a source boundary and must remain pending.
    for metadata in [0, 1] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        let (batch, proof) = import_batch(&mut storage, 1, Some(9), 9).await;
        storage
            .via_l1_block_dal()
            .insert_reorg_metadata(10, metadata)
            .await
            .unwrap();
        let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
        let result = reverter.loop_iteration(&mut storage).await;
        if metadata == 0 {
            result.unwrap();
            assert_eq!(
                storage
                    .via_l1_block_dal()
                    .has_reorg_in_progress()
                    .await
                    .unwrap(),
                None
            );
        } else {
            assert!(result.is_err());
            assert_eq!(
                storage
                    .via_l1_block_dal()
                    .has_reorg_in_progress()
                    .await
                    .unwrap(),
                Some((10, metadata))
            );
        }
        assert!(storage
            .via_votes_dal()
            .proof_reveal_tx_exists(&proof)
            .await
            .unwrap());
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
    }
}

#[tokio::test]
async fn proof_or_vote_reorg_invalidates_sources_before_deposits_or_without_deposits() {
    // Proof-only and vote-only divergence must both find batch 2, even when
    // deposits point to batch 3 or do not exist at all.
    for (vote_only, later_deposit) in [(false, true), (true, true), (false, false), (true, false)] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        let (predecessor, predecessor_proof) = import_batch(&mut storage, 1, None, 9).await;
        let first = import_batch(&mut storage, 2, Some(9), if vote_only { 9 } else { 10 }).await;
        let descendant = import_batch(&mut storage, 3, later_deposit.then_some(11), 9).await;
        if vote_only {
            let verifiers = seed_verifiers(&mut storage, 2).await;
            storage
                .via_l1_block_dal()
                .insert_l1_block(10, source_hash(10).to_string())
                .await
                .unwrap();
            let source_id = storage
                .via_votes_dal()
                .get_votable_transaction_id(&first.1)
                .await
                .unwrap()
                .unwrap();
            storage
                .via_votes_dal()
                .insert_vote(source_id, &verifiers[0], true, Some((10, source_hash(10))))
                .await
                .unwrap();
        }
        assert_eq!(
            storage
                .via_transactions_dal()
                .get_l1_batch_number_affected_by_reorg(9)
                .await
                .unwrap(),
            later_deposit.then_some(3)
        );
        assert_eq!(
            storage
                .via_votes_dal()
                .get_l1_batch_number_affected_by_source_reorg(9)
                .await
                .unwrap(),
            Some(2)
        );
        assert_eq!(
            storage
                .via_withdrawal_dal()
                .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
                .await
                .unwrap(),
            [
                predecessor.withdrawals.clone(),
                first.0.withdrawals.clone(),
                descendant.0.withdrawals.clone()
            ]
            .concat()
        );
        storage
            .via_indexer_dal()
            .init_indexer_metadata("via_btc_watch", 11)
            .await
            .unwrap();
        // Zero metadata must not hide source evidence; nonzero metadata may be
        // the later deposit batch, not the first affected source.
        storage
            .via_l1_block_dal()
            .insert_reorg_metadata(10, if later_deposit { 3 } else { 0 })
            .await
            .unwrap();
        let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
        reverter.loop_iteration(&mut storage).await.unwrap();
        // The canonical deposit remains spendable for reassignment when its
        // proof chain is deleted, and keeps its assignment on vote-only reorgs.
        assert!(storage
            .via_transactions_dal()
            .transaction_exists_with_txid(&H256::from_low_u64_be(2))
            .await
            .unwrap());
        assert_eq!(
            storage
                .via_transactions_dal()
                .list_transactions_not_processed(10)
                .await
                .unwrap(),
            if vote_only {
                vec![]
            } else {
                vec![first.1.clone()]
            }
        );
        for (batch, proof) in [&first, &descendant] {
            let retained = vote_only && (batch.batch_number == 2 || !later_deposit);
            assert_eq!(
                storage
                    .via_votes_dal()
                    .proof_reveal_tx_exists(proof)
                    .await
                    .unwrap(),
                retained
            );
            // The affected vote parent cannot authorize imports until votes
            // finalize again. A canonical descendant survives unless the
            // independent deposit boundary removes its proof chain.
            if batch.batch_number == 2 || !retained {
                assert!(storage
                    .via_withdrawal_dal()
                    .import_complete_batch(wallet().as_bytes(), proof, batch)
                    .await
                    .is_err());
            }
        }
        // A stale scan of the removed source block cannot recreate authority.
        assert!(storage
            .via_votes_dal()
            .insert_votable_transaction(
                2,
                H256::from_low_u64_be(2),
                H256::from_low_u64_be(1),
                "fixture".into(),
                H256::from_low_u64_be(2),
                "proof-2".into(),
                "pubdata-2".into(),
                "blob-2".into(),
                Some((10, source_hash(10))),
            )
            .await
            .is_err());
        assert!(storage
            .via_votes_dal()
            .proof_reveal_tx_exists(&predecessor_proof)
            .await
            .unwrap());
        assert_eq!(
            storage
                .via_withdrawal_dal()
                .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
                .await
                .unwrap(),
            predecessor.withdrawals
        );
        assert_eq!(
            storage
                .via_l1_block_dal()
                .has_reorg_in_progress()
                .await
                .unwrap(),
            None
        );
    }
}

#[tokio::test]
async fn source_batch_zero_is_not_confused_with_soft_reorg_metadata() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await.unwrap();
    let zero = import_batch(&mut storage, 0, None, 10).await;
    let successor = import_batch(&mut storage, 1, None, 9).await;
    storage
        .via_indexer_dal()
        .init_indexer_metadata("via_btc_watch", 10)
        .await
        .unwrap();
    storage
        .via_l1_block_dal()
        .insert_reorg_metadata(10, 0)
        .await
        .unwrap();
    let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
    reverter.loop_iteration(&mut storage).await.unwrap();
    for (batch, proof) in [&zero, &successor] {
        assert!(!storage
            .via_votes_dal()
            .proof_reveal_tx_exists(proof)
            .await
            .unwrap());
        assert!(storage
            .via_withdrawal_dal()
            .import_complete_batch(wallet().as_bytes(), proof, batch)
            .await
            .is_err());
    }
    assert!(storage
        .via_withdrawal_dal()
        .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        storage
            .via_l1_block_dal()
            .has_reorg_in_progress()
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn vote_only_reorg_retains_canonical_proofs_and_resumes_finalization() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await.unwrap();
    let verifiers = seed_verifiers(&mut storage, 2).await;
    let (batch, proof) = import_batch(&mut storage, 1, None, 9).await;
    let (successor, successor_proof) = import_batch(&mut storage, 2, None, 9).await;
    storage
        .via_l1_block_dal()
        .insert_l1_block(10, source_hash(10).to_string())
        .await
        .unwrap();
    let id = storage
        .via_votes_dal()
        .get_votable_transaction_id(&proof)
        .await
        .unwrap()
        .unwrap();
    storage
        .via_votes_dal()
        .insert_vote(id, &verifiers[0], true, Some((9, source_hash(9))))
        .await
        .unwrap();
    storage
        .via_votes_dal()
        .insert_vote(id, &verifiers[1], true, Some((10, source_hash(10))))
        .await
        .unwrap();
    storage
        .via_indexer_dal()
        .init_indexer_metadata("via_btc_watch", 10)
        .await
        .unwrap();
    storage
        .via_l1_block_dal()
        .insert_reorg_metadata(10, 1)
        .await
        .unwrap();
    let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
    reverter.loop_iteration(&mut storage).await.unwrap();
    assert!(storage
        .via_votes_dal()
        .proof_reveal_tx_exists(&proof)
        .await
        .unwrap());
    assert!(storage
        .via_votes_dal()
        .proof_reveal_tx_exists(&successor_proof)
        .await
        .unwrap());
    assert!(storage
        .via_withdrawal_dal()
        .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        storage.via_votes_dal().get_vote_count(id).await.unwrap(),
        (0, 1, 1)
    );
    assert!(!storage
        .via_votes_dal()
        .finalize_transaction_if_needed(id, BATCH_FINALIZATION_THRESHOLD, verifiers.len())
        .await
        .unwrap());
    assert_eq!(
        storage
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await
            .unwrap(),
        9
    );
    let replacement = source_hash(20);
    storage
        .via_l1_block_dal()
        .insert_l1_block(10, replacement.to_string())
        .await
        .unwrap();
    storage
        .via_votes_dal()
        .insert_vote(id, &verifiers[1], true, Some((10, replacement)))
        .await
        .unwrap();
    assert_eq!(
        storage.via_votes_dal().get_vote_count(id).await.unwrap(),
        (0, 2, 2)
    );
    assert!(storage
        .via_votes_dal()
        .finalize_transaction_if_needed(id, BATCH_FINALIZATION_THRESHOLD, verifiers.len())
        .await
        .unwrap());
    for (batch, proof) in [(&batch, &proof), (&successor, &successor_proof)] {
        storage
            .via_withdrawal_dal()
            .import_complete_batch(wallet().as_bytes(), proof, batch)
            .await
            .unwrap();
    }
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
            .await
            .unwrap(),
        [batch.withdrawals, successor.withdrawals].concat()
    );
}

#[tokio::test]
async fn redundant_vote_reorg_finalizes_retained_quorum_without_replacement() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await.unwrap();
    let verifiers = seed_verifiers(&mut storage, 3).await;
    let (batch, proof) = import_batch(&mut storage, 1, None, 9).await;
    let id = storage
        .via_votes_dal()
        .get_votable_transaction_id(&proof)
        .await
        .unwrap()
        .unwrap();
    for verifier in &verifiers[..2] {
        storage
            .via_votes_dal()
            .insert_vote(id, verifier, true, Some((9, source_hash(9))))
            .await
            .unwrap();
    }
    storage
        .via_l1_block_dal()
        .insert_l1_block(10, source_hash(10).to_string())
        .await
        .unwrap();
    storage
        .via_votes_dal()
        .insert_vote(id, &verifiers[2], true, Some((10, source_hash(10))))
        .await
        .unwrap();
    storage
        .via_indexer_dal()
        .init_indexer_metadata("via_btc_watch", 10)
        .await
        .unwrap();
    storage
        .via_l1_block_dal()
        .insert_reorg_metadata(10, 1)
        .await
        .unwrap();
    let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
    reverter.loop_iteration(&mut storage).await.unwrap();
    assert_eq!(
        storage.via_votes_dal().get_vote_count(id).await.unwrap(),
        (0, 2, 2)
    );
    // No new vote or explicit finalizer call: the retained two-of-three quorum
    // must restore source finalization within the reorg transaction.
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &batch)
        .await
        .unwrap();
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
            .await
            .unwrap(),
        batch.withdrawals
    );
}

#[tokio::test]
async fn rejected_sibling_vote_reorg_preserves_canonical_replacement() {
    for replacement_finalized in [None, Some(true), Some(false)] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        let verifiers = seed_verifiers(&mut storage, 2).await;
        let (batch, proof) = import_batch(&mut storage, 2, None, 9).await;
        let rejected_proof = H256::repeat_byte(88);
        sqlx::query("INSERT INTO via_votable_transactions(l1_batch_number,l1_batch_hash,prev_l1_batch_hash,proof_blob_id,proof_reveal_tx_id,pubdata_blob_id,pubdata_reveal_tx_id,da_identifier,is_finalized,l1_batch_status,source_l1_block_number,source_l1_block_hash,proof_l1_block_number,proof_l1_block_hash)
            SELECT l1_batch_number,$1,prev_l1_batch_hash,'rejected-proof',$1,'rejected-pubdata','rejected-pubdata-tx',da_identifier,FALSE,TRUE,source_l1_block_number,source_l1_block_hash,proof_l1_block_number,proof_l1_block_hash
            FROM via_votable_transactions WHERE proof_reveal_tx_id=$2")
            .bind(rejected_proof.as_bytes())
            .bind(&proof)
            .instrument("reverter_rejected_sibling_fixture")
            .execute(&mut storage)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE via_votable_transactions SET is_finalized=$1 WHERE proof_reveal_tx_id=$2",
        )
        .bind(replacement_finalized)
        .bind(&proof)
        .instrument("reverter_replacement_finality_fixture")
        .execute(&mut storage)
        .await
        .unwrap();
        let rejected_id = storage
            .via_votes_dal()
            .get_votable_transaction_id(rejected_proof.as_bytes())
            .await
            .unwrap()
            .unwrap();
        storage
            .via_votes_dal()
            .insert_vote(rejected_id, &verifiers[0], false, Some((9, source_hash(9))))
            .await
            .unwrap();
        storage
            .via_l1_block_dal()
            .insert_l1_block(10, source_hash(10).to_string())
            .await
            .unwrap();
        storage
            .via_votes_dal()
            .insert_vote(
                rejected_id,
                &verifiers[1],
                false,
                Some((10, source_hash(10))),
            )
            .await
            .unwrap();
        storage
            .via_l1_block_dal()
            .insert_reorg_metadata(10, 2)
            .await
            .unwrap();
        let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
        reverter.loop_iteration(&mut storage).await.unwrap();
        for (source, expected) in [
            (rejected_proof.as_bytes(), Some(false)),
            (proof.as_slice(), replacement_finalized),
        ] {
            let actual: Option<bool> = sqlx::query_scalar(
                "SELECT is_finalized FROM via_votable_transactions WHERE proof_reveal_tx_id=$1",
            )
            .bind(source)
            .instrument("reverter_sibling_finality")
            .fetch_one(&mut storage)
            .await
            .unwrap();
            assert_eq!(actual, expected);
        }
        assert_eq!(
            storage
                .via_votes_dal()
                .get_vote_count(rejected_id)
                .await
                .unwrap(),
            (1, 0, 1)
        );
        assert_eq!(
            storage
                .via_l1_block_dal()
                .has_reorg_in_progress()
                .await
                .unwrap(),
            None
        );
        let restored = storage
            .via_withdrawal_dal()
            .import_complete_batch(wallet().as_bytes(), &proof, &batch)
            .await;
        assert_eq!(restored.is_ok(), replacement_finalized == Some(true));
    }
}

#[tokio::test]
async fn vote_reorg_preserves_proof_forced_descendant_rejection() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await.unwrap();
    let verifiers = seed_verifiers(&mut storage, 3).await;
    let (_, parent_proof) = import_batch(&mut storage, 1, None, 9).await;
    let (batch, proof) = import_batch(&mut storage, 2, None, 9).await;
    let id = storage
        .via_votes_dal()
        .get_votable_transaction_id(&proof)
        .await
        .unwrap()
        .unwrap();
    for verifier in &verifiers[..2] {
        storage
            .via_votes_dal()
            .insert_vote(id, verifier, true, Some((9, source_hash(9))))
            .await
            .unwrap();
    }
    storage
        .via_l1_block_dal()
        .insert_l1_block(10, source_hash(10).to_string())
        .await
        .unwrap();
    storage
        .via_votes_dal()
        .insert_vote(id, &verifiers[2], true, Some((10, source_hash(10))))
        .await
        .unwrap();
    storage
        .via_votes_dal()
        .verify_votable_transaction(1, H256::from_slice(&parent_proof), false)
        .await
        .unwrap();
    storage
        .via_l1_block_dal()
        .insert_reorg_metadata(10, 2)
        .await
        .unwrap();
    let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
    reverter.loop_iteration(&mut storage).await.unwrap();
    assert_eq!(
        storage.via_votes_dal().get_vote_count(id).await.unwrap(),
        (0, 2, 2)
    );
    assert_eq!(
        storage
            .via_votes_dal()
            .get_first_rejected_l1_batch()
            .await
            .unwrap(),
        Some((2, proof.clone()))
    );
    assert!(storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &batch)
        .await
        .is_err());
    storage
        .via_votes_dal()
        .delete_invalid_votable_transactions_if_exists()
        .await
        .unwrap();
    assert!(!storage
        .via_votes_dal()
        .proof_reveal_tx_exists(&proof)
        .await
        .unwrap());
}
