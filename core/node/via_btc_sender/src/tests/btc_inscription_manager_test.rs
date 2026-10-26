#[cfg(test)]
pub(crate) mod tests {
    use crate::btc_inscription_aggregator::test_support::tests::run_aggregator;
    use via_btc_client::inscriber::test_utils::MockBitcoinOps;
    use zksync_config::ViaBtcSenderConfig;
    use zksync_dal::{ConnectionPool, Core, CoreDal};
    use zksync_node_test_utils::l1_batch_metadata_to_commitment_artifacts;
    use zksync_types::{btc_inscription_operations::ViaBtcInscriptionRequestType};

    use crate::tests::utils::{
        create_l1_batch, default_l1_batch_metadata, get_btc_sender_config,
        get_inscription_manager_mock, ViaAggregatorTest,
    };

    #[tokio::test]
    async fn test_btc_inscription_manager_when_no_inscription_request() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let config = get_btc_sender_config(1, 1);
        let header = create_l1_batch(0);
        let mut fixture = ViaAggregatorTest::new(
            header.protocol_version.unwrap(),
            header.base_system_contracts_hashes,
            pool.clone(),
            None,
        )
        .await;
        fixture.create_genesis_l1_batch().await.unwrap();
        run_manager(pool.clone(), config, MockBitcoinOps::default()).await;
        assert!(pool
            .connection()
            .await
            .unwrap()
            .btc_sender_dal()
            .list_inflight_inscription_ids()
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_btc_inscription_manager_run_one_inscription_request() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let config = get_btc_sender_config(1, 1);
        let mut mock_btc_ops_config = MockBitcoinOps::default();
        mock_btc_ops_config.set_block_height(1);

        let mut aggregator_test = single_batch(pool.clone(), config.clone()).await;

        run_aggregator(pool.clone(), config.clone()).await;

        let inflight_inscriptions =
            aggregator_test.storage.btc_sender_dal().list_inflight_inscription_ids().await.unwrap();

        assert_eq!(inflight_inscriptions.len(), 0);

        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        assert_inflight_request(
            &mut aggregator_test,
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain,
        )
        .await;

        // Start the manager

        // Simulate block mint and transaction execution on chain
        mock_btc_ops_config.set_block_height(2);
        mock_btc_ops_config.set_tx_confirmation(true);

        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let inflight_inscription_ids =
            aggregator_test.storage.btc_sender_dal().list_inflight_inscription_ids().await.unwrap();
        assert_eq!(inflight_inscription_ids.len(), 0);

        // Start the manager

        mock_btc_ops_config.set_block_height(3);
        mock_btc_ops_config.set_tx_confirmation(false);

        // Create a commit proof inscription request
        run_aggregator(pool.clone(), config.clone()).await;

        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        assert_inflight_request(
            &mut aggregator_test,
            ViaBtcInscriptionRequestType::CommitProofOnchain,
        )
        .await;
    }

    async fn run_manager(
        pool: ConnectionPool<Core>,
        config: ViaBtcSenderConfig,
        mock_btc_ops_config: MockBitcoinOps,
    ) {
        let mut manager =
            get_inscription_manager_mock(pool.clone(), config, mock_btc_ops_config).await;
        manager.loop_iteration(&mut pool.connection().await.unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn public_run_submits_request_and_stops() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let mut config = get_btc_sender_config(1, 1);
        config.poll_interval = 60_000;
        let mut fixture = single_batch(pool.clone(), config.clone()).await;
        run_aggregator(pool.clone(), config.clone()).await;
        let request = fixture
            .storage
            .btc_sender_dal()
            .list_new_inscription_request(10)
            .await
            .unwrap()
            .remove(0);
        let manager = get_inscription_manager_mock(pool, config, MockBitcoinOps::default()).await;
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(manager.run(receiver));
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                assert!(!task.is_finished(), "manager exited before submitting the request");
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                let inflight =
                    fixture.storage.btc_sender_dal().list_inflight_inscription_ids().await.unwrap();
                if !inflight.is_empty() {
                    assert_eq!(inflight, vec![request.id]);
                    break;
                }
            }
        })
        .await
        .expect("manager did not submit the request");
        stop.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("manager ignored stop")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cached_publication_requires_current_source_and_mature_sequencer() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let config = get_btc_sender_config(1, 1);
        let mut fixture = single_batch(pool.clone(), config.clone()).await;
        run_aggregator(pool.clone(), config.clone()).await;
        let mut manager =
            get_inscription_manager_mock(pool.clone(), config, MockBitcoinOps::default()).await;
        let sequencer: String =
            sqlx::query_scalar("SELECT address FROM via_wallets WHERE role='Sequencer'")
                .fetch_one(fixture.storage.conn())
                .await
                .unwrap();
        for hold in ["UPDATE via_btc_reader SET replay_ready=FALSE","UPDATE via_btc_reader SET confirmed_hash=genesis_hash","UPDATE via_wallets SET address=(SELECT address FROM via_wallets WHERE role='Gov' LIMIT 1) WHERE role='Sequencer'"] {
            sqlx::query(hold).execute(fixture.storage.conn()).await.unwrap();
            assert!(manager.loop_iteration(&mut fixture.storage).await.is_err());
            assert!(fixture.storage.btc_sender_dal().list_inflight_inscription_ids().await.unwrap().is_empty());
            sqlx::query("UPDATE via_btc_reader SET replay_ready=TRUE,confirmed_hash=(SELECT hash FROM via_btc_headers WHERE number=confirmed_number)").execute(fixture.storage.conn()).await.unwrap();
        }
        sqlx::query("UPDATE via_wallets SET address=$1 WHERE role='Sequencer'")
            .bind(sequencer)
            .execute(fixture.storage.conn())
            .await
            .unwrap();
        sqlx::query("UPDATE via_btc_reader SET confirmed_hash=(SELECT hash FROM via_btc_headers WHERE number=confirmed_number)").execute(fixture.storage.conn()).await.unwrap();
        manager.loop_iteration(&mut fixture.storage).await.unwrap();
        assert_eq!(
            fixture.storage.btc_sender_dal().list_inflight_inscription_ids().await.unwrap().len(),
            1
        );
    }

    async fn single_batch(
        pool: ConnectionPool<Core>,
        config: ViaBtcSenderConfig,
    ) -> ViaAggregatorTest {
        let header = create_l1_batch(1);
        let mut fixture = ViaAggregatorTest::new(
            header.protocol_version.unwrap(),
            header.base_system_contracts_hashes,
            pool.clone(),
            Some(config.clone()),
        )
        .await;
        fixture.create_genesis_l1_batch().await.unwrap();
        fixture
            .insert_l1_batch(
                header.clone(),
                l1_batch_metadata_to_commitment_artifacts(&default_l1_batch_metadata()),
            )
            .await;
        fixture.insert_proof_da(header.number).await;
        get_inscription_manager_mock(pool, config, MockBitcoinOps::default()).await;
        fixture
    }

    async fn assert_inflight_request(
        fixture: &mut ViaAggregatorTest,
        kind: ViaBtcInscriptionRequestType,
    ) {
        let ids = fixture.storage.btc_sender_dal().list_inflight_inscription_ids().await.unwrap();
        assert_eq!(ids.len(), 1);
        let request = fixture
            .storage
            .btc_sender_dal()
            .get_inscription_request(ids[0])
            .await
            .unwrap()
            .unwrap();
        assert!(request.confirmed_inscriptions_request_history_id.is_none());
        assert_eq!(request.request_type, kind.to_string());
    }
}
