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
    let (l1_batch_number, tx_id, verifier_address, vote) = mock_via_vote();

    // First insert a votable transaction
    storage
        .via_votes_dal()
        .insert_votable_transaction(
            l1_batch_number,
            tx_id,
            "test_da_id".to_string(),
            "test_blob_id".to_string(),
            "test_pubdata_tx_id".to_string(),
            "test_pubdata_blob_id".to_string(),
        )
        .await
        .unwrap();

    // Test inserting a vote
    storage
        .via_votes_dal()
        .insert_vote(l1_batch_number, tx_id, &verifier_address, vote)
        .await
        .unwrap();

    // Test getting vote count
    let (ok_votes, total_votes) = storage
        .via_votes_dal()
        .get_vote_count(l1_batch_number, tx_id)
        .await
        .unwrap();

    assert_eq!(total_votes, 1);
    assert_eq!(ok_votes, if vote { 1 } else { 0 });

    // Test finalizing transaction
    let is_finalized = storage
        .via_votes_dal()
        .finalize_transaction_if_needed(l1_batch_number, tx_id, 0.5, 1)
        .await
        .unwrap();

    assert_eq!(is_finalized, vote); // Should be finalized if the vote was true
}
