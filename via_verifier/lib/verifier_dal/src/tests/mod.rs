use rand::random;
use zksync_db_connection::{connection::Connection, connection_pool::ConnectionPool};
use zksync_types::H256;

use crate::{Verifier, VerifierDal};

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
