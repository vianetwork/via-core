#[cfg(test)]
mod tests {

    use tokio::{sync::watch, time};
    use via_btc_client::{
        inscriber::test_utils::MockBitcoinOpsConfig, traits::Serializable,
        types::InscriptionMessage,
    };
    use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
    use zksync_config::ViaBtcSenderConfig;
    use zksync_types::{L1BatchNumber, H256};

    use crate::{
        btc_vote_inscription::ViaVoteInscription,
        tests::utils::{get_btc_sender_config, get_inscription_manager_mock},
    };

    pub struct ViaVoteInscriptionTest {
        pub aggregator: ViaVoteInscription,
        pub storage: Connection<'static, Verifier>,
    }

    impl ViaVoteInscriptionTest {
        pub async fn new(
            pool: ConnectionPool<Verifier>,
            mut config: Option<ViaBtcSenderConfig>,
        ) -> Self {
            let storage = pool.connection().await.unwrap();

            if config.is_none() {
                config = Some(ViaBtcSenderConfig::for_tests());
            }
            let aggregator = ViaVoteInscription::new(pool, config.unwrap())
                .await
                .unwrap();

            Self {
                aggregator,
                storage,
            }
        }
    }

    // Get the current operation (commitBatch or commitProof) to execute when there is no batches. Should return 'None'
    #[tokio::test]
    async fn test_get_next_ready_vote_operation() {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut aggregator_test = ViaVoteInscriptionTest::new(pool, None).await;

        let tx_id = H256::random();
        let _ = aggregator_test
            .storage
            .via_votes_dal()
            .insert_votable_transaction(
                1,
                tx_id,
                "".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
            )
            .await;

        let op = aggregator_test
            .aggregator
            .get_voting_operation(&mut aggregator_test.storage)
            .await
            .unwrap();
        assert!(op.is_none());

        let _ = aggregator_test
            .storage
            .via_votes_dal()
            .verify_votable_transaction(1, tx_id, true)
            .await;

        let op = aggregator_test
            .aggregator
            .get_voting_operation(&mut aggregator_test.storage)
            .await
            .unwrap();
        assert!(op.is_some());
        let (l1_block_number, vote, tx_id_vec) = op.unwrap();
        assert_eq!(l1_block_number, L1BatchNumber::from(1));
        assert!(vote);
        assert_eq!(H256::from_slice(&tx_id_vec), tx_id);

        let inscription = aggregator_test
            .aggregator
            .construct_voting_inscription_message(vote, tx_id_vec)
            .unwrap();

        aggregator_test
            .aggregator
            .loop_iteration(&mut aggregator_test.storage)
            .await
            .unwrap();

        let inscriptions = aggregator_test
            .storage
            .via_btc_sender_dal()
            .list_new_inscription_request(10)
            .await
            .unwrap();
        assert_eq!(inscriptions.len(), 1);

        assert_eq!(
            InscriptionMessage::from_bytes(
                inscriptions
                    .first()
                    .unwrap()
                    .inscription_message
                    .as_ref()
                    .unwrap()
            ),
            inscription
        );
    }

    #[tokio::test]
    async fn test_verifier_vote_inscription_manager() {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let config = get_btc_sender_config(1, 1);
        let mut mock_btc_ops_config = MockBitcoinOpsConfig::default();
        mock_btc_ops_config.set_block_height(1);

        let mut aggregator_test = ViaVoteInscriptionTest::new(pool.clone(), None).await;

        let tx_id = H256::random();

        let _ = aggregator_test
            .storage
            .via_votes_dal()
            .insert_votable_transaction(
                1,
                tx_id,
                "".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
            )
            .await;

        let _ = aggregator_test
            .storage
            .via_votes_dal()
            .verify_votable_transaction(1, tx_id, true)
            .await;

        run_aggregator(pool.clone()).await;
        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let inflight_inscriptions_before = aggregator_test
            .storage
            .via_btc_sender_dal()
            .get_inflight_inscriptions()
            .await
            .unwrap();

        assert!(!inflight_inscriptions_before.is_empty());

        let last_inscription_history_before = aggregator_test
            .storage
            .via_btc_sender_dal()
            .get_last_inscription_request_history(inflight_inscriptions_before[0].id)
            .await
            .unwrap();

        assert!(last_inscription_history_before.is_some());

        // Simulate the transaction is stuck for 10 blocks
        mock_btc_ops_config.set_block_height(10);

        // THis should create a new inscription_history
        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let last_inscription_history_after = aggregator_test
            .storage
            .via_btc_sender_dal()
            .get_last_inscription_request_history(inflight_inscriptions_before[0].id)
            .await
            .unwrap();

        assert!(last_inscription_history_after.is_some());

        assert_ne!(
            last_inscription_history_after.unwrap().id,
            last_inscription_history_before.unwrap().id
        );

        // Simulate the transaction was processed in next block
        mock_btc_ops_config.set_block_height(11);
        mock_btc_ops_config.set_tx_confirmation(true);

        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let inflight_inscriptions_after = aggregator_test
            .storage
            .via_btc_sender_dal()
            .get_inflight_inscriptions()
            .await
            .unwrap();

        assert!(inflight_inscriptions_after.is_empty());

        let last_inscription_history_after = aggregator_test
            .storage
            .via_btc_sender_dal()
            .get_last_inscription_request_history(inflight_inscriptions_before[0].id)
            .await
            .unwrap();

        assert!(last_inscription_history_after
            .unwrap()
            .confirmed_at
            .is_some());

        // Run the manager to make sure there is no unexpected behavior
        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;
    }

    async fn run_aggregator(pool: ConnectionPool<Verifier>) {
        {
            // Create an async channel to break the while loop afer 3 seconds.
            let (sender, receiver): (watch::Sender<bool>, watch::Receiver<bool>) =
                watch::channel(false);

            let toggle_handler = tokio::spawn(async move {
                let mut toggle = false;

                loop {
                    time::sleep(time::Duration::from_secs(3)).await;
                    toggle = !toggle;
                    if sender.send(toggle).is_err() {
                        break;
                    }
                    println!("Sent: {}", toggle);
                }
            });

            let aggregator_test = ViaVoteInscriptionTest::new(pool.clone(), None).await;

            aggregator_test.aggregator.run(receiver).await.unwrap();
            if let Err(e) = toggle_handler.await {
                eprintln!("Toggle task failed: {:?}", e);
            }
        }
    }

    async fn run_manager(
        pool: ConnectionPool<Verifier>,
        config: ViaBtcSenderConfig,
        mock_btc_ops_config: MockBitcoinOpsConfig,
    ) {
        {
            // Create an async channel to break the while loop afer 3 seconds.
            let (sender, receiver): (watch::Sender<bool>, watch::Receiver<bool>) =
                watch::channel(false);

            let toggle_handler = tokio::spawn(async move {
                let mut toggle = false;

                loop {
                    time::sleep(time::Duration::from_secs(3)).await;
                    toggle = !toggle;
                    if sender.send(toggle).is_err() {
                        break;
                    }
                    println!("Sent: {}", toggle);
                }
            });

            let inscription_manager_mock =
                get_inscription_manager_mock(pool.clone(), config.clone(), mock_btc_ops_config)
                    .await;

            inscription_manager_mock.run(receiver).await.unwrap();
            if let Err(e) = toggle_handler.await {
                eprintln!("Toggle task failed: {:?}", e);
            }
        }
    }
}
