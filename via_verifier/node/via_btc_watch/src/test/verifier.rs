#[cfg(test)]
mod tests {
    use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
    use zksync_types::H256;

    use crate::{message_processors::VerifierMessageProcessor, MessageProcessor};

    use via_test_utils::utils::{
        create_chained_inscriptions, test_create_indexer, test_verifier_add_1, test_verifier_add_2,
    };

    #[tokio::test]
    async fn test_insert_first_batch() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut indexer = test_create_indexer().await?;
        let start = 1;
        let end = 1;

        let mut processor = VerifierMessageProcessor::new(1.0);
        let (msgs, _) = create_chained_inscriptions(start, end, None).await?;

        processor
            .process_messages(&mut pool.connection().await?, msgs, &mut indexer)
            .await?;

        let expected_batch = 1;
        let inserted = pool
            .connection()
            .await?
            .via_votes_dal()
            .batch_exists(expected_batch)
            .await?;
        assert!(inserted);

        let found_batch = pool
            .connection()
            .await?
            .via_votes_dal()
            .get_first_non_finalized_l1_batch_in_canonical_inscription_chain()
            .await?;
        assert_eq!(found_batch, Some(expected_batch as i64));

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_two_times_first_batch() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut indexer = test_create_indexer().await?;
        let start = 1;
        let end = 1;

        let mut processor = VerifierMessageProcessor::new(1.0);
        let (mut msgs, _) = create_chained_inscriptions(start, end, None).await?;

        // duplicate the first batch message
        msgs.extend(msgs.clone());

        processor
            .process_messages(&mut pool.connection().await?, msgs, &mut indexer)
            .await?;

        let expected_batch = 1;
        let inserted = pool
            .connection()
            .await?
            .via_votes_dal()
            .batch_exists(expected_batch)
            .await?;
        assert!(inserted);

        let found_batch = pool
            .connection()
            .await?
            .via_votes_dal()
            .get_first_non_finalized_l1_batch_in_canonical_inscription_chain()
            .await?;
        assert_eq!(found_batch, Some(expected_batch as i64));

