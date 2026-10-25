#[cfg(test)]
pub(crate) mod tests {
    use via_btc_client::inscriber::test_utils::{get_mock_inscriber_and_conditions, MockBitcoinOps};
    use super::super::ViaBtcInscriptionAggregator;
    use zksync_config::ViaBtcSenderConfig;
    use zksync_dal::{ConnectionPool, Core, CoreDal};
    use zksync_node_test_utils::l1_batch_metadata_to_commitment_artifacts;
    use zksync_types::{
        btc_inscription_operations::ViaBtcInscriptionRequestType,
        via_btc_sender::ViaBtcInscriptionRequest,
    };

    use crate::tests::utils::{
        create_l1_batch, default_l1_batch_metadata, get_btc_sender_config, ViaAggregatorTest,
    };

    #[tokio::test]
    async fn test_btc_inscription_aggregator_run_multiple_batch() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let config = get_btc_sender_config(1, 1);
        let mut aggregator_test = ready_batches(pool.clone(), config.clone(), 4).await;

        // ----------------------------------------- EXECTION 1 -----------------------------------------
        // Commit pubdata batch 1
        run_aggregator(pool.clone(), config.clone()).await;

        let inscription_request_list = pending(&mut aggregator_test, 1).await;

        // The first batch pubdata inscription_request was created, so the next batch to create an inscription request is 2
        assert_next(&mut aggregator_test, ViaBtcInscriptionRequestType::CommitL1BatchOnchain, 2)
            .await;

        // We confirm that the first inscription to commit batch pub data was processed.
        aggregator_test.confirme_inscription_request(inscription_request_list[0].id).await;

        pending(&mut aggregator_test, 0).await;

        // ----------------------------------------- EXECTION 2 -----------------------------------------
        // The next operation should be 'CommitProofOnchain' of the block 1.
        assert_next(&mut aggregator_test, ViaBtcInscriptionRequestType::CommitProofOnchain, 1)
            .await;

        // Commit proof batch 1 and confirm the inscription
        run_aggregator(pool.clone(), config.clone()).await;

        let inscription_request_list = pending(&mut aggregator_test, 1).await;
        assert_eq!(
            inscription_request_list[0].request_type,
            ViaBtcInscriptionRequestType::CommitProofOnchain.to_string()
        );

        // We confirm that the proof inscription was processed.
        aggregator_test.confirme_inscription_request(inscription_request_list[0].id).await;

        pending(&mut aggregator_test, 0).await;

        // ----------------------------------------- EXECTION 3 -----------------------------------------
        // The next operation should be 'CommitL1BatchOnchain' of the block 2.
        assert_next(&mut aggregator_test, ViaBtcInscriptionRequestType::CommitL1BatchOnchain, 2)
            .await;

        // Commit pubdata batch 2 without confirm the inscription wa sent to btc chain
        run_aggregator(pool.clone(), config.clone()).await;

        pending(&mut aggregator_test, 1).await;

        // Commit pubdata batch 3 without confirm the inscription wa sent to btc chain
        run_aggregator(pool.clone(), config.clone()).await;

        let inscription_request_list = pending(&mut aggregator_test, 2).await;

        assert_next(&mut aggregator_test, ViaBtcInscriptionRequestType::CommitL1BatchOnchain, 4)
            .await;

        // We confirm that the pubdata inscription was processed of batch 2.
        aggregator_test.confirme_inscription_request(inscription_request_list[0].id).await;

        let inscription_request_list = pending(&mut aggregator_test, 1).await;

        // We confirm that the pubdata inscription was processed of batch 3.
        aggregator_test.confirme_inscription_request(inscription_request_list[0].id).await;

        pending(&mut aggregator_test, 0).await;

        for batch_number in 2..=3 {
            assert_next(
                &mut aggregator_test,
                ViaBtcInscriptionRequestType::CommitProofOnchain,
                batch_number,
            )
            .await;
            run_aggregator(pool.clone(), config.clone()).await;
            let requests = pending(&mut aggregator_test, 1).await;
            aggregator_test.confirme_inscription_request(requests[0].id).await;
            pending(&mut aggregator_test, 0).await;
        }

