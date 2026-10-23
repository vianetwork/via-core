#[cfg(test)]
mod tests {
    use bitcoin::{hashes::Hash, BlockHash};
    use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
    use via_test_utils::bitcoin_core::BitcoinCore;
    use via_test_utils::utils::{
        create_chained_inscriptions, test_create_indexer, test_verifier_add_1, test_verifier_add_2,
    };
    use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
    use zksync_types::H256;

    use crate::{message_processors::VerifierMessageProcessor, MessageProcessor};

    async fn setup() -> anyhow::Result<(
        ConnectionPool<Verifier>,
        BitcoinCore,
        via_btc_client::indexer::BitcoinInscriptionIndexer,
        VerifierMessageProcessor,
    )> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let core = BitcoinCore::new()?;
        let indexer = test_create_indexer(&core)?;
        Ok((pool, core, indexer, VerifierMessageProcessor::default()))
    }

    async fn assert_pending(pool: &ConnectionPool<Verifier>, batch: u32) -> anyhow::Result<()> {
        let mut storage = pool.connection().await?;
        assert!(storage.via_votes_dal().batch_exists(batch).await?);
        assert_eq!(
            storage
                .via_votes_dal()
                .get_first_non_finalized_l1_batch_in_canonical_inscription_chain()
                .await?,
            Some(1)
        );
        Ok(())
    }

    async fn apply_messages(
        pool: &ConnectionPool<Verifier>,
        processor: &mut VerifierMessageProcessor,
        indexer: &mut BitcoinInscriptionIndexer,
        messages: Vec<FullInscriptionMessage>,
    ) -> anyhow::Result<()> {
        let messages = attach_scanned_sources(pool, messages).await?;
        processor.process_messages(&mut pool.connection().await?, messages, indexer).await?;
        Ok(())
    }

    async fn attach_scanned_sources(
        pool: &ConnectionPool<Verifier>, mut messages: Vec<FullInscriptionMessage>,
    ) -> anyhow::Result<Vec<FullInscriptionMessage>> {
        let mut storage = pool.connection().await?;
        for message in &mut messages {
            let common = match message {
                FullInscriptionMessage::ProofDAReference(message) => &mut message.common,
                FullInscriptionMessage::ValidatorAttestation(message) => &mut message.common,
                _ => continue,
            };
            // Standalone parser fixtures have no scan context; supply a fixed
            // accepted local block explicitly, just as the scanner would.
            common.block_height = 100;
            let hash = BlockHash::from_byte_array([100; 32]);
            common.block_hash = Some(hash);
            storage.via_l1_block_dal().insert_l1_block(100, hash.to_string()).await?;
        }
        Ok(messages)
    }

    #[tokio::test]
    async fn test_insert_first_batch() -> anyhow::Result<()> {
        let (pool, core, mut indexer, mut processor) = setup().await?;
        let start = 1;
        let end = 1;

        let (msgs, _) = create_chained_inscriptions(&core, start, end, None).await?;

        apply_messages(&pool, &mut processor, &mut indexer, msgs).await?;

        assert_pending(&pool, 1).await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_two_times_first_batch() -> anyhow::Result<()> {
        let (pool, core, mut indexer, mut processor) = setup().await?;
        let start = 1;
        let end = 1;

        let (mut msgs, _) = create_chained_inscriptions(&core, start, end, None).await?;

        // duplicate the first batch message
        msgs.extend(msgs.clone());

        apply_messages(&pool, &mut processor, &mut indexer, msgs).await?;

        assert_pending(&pool, 1).await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_multiple_batches() -> anyhow::Result<()> {
        let (pool, core, mut indexer, mut processor) = setup().await?;

        let start = 1;
        let end = 3;
        let (msgs, _) = create_chained_inscriptions(&core, start, end, None).await?;

        apply_messages(&pool, &mut processor, &mut indexer, msgs).await?;

        for i in 0..end {
            assert_pending(&pool, (i + 1) as u32).await?;
        }
        verify_canonical_chain(pool, end as u32).await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_should_fail_to_insert_batch_with_invalid_prev_batch_hash() -> anyhow::Result<()> {
        let (pool, core, mut indexer, mut processor) = setup().await?;
        let start = 1;
        let end = 2;
        let (mut msgs, prev_l1_batch_hash) =
            create_chained_inscriptions(&core, start, end, None).await?;

        //--------------------------------------------------------------------------------------------------
        // Insert invalid list of batches {3, 4, 5}, the batch 3 prev hash is invalid
        //--------------------------------------------------------------------------------------------------
        // Insert another batches {3, 4, 5} with invalid prev batch hash.
        let start = 3;
        let end = 5;
        let (msgs2, _) = create_chained_inscriptions(&core, start, end, None).await?;
        msgs.extend(msgs2);

        let msgs = attach_scanned_sources(&pool, msgs).await?;
        let error = processor.process_messages(&mut pool.connection().await?, msgs, &mut indexer).await.unwrap_err();
        assert!(matches!(error, crate::message_processors::MessageProcessorError::ProofContinuityGap(_)));

        // Expect that only the batch {1, 2} are inserted.
        let expected_batch_len = 2;
        for i in 0..expected_batch_len {
            assert_pending(&pool, (i + 1) as u32).await?;
        }

        // The batch 3 should not be inserted
        let found_batch = pool.connection().await?.via_votes_dal().batch_exists(3).await?;
        assert!(!found_batch);

        verify_canonical_chain(pool.clone(), expected_batch_len).await?;

        //--------------------------------------------------------------------------------------------------
        // Insert valid list of batches {3, 4, 5}
        //--------------------------------------------------------------------------------------------------
        // Create a valid batch 3 using the batch 2 hash.
        let (msgs, _) =
            create_chained_inscriptions(&core, start, end, Some(prev_l1_batch_hash)).await?;

        apply_messages(&pool, &mut processor, &mut indexer, msgs).await?;

        for i in start..end {
            let found_batch = pool.connection().await?.via_votes_dal().batch_exists(i as u32).await?;
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
                    .insert_vote(votable_transaction_id, &test_verifier_add_1().to_string(), true, None)
                    .await?;

                pool.connection()
                    .await?
                    .via_votes_dal()
                    .insert_vote(votable_transaction_id, &test_verifier_add_2().to_string(), true, None)
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
            pool.connection().await?.via_votes_dal().get_last_finalized_l1_batch().await?,
            Some(expected_batch_len)
        );

        //--------------------------------------------------------------------------------------------------
        // Try insert an invalid list of batches, should not insert
        //--------------------------------------------------------------------------------------------------
        let start = 6;
        let end = 7;
        let (msgs3, _) = create_chained_inscriptions(&core, start, end, None).await?;

        let msgs3 = attach_scanned_sources(&pool, msgs3).await?;
        let error = processor.process_messages(&mut pool.connection().await?, msgs3, &mut indexer).await.unwrap_err();
        assert!(matches!(error, crate::message_processors::MessageProcessorError::ProofContinuityGap(_)));

        // No batch should be inserted.
        for i in start..end {
            let found_batch = pool.connection().await?.via_votes_dal().batch_exists(i as u32).await?;
            assert!(!found_batch);
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_should_not_insert_batch_zero() -> anyhow::Result<()> {
        let (pool, core, mut indexer, mut processor) = setup().await?;
        let start = 0;
        let end = 0;

        let (msgs, _) = create_chained_inscriptions(&core, start, end, None).await?;

        apply_messages(&pool, &mut processor, &mut indexer, msgs).await?;

        let expected_batch = 0;
        let inserted = pool.connection().await?.via_votes_dal().batch_exists(expected_batch).await?;
        assert!(!inserted);

        Ok(())
    }

    // Scenario: the batch 3 was rejected by the verifier network, then the sequencer created a new valid batch 3.
    #[tokio::test]
    async fn test_should_insert_new_valid_batch_with_same_block_number_after_it_was_rejected(
    ) -> anyhow::Result<()> {
        let (pool, core, mut indexer, mut processor) = setup().await?;

        let start = 1;
        let end = 2;
        let (mut msgs, batch_2_hash) = create_chained_inscriptions(&core, start, end, None).await?;

        let start = 3;
        let end = 3;
        let (msgs2, batch_3_hash) =
            create_chained_inscriptions(&core, start, end, Some(batch_2_hash)).await?;
        msgs.extend(msgs2);

        apply_messages(&pool, &mut processor, &mut indexer, msgs).await?;

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
                    .insert_vote(votable_transaction_id, &test_verifier_add_1().to_string(), vote, None)
                    .await?;

                pool.connection()
                    .await?
                    .via_votes_dal()
                    .insert_vote(votable_transaction_id, &test_verifier_add_2().to_string(), vote, None)
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

        // A child of rejected batch 3 cannot extend the accepted prefix; report the continuity gap.
        let start = 4;
        let end = 4;
        let (msgs, _) = create_chained_inscriptions(&core, start, end, Some(batch_3_hash)).await?;
        let messages = attach_scanned_sources(&pool, msgs).await?;
        let error = processor
            .process_messages(&mut pool.connection().await?, messages, &mut indexer)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            crate::message_processors::MessageProcessorError::ProofContinuityGap(_)
        ));

        verify_canonical_chain(pool.clone(), 2).await?;

        // Insert a new batches {3, 4} which is a child of the batch 2, those batches should be inserted.
        let start = 3;
        let end = 4;
        let (msgs, _) = create_chained_inscriptions(&core, start, end, Some(batch_2_hash)).await?;
        apply_messages(&pool, &mut processor, &mut indexer, msgs).await?;

        verify_canonical_chain(pool.clone(), end as u32).await?;

        Ok(())
    }

    async fn verify_canonical_chain(pool: ConnectionPool<Verifier>, expected_batch_len: u32) -> anyhow::Result<()> {
        // Check the canonical chain
        let chain_status = pool.connection().await?.via_votes_dal().verify_canonical_chain().await?;

        assert!(chain_status.is_valid);
        assert!(chain_status.has_genesis);
        assert_eq!(chain_status.max_batch_number, Some(expected_batch_len));
        assert_eq!(chain_status.total_canonical_batches, expected_batch_len as i64);
        assert_eq!(chain_status.min_batch_number, Some(1_u32));
        assert!(chain_status.missing_batches.is_empty());

        Ok(())
    }
}