        assert_eq!(found_batch, Some(1 as i64));
        Ok(())
    }

    #[tokio::test]
    async fn test_insert_multiple_batches() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut indexer = test_create_indexer().await?;

        let mut processor = VerifierMessageProcessor::new(1.0);
        let start = 1;
        let end = 3;
        let (msgs, _) = create_chained_inscriptions(start, end, None).await?;

        processor
            .process_messages(&mut pool.connection().await?, msgs, &mut indexer)
            .await?;

        for i in 0..end {
            let expected_id = i + 1;
            let inserted = pool
                .connection()
                .await?
                .via_votes_dal()
                .batch_exists(expected_id as u32)
                .await?;
            assert!(inserted);

            let found_batch = pool
                .connection()
                .await?
                .via_votes_dal()
                .get_first_non_finalized_l1_batch_in_canonical_inscription_chain()
                .await?;
            assert_eq!(found_batch, Some(1 as i64));
        }
        verify_canonical_chain(pool, end as u32).await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_should_fail_to_insert_batch_with_invalid_prev_batch_hash() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut indexer = test_create_indexer().await?;
        let mut processor = VerifierMessageProcessor::new(1.0);
        let start = 1;
        let end = 2;
        let (mut msgs, prev_l1_batch_hash) = create_chained_inscriptions(start, end, None).await?;

        //--------------------------------------------------------------------------------------------------
        // Insert invalid list of batches {3, 4, 5}, the batch 3 prev hash is invalid
        //--------------------------------------------------------------------------------------------------
        // Insert another batches {3, 4, 5} with invalid prev batch hash.
        let start = 3;
        let end = 5;
        let (msgs2, _) = create_chained_inscriptions(start, end, None).await?;
        msgs.extend(msgs2);

        processor
            .process_messages(&mut pool.connection().await?, msgs, &mut indexer)
            .await?;

        // Expect that only the batch {1, 2} are inserted.
        let expected_batch_len = 2;
        for i in 0..expected_batch_len {
            let expected_id = i + 1;
            let inserted = pool
                .connection()
                .await?
                .via_votes_dal()
                .batch_exists(expected_id as u32)
                .await?;
            assert!(inserted);

            let found_batch = pool
                .connection()
                .await?
                .via_votes_dal()
                .get_first_non_finalized_l1_batch_in_canonical_inscription_chain()
                .await?;
            assert_eq!(found_batch, Some(1 as i64));
        }

        // The batch 3 should not be inserted
        let found_batch = pool
            .connection()
            .await?
            .via_votes_dal()
            .batch_exists(3)
            .await?;
        assert!(!found_batch);

        verify_canonical_chain(pool.clone(), expected_batch_len).await?;

        //--------------------------------------------------------------------------------------------------
        // Insert valid list of batches {3, 4, 5}
        //--------------------------------------------------------------------------------------------------
        // Create a valid batch 3 using the batch 2 hash.
        let (msgs, _) = create_chained_inscriptions(start, end, Some(prev_l1_batch_hash)).await?;

        processor
            .process_messages(&mut pool.connection().await?, msgs, &mut indexer)
            .await?;

        for i in start..end {
            let found_batch = pool
                .connection()
                .await?
                .via_votes_dal()
                .batch_exists(i as u32)
                .await?;
            assert!(found_batch);
        }

        // Expected last batch is 5
        let expected_batch_len = 5;
        verify_canonical_chain(pool.clone(), expected_batch_len).await?;

        // Vote to mark the batches as finalized
        loop {
            if let Some(votable_transaction_id) = pool
                .connection()
                .await?
                .via_votes_dal()
                .get_first_non_finalized_l1_batch_in_canonical_inscription_chain()
                .await?
            {
                let Some((l1_batch_number, proof_tx_id)) = pool
                    .connection()
                    .await?
                    .via_votes_dal()
                    .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
                    .await?
                else {
                    anyhow::bail!("Invalid batch");
                };

                pool.connection()
                    .await?
                    .via_votes_dal()
                    .insert_vote(
                        votable_transaction_id,
                        &test_verifier_add_1().to_string(),
                        true,
                    )
                    .await?;

                pool.connection()
                    .await?
                    .via_votes_dal()
                    .insert_vote(
                        votable_transaction_id,
                        &test_verifier_add_2().to_string(),
                        true,
                    )
                    .await?;

                let proof_reveal_tx_id = H256::from_slice(&proof_tx_id);
                pool.connection()
                    .await?
                    .via_votes_dal()
                    .verify_votable_transaction(l1_batch_number, proof_reveal_tx_id, true)
                    .await?;

                let is_finalized = pool
                    .connection()
                    .await?
                    .via_votes_dal()
                    .finalize_transaction_if_needed(votable_transaction_id, 0.5, 2)
                    .await?;

                assert!(is_finalized);

                continue;
            }
            break;
        }

        assert_eq!(
            pool.connection()
                .await?
                .via_votes_dal()
                .get_first_non_finalized_l1_batch_in_canonical_inscription_chain()
                .await?,
            None
        );

        // Check if the last finalized batch
        assert_eq!(
            pool.connection()
                .await?
                .via_votes_dal()
                .get_last_finalized_l1_batch()
                .await?,
            Some(expected_batch_len as u32)
        );

        //--------------------------------------------------------------------------------------------------
        // Try insert an invalid list of batches, should not insert
        //--------------------------------------------------------------------------------------------------
        let start = 6;
        let end = 7;
        let (msgs3, _) = create_chained_inscriptions(start, end, None).await?;

        processor
            .process_messages(&mut pool.connection().await?, msgs3, &mut indexer)
            .await?;

        // No batch should be inserted.
        for i in start..end {
            let found_batch = pool
                .connection()
                .await?
                .via_votes_dal()
                .batch_exists(i as u32)
                .await?;
            assert!(!found_batch);
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_should_not_insert_batch_zero() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut indexer = test_create_indexer().await?;
        let start = 0;
        let end = 0;

        let mut processor = VerifierMessageProcessor::new(1.0);
        let (msgs, _) = create_chained_inscriptions(start, end, None).await?;

        processor
            .process_messages(&mut pool.connection().await?, msgs, &mut indexer)
            .await?;

        let expected_batch = 0;
        let inserted = pool
            .connection()
            .await?
            .via_votes_dal()
            .batch_exists(expected_batch)
            .await?;
        assert!(!inserted);

        Ok(())
    }

    // Scenario: the batch 3 was rejected by the verifier network, then the sequencer created a new valid batch 3.
    #[tokio::test]
    async fn test_should_insert_new_valid_batch_with_same_block_number_after_it_was_rejected(
    ) -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;

        let mut indexer = test_create_indexer().await?;
        let mut processor = VerifierMessageProcessor::new(1.0);
        let start = 1;
        let end = 2;
        let (mut msgs, batch_2_hash) = create_chained_inscriptions(start, end, None).await?;

        let start = 3;
        let end = 3;
        let (msgs2, batch_3_hash) =
            create_chained_inscriptions(start, end, Some(batch_2_hash)).await?;
        msgs.extend(msgs2);

        processor
            .process_messages(&mut pool.connection().await?, msgs, &mut indexer)
            .await?;

        verify_canonical_chain(pool.clone(), end as u32).await?;

        // Vote to mark the batches as finalized
        loop {
            if let Some(votable_transaction_id) = pool
                .connection()
                .await?
                .via_votes_dal()
                .get_first_non_finalized_l1_batch_in_canonical_inscription_chain()
                .await?
            {
                let Some((l1_batch_number, proof_tx_id)) = pool
                    .connection()
                    .await?
                    .via_votes_dal()
                    .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
                    .await?
                else {
                    anyhow::bail!("Invalid batch");
                };
                // The verifier network should reject the batch 3.
                let vote = votable_transaction_id != 3;

                pool.connection()
                    .await?
                    .via_votes_dal()
                    .insert_vote(
                        votable_transaction_id,
                        &test_verifier_add_1().to_string(),
                        vote,
                    )
                    .await?;

                pool.connection()
                    .await?
                    .via_votes_dal()
                    .insert_vote(
                        votable_transaction_id,
                        &test_verifier_add_2().to_string(),
                        vote,
                    )
                    .await?;

                let proof_reveal_tx_id = H256::from_slice(&proof_tx_id);

                pool.connection()
                    .await?
                    .via_votes_dal()
                    .verify_votable_transaction(l1_batch_number, proof_reveal_tx_id, vote)
                    .await?;

                let is_finalized = pool
                    .connection()
                    .await?
                    .via_votes_dal()
                    .finalize_transaction_if_needed(votable_transaction_id, 0.5, 2)
                    .await?;

                assert_eq!(is_finalized, vote);

                continue;
            }
            break;
        }

        // Insert a batch 4 which is a child of the rejected batch 3, this batch should be ignored.
        let start = 4;
        let end = 4;
        let (msgs, _) = create_chained_inscriptions(start, end, Some(batch_3_hash)).await?;
        processor
            .process_messages(&mut pool.connection().await?, msgs, &mut indexer)
            .await?;

        verify_canonical_chain(pool.clone(), 2).await?;

        // Insert a new batches {3, 4} which is a child of the batch 2, those batches should be inserted.
        let start = 3;
        let end = 4;
        let (msgs, _) = create_chained_inscriptions(start, end, Some(batch_2_hash)).await?;
        processor
            .process_messages(&mut pool.connection().await?, msgs, &mut indexer)
            .await?;

        verify_canonical_chain(pool.clone(), end as u32).await?;

        Ok(())
    }

    async fn verify_canonical_chain(
        pool: ConnectionPool<Verifier>,
        expected_batch_len: u32,
    ) -> anyhow::Result<()> {
        // Check the canonical chain
        let chain_status = pool
            .connection()
            .await?
            .via_votes_dal()
            .verify_canonical_chain()
            .await?;

        assert!(chain_status.is_valid);
        assert!(chain_status.has_genesis);
        assert_eq!(chain_status.max_batch_number, Some(expected_batch_len));
        assert_eq!(
            chain_status.total_canonical_batches,
            expected_batch_len as i64
        );
        assert_eq!(chain_status.min_batch_number, Some(1 as u32));
        assert!(chain_status.missing_batches.is_empty());

        Ok(())
    }
}