        // The next operation should be 'CommitL1BatchOnchain' batch 4.
        assert_next(&mut aggregator_test, ViaBtcInscriptionRequestType::CommitL1BatchOnchain, 4)
            .await;

        // Commit pubdata batch 4 and confirm the inscription wa sent to btc chain
        run_aggregator(pool.clone(), config.clone()).await;

        // The next operation should be 'CommitL1BatchOnchain' batch 4.
        let op = aggregator_test.get_next_ready_operation().await;
        assert!(op.is_none());

        let inscription_request_list = pending(&mut aggregator_test, 1).await;

        aggregator_test.confirme_inscription_request(inscription_request_list[0].id).await;

        pending(&mut aggregator_test, 0).await;

        assert_next(&mut aggregator_test, ViaBtcInscriptionRequestType::CommitProofOnchain, 4)
            .await;
    }

    async fn list_new_inscription_request(
        aggregator_test: &mut ViaAggregatorTest,
        limit: i64,
    ) -> Vec<ViaBtcInscriptionRequest> {
        aggregator_test.storage.btc_sender_dal().list_new_inscription_request(limit).await.unwrap()
    }

    pub(crate) async fn run_aggregator(pool: ConnectionPool<Core>, config: ViaBtcSenderConfig) {
        let inscriber = get_mock_inscriber_and_conditions(MockBitcoinOps::default());
        let mut aggregator =
            ViaBtcInscriptionAggregator::new(inscriber, pool.clone(), config).await.unwrap();
        aggregator.loop_iteration(&mut pool.connection().await.unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn public_run_processes_request_and_stops() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let mut config = get_btc_sender_config(1, 1);
        config.poll_interval = 60_000;
        let mut fixture = ready_batches(pool.clone(), config.clone(), 1).await;
        let inscriber = get_mock_inscriber_and_conditions(MockBitcoinOps::default());
        let actor = ViaBtcInscriptionAggregator::new(inscriber, pool, config).await.unwrap();
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let mut task = tokio::spawn(actor.run(receiver));
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                tokio::select! {
                    result = &mut task => panic!("aggregator exited before creating a request: {result:?}"),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
                }
                let requests = fixture.storage.btc_sender_dal().list_new_inscription_request(10).await.unwrap();
                if !requests.is_empty() {
                    assert_eq!(requests.len(), 1);
                    assert_eq!(requests[0].request_type, ViaBtcInscriptionRequestType::CommitL1BatchOnchain.to_string());
                    break;
                }
            }
        }).await.expect("aggregator did not create a request");
        stop.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("aggregator ignored stop")
            .unwrap()
            .unwrap();
    }

    async fn ready_batches(
        pool: ConnectionPool<Core>,
        config: zksync_config::ViaBtcSenderConfig,
        count: u32,
    ) -> ViaAggregatorTest {
        let first = create_l1_batch(1);
        let mut fixture = ViaAggregatorTest::new(
            first.protocol_version.unwrap(),
            first.base_system_contracts_hashes,
            pool,
            Some(config),
        )
        .await;
        fixture.create_genesis_l1_batch().await.unwrap();
        for number in 1..=count {
            let header = create_l1_batch(number);
            fixture
                .insert_l1_batch(
                    header.clone(),
                    l1_batch_metadata_to_commitment_artifacts(&default_l1_batch_metadata()),
                )
                .await;
            fixture.insert_proof_da(header.number).await;
        }
        fixture
    }

    async fn assert_next(
        fixture: &mut ViaAggregatorTest,
        kind: ViaBtcInscriptionRequestType,
        number: u32,
    ) {
        let operation = fixture.get_next_ready_operation().await.unwrap();
        assert_eq!(operation.get_inscription_request_type(), kind);
        assert_eq!(operation.get_l1_batches_detail()[0].number.0, number);
    }

    async fn pending(
        fixture: &mut ViaAggregatorTest,
        count: usize,
    ) -> Vec<ViaBtcInscriptionRequest> {
        let requests = list_new_inscription_request(fixture, 10).await;
        assert_eq!(requests.len(), count);
        requests
    }
}
