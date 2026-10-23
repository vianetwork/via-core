#[cfg(test)]
mod tests {
    use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
    use via_test_utils::bitcoin_core::BitcoinCore;
    use via_test_utils::utils::{create_chained_inscriptions, test_create_indexer};
    use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};

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
        core: &BitcoinCore,
        processor: &mut VerifierMessageProcessor,
        indexer: &mut BitcoinInscriptionIndexer,
        messages: Vec<FullInscriptionMessage>,
    ) -> anyhow::Result<()> {
        let messages = attach_scanned_sources(pool, core, messages).await?;
        process_owned(pool, processor, indexer, messages).await?;
        Ok(())
    }

    async fn process_owned(
        pool: &ConnectionPool<Verifier>,
        processor: &mut VerifierMessageProcessor,
        indexer: &mut BitcoinInscriptionIndexer,
        messages: Vec<FullInscriptionMessage>,
    ) -> Result<(), crate::message_processors::MessageProcessorError> {
        let mut blocks = std::collections::BTreeMap::<u32, Vec<FullInscriptionMessage>>::new();
        for message in messages {
            blocks.entry(message.common().block_height).or_default().push(message);
        }
        for (_, messages) in blocks {
            let mut storage = pool.connection().await.map_err(anyhow::Error::from)?;
            let mut tx = storage.start_transaction().await?;
            tx.via_indexer_dal().lock_source_writer().await.map_err(anyhow::Error::from)?;
            processor.process_messages(&mut tx, messages, indexer).await?;
            tx.commit().await?;
        }
        Ok(())
    }

    async fn attach_scanned_sources(
        pool: &ConnectionPool<Verifier>,
        core: &BitcoinCore,
        messages: Vec<FullInscriptionMessage>,
    ) -> anyhow::Result<Vec<FullInscriptionMessage>> {
        let client = core.client()?;
        let mut storage = pool.connection().await?;
        let mut tx = storage.start_transaction().await?;
        tx.via_indexer_dal().lock_source_writer().await?;
        via_test_utils::reader_db::bootstrap_verifiers(
            tx.conn(),
            &via_test_utils::utils::test_wallets()
                .verifiers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            0,
        )
        .await?;
        let mut through = 0;
        for message in &messages {
            let inclusion = message.common().inclusion()?;
            let header = client
                .canonical_header_at(inclusion.block.height, inclusion.block.block_hash)
                .await?;
            tx.via_l1_block_dal().accept_header(header.into()).await?;
            through = through.max(header.height);
        }
        tx.via_indexer_dal().init_indexer_metadata("via_btc_watch", through).await?;
        tx.via_indexer_dal().update_last_processed_l1_block("via_btc_watch", through).await?;
        tx.commit().await?;
        Ok(messages)
    }

    async fn complete_fixture_verdict(
        pool: &ConnectionPool<Verifier>,
        id: i64,
        approved: bool,
    ) -> anyhow::Result<bool> {
        let mut storage = pool.connection().await?;
        let mut tx = storage.start_transaction().await?;
        tx.via_indexer_dal().lock_source_writer().await?;
        let inputs = tx
            .via_votes_dal()
            .verification_snapshot(id, 0)
            .await?
            .ok_or_else(|| anyhow::anyhow!("fixture proof missing"))?;
        if approved {
            tx.via_votes_dal().mark_proof_verified(id, &inputs).await?;
        } else {
            sqlx::query("UPDATE via_proof_verdicts SET l1_batch_status=FALSE WHERE id=$1")
                .bind(id)
                .execute(tx.conn())
                .await?;
        }
        // This fixture exercises L2 chain admission; network quorum behavior has independent reader-DAL coverage.
        sqlx::query("INSERT INTO via_proof_finalization(proof_inclusion_id,verifier_set_id,decision) SELECT i.id,i.verifier_set_id,$2 FROM via_proof_inclusions i JOIN via_proof_verdicts v ON v.reader_proof_inclusion_id=i.id WHERE v.id=$1 ON CONFLICT(proof_inclusion_id) DO UPDATE SET decision=EXCLUDED.decision")
            .bind(id).bind(approved).execute(tx.conn()).await?;
        tx.commit().await?;
        Ok(approved)
    }

    async fn finalize_all(
        pool: &ConnectionPool<Verifier>,
        vote: impl Fn(i64) -> bool,
    ) -> anyhow::Result<()> {
        loop {
            if let Some(votable_transaction_id) = pool
                .connection()
                .await?
                .via_votes_dal()
                .get_first_non_finalized_l1_batch_in_canonical_inscription_chain()
                .await?
            {
                let Some((_l1_batch_number, _proof_tx_id)) = pool
                    .connection()
                    .await?
                    .via_votes_dal()
                    .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
                    .await?
                else {
                    anyhow::bail!("Invalid batch");
                };

                let is_finalized = complete_fixture_verdict(
                    pool,
                    votable_transaction_id,
                    vote(votable_transaction_id),
                )
                .await?;
                assert_eq!(is_finalized, vote(votable_transaction_id));

                continue;
            }
            break;
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_insert_first_batch() -> anyhow::Result<()> {
        let (pool, core, mut indexer, mut processor) = setup().await?;
        let start = 1;
        let end = 1;

        let (msgs, _) = create_chained_inscriptions(&core, start, end, None).await?;

        apply_messages(&pool, &core, &mut processor, &mut indexer, msgs).await?;

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
        let (alternative, _) = create_chained_inscriptions(&core, start, end, None).await?;
        msgs.extend(alternative);

        apply_messages(&pool, &core, &mut processor, &mut indexer, msgs).await?;

        assert_pending(&pool, 1).await?;
        let mut storage = pool.connection().await?;
        let (retained, current): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM via_proof_verdicts),
            (SELECT count(*) FROM via_votable_transactions)",
        )
        .fetch_one(storage.conn())
        .await?;
        assert_eq!((retained, current), (2, 1));

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_multiple_batches() -> anyhow::Result<()> {
        let (pool, core, mut indexer, mut processor) = setup().await?;

        let start = 1;
        let end = 3;
        let (msgs, _) = create_chained_inscriptions(&core, start, end, None).await?;

        apply_messages(&pool, &core, &mut processor, &mut indexer, msgs).await?;

        for i in 0..end {
            assert_pending(&pool, (i + 1) as u32).await?;
        }
        verify_canonical_chain(pool.clone(), end as u32).await?;
        for id in 1..=3 {
            complete_fixture_verdict(&pool, id, true).await?;
        }
        let mut storage = pool.connection().await?;
        let original: (i64,Vec<u8>,Option<bool>,Option<serde_json::Value>) = sqlx::query_as(
            "SELECT id,proof_reveal_tx_id,l1_batch_status,completed_input_identity FROM via_proof_verdicts WHERE l1_batch_number=3",
        ).fetch_one(storage.conn()).await?;
        let parent: Vec<u8> = sqlx::query_scalar(
            "SELECT l1_batch_hash FROM via_votable_transactions WHERE l1_batch_number=2",
        )
        .fetch_one(storage.conn())
        .await?;
        let (msgs, _) =
            create_chained_inscriptions(&core, 3, 4, Some(zksync_types::H256::from_slice(&parent)))
                .await?;
        let msgs = attach_scanned_sources(&pool, &core, msgs).await?;
        let retained_height =
            i64::from(msgs.iter().map(|msg| msg.common().block_height).min().unwrap()) - 1;
        process_owned(&pool, &mut processor, &mut indexer, msgs).await?;
        verify_canonical_chain(pool.clone(), 4).await?;
        let displaced: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM via_votable_transactions WHERE id=$1)")
                .bind(original.0)
                .fetch_one(storage.conn())
                .await?;
        assert!(!displaced);
        storage.via_l1_block_dal().insert_reorg_metadata(retained_height + 1, 4).await?;
        let reverter = via_verifier_block_reverter::ViaVerifierBlockReverter::new(
            pool.clone(),
            zksync_config::configs::via_reorg_detector::ViaReorgDetectorConfig::for_tests(),
        );
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(reverter.run(receiver));
        let reverted = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while storage.via_l1_block_dal().has_reorg_in_progress().await?.is_some() {
                tokio::task::yield_now().await;
            }
            anyhow::Ok(())
        })
        .await;
        let _ = stop.send(true);
        task.await??;
        reverted??;
        for _ in 0..2 {
            let mut tx = storage.start_transaction().await?;
            tx.via_indexer_dal().lock_source_writer().await?;
            tx.via_l1_block_dal().delete_l1_blocks(retained_height).await?;
            tx.via_votes_dal().revoke_reader_inclusions_after(retained_height).await?;
            tx.commit().await?;
            let restored: (i64,Vec<u8>,Option<bool>,Option<serde_json::Value>) = sqlx::query_as(
                "SELECT v.id,v.proof_reveal_tx_id,v.l1_batch_status,f.completed_input_identity FROM via_votable_transactions v JOIN via_proof_verdicts f ON f.id=v.id WHERE v.l1_batch_number=3",
            ).fetch_one(storage.conn()).await?;
            assert_eq!(restored, original);
        }
        verify_canonical_chain(pool.clone(), 3).await?;

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

        let msgs = attach_scanned_sources(&pool, &core, msgs).await?;
        let error = process_owned(&pool, &mut processor, &mut indexer, msgs).await.unwrap_err();
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

        apply_messages(&pool, &core, &mut processor, &mut indexer, msgs).await?;

        for i in start..end {
            let found_batch = pool.connection().await?.via_votes_dal().batch_exists(i as u32).await?;
            assert!(found_batch);
        }

        // Expected last batch is 5
        let expected_batch_len = 5;
        verify_canonical_chain(pool.clone(), expected_batch_len).await?;

        finalize_all(&pool, |_| true).await?;

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

        let msgs3 = attach_scanned_sources(&pool, &core, msgs3).await?;
        let error = process_owned(&pool, &mut processor, &mut indexer, msgs3).await.unwrap_err();
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

        apply_messages(&pool, &core, &mut processor, &mut indexer, msgs).await?;

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

        apply_messages(&pool, &core, &mut processor, &mut indexer, msgs).await?;

        verify_canonical_chain(pool.clone(), end as u32).await?;

        finalize_all(&pool, |id| id != 3).await?;

        // A child of rejected batch 3 cannot extend the accepted prefix; report the continuity gap.
        let start = 4;
        let end = 4;
        let (msgs, _) = create_chained_inscriptions(&core, start, end, Some(batch_3_hash)).await?;
        let messages = attach_scanned_sources(&pool, &core, msgs).await?;
        let error = process_owned(&pool, &mut processor, &mut indexer, messages).await.unwrap_err();
        assert!(matches!(
            error,
            crate::message_processors::MessageProcessorError::ProofContinuityGap(_)
        ));

        verify_canonical_chain(pool.clone(), 2).await?;

        // Insert a new batches {3, 4} which is a child of the batch 2, those batches should be inserted.
        let start = 3;
        let end = 4;
        let (msgs, _) = create_chained_inscriptions(&core, start, end, Some(batch_2_hash)).await?;
        apply_messages(&pool, &core, &mut processor, &mut indexer, msgs).await?;

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
