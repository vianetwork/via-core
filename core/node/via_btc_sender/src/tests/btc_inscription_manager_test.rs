#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use tokio::{sync::watch, time};
    use via_btc_client::inscriber::test_utils::MockBitcoinOpsConfig;
    use zksync_config::ViaBtcSenderConfig;
    use zksync_contracts::BaseSystemContractsHashes;
    use zksync_dal::{ConnectionPool, Core, CoreDal};
    use zksync_node_test_utils::{create_l1_batch, l1_batch_metadata_to_commitment_artifacts};
    use zksync_types::{
        block::L1BatchHeader, btc_inscription_operations::ViaBtcInscriptionRequestType,
        ProtocolVersionId, H256,
    };

    use crate::tests::utils::{
        default_l1_batch_metadata, get_btc_sender_config, get_inscription_aggregator_mock,
        get_inscription_manager_mock, ViaAggregatorTest,
    };

    #[tokio::test]
    async fn test_btc_inscription_manager_when_no_inscription_request() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let config = get_btc_sender_config(1, 1);
        run_manager(pool, config, MockBitcoinOpsConfig::default()).await;
    }

    #[tokio::test]
    async fn test_btc_inscription_manager_run_one_inscription_request_with_retry() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let config = get_btc_sender_config(1, 1);
        let mut mock_btc_ops_config = MockBitcoinOpsConfig::default();
        mock_btc_ops_config.set_block_height(1);

        let number_of_batches = 1;
        let mut protocol_version: Option<ProtocolVersionId> = None;
        let mut base_system_contracts_hashes: Option<BaseSystemContractsHashes> = None;
        let mut l1_headers = vec![];

        for batch_number in 1..number_of_batches + 1 {
            let header: L1BatchHeader = via_create_l1_batch(batch_number);
            l1_headers.push(header.clone());

            if protocol_version.is_none() {
                protocol_version = header.protocol_version;
            }
            if base_system_contracts_hashes.is_none() {
                base_system_contracts_hashes = Some(header.base_system_contracts_hashes);
            }
        }

        let mut aggregator_test = ViaAggregatorTest::new(
            protocol_version.unwrap(),
            base_system_contracts_hashes.unwrap(),
            pool.clone(),
            Some(config.clone()),
        )
        .await;

        for header in l1_headers {
            aggregator_test
                .insert_l1_batch(
                    header.clone(),
                    l1_batch_metadata_to_commitment_artifacts(&default_l1_batch_metadata()),
                )
                .await;
        }

        run_aggregator(pool.clone(), config.clone()).await;
        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let inflight_inscriptions_before = aggregator_test
            .storage
            .btc_sender_dal()
            .get_inflight_inscriptions()
            .await
            .unwrap();

        assert!(!inflight_inscriptions_before.is_empty());

        let last_inscription_history_before = aggregator_test
            .storage
            .btc_sender_dal()
            .get_last_inscription_request_history(inflight_inscriptions_before[0].id)
            .await
            .unwrap();

        assert!(last_inscription_history_before.is_some());

        // Simulate the transaction is stuck for 10 blocks
        mock_btc_ops_config.set_block_height(10);

        // THis hould create a new inscription_history
        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let last_inscription_history_after = aggregator_test
            .storage
            .btc_sender_dal()
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
            .btc_sender_dal()
            .get_inflight_inscriptions()
            .await
            .unwrap();

        assert!(inflight_inscriptions_after.is_empty());

        let last_inscription_history_after = aggregator_test
            .storage
            .btc_sender_dal()
            .get_last_inscription_request_history(inflight_inscriptions_before[0].id)
            .await
            .unwrap();

        assert!(last_inscription_history_after
            .unwrap()
            .confirmed_at
            .is_some());
    }

    #[tokio::test]
    async fn test_btc_inscription_manager_run_one_inscription_request() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let config = get_btc_sender_config(1, 1);
        let mut mock_btc_ops_config = MockBitcoinOpsConfig::default();
        mock_btc_ops_config.set_block_height(1);

        let number_of_batches: u32 = 1;

        let mut protocol_version: Option<ProtocolVersionId> = None;
        let mut base_system_contracts_hashes: Option<BaseSystemContractsHashes> = None;
        let mut l1_headers = vec![];

        for batch_number in 1..number_of_batches + 1 {
            let header: L1BatchHeader = via_create_l1_batch(batch_number);
            l1_headers.push(header.clone());

            if protocol_version.is_none() {
                protocol_version = header.protocol_version;
            }
            if base_system_contracts_hashes.is_none() {
                base_system_contracts_hashes = Some(header.base_system_contracts_hashes);
            }
        }

        let mut aggregator_test = ViaAggregatorTest::new(
            protocol_version.unwrap(),
            base_system_contracts_hashes.unwrap(),
            pool.clone(),
            Some(config.clone()),
        )
        .await;

        for header in l1_headers {
            aggregator_test
                .insert_l1_batch(
                    header.clone(),
                    l1_batch_metadata_to_commitment_artifacts(&default_l1_batch_metadata()),
                )
                .await;
        }

        run_aggregator(pool.clone(), config.clone()).await;

        let inflight_inscriptions = aggregator_test
            .storage
            .btc_sender_dal()
            .get_inflight_inscriptions()
            .await
            .unwrap();

        assert_eq!(inflight_inscriptions.len(), 0);

        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let inflight_inscriptions = aggregator_test
            .storage
            .btc_sender_dal()
            .get_inflight_inscriptions()
            .await
            .unwrap();

        assert_eq!(inflight_inscriptions.len(), 1);
        assert!(inflight_inscriptions[0]
            .confirmed_inscriptions_request_history_id
            .is_none());
        assert_eq!(
            inflight_inscriptions[0].request_type,
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain
        );

        // Start the manager

        // Simulate block mint and transaction execution on chain
        mock_btc_ops_config.set_block_height(2);
        mock_btc_ops_config.set_tx_confirmation(true);

        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let inflight_inscriptions = aggregator_test
            .storage
            .btc_sender_dal()
            .get_inflight_inscriptions()
            .await
            .unwrap();
        assert_eq!(inflight_inscriptions.len(), 0);

        // Start the manager

        mock_btc_ops_config.set_block_height(3);
        mock_btc_ops_config.set_tx_confirmation(false);

        // Create a commit proof inscription request
        run_aggregator(pool.clone(), config.clone()).await;

        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let inflight_inscriptions = aggregator_test
            .storage
            .btc_sender_dal()
            .get_inflight_inscriptions()
            .await
            .unwrap();

        assert_eq!(inflight_inscriptions.len(), 1);

        assert!(inflight_inscriptions[0]
            .confirmed_inscriptions_request_history_id
            .is_none());
        assert_eq!(
            inflight_inscriptions[0].request_type,
            ViaBtcInscriptionRequestType::CommitProofOnchain
        );
    }

    async fn run_aggregator(pool: ConnectionPool<Core>, config: ViaBtcSenderConfig) {
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

            let inscription_aggregator_mock =
                get_inscription_aggregator_mock(pool.clone(), config.clone()).await;

            inscription_aggregator_mock.run(receiver).await.unwrap();
            if let Err(e) = toggle_handler.await {
                eprintln!("Toggle task failed: {:?}", e);
            }
        }
    }

    async fn run_manager(
        pool: ConnectionPool<Core>,
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

    pub fn via_create_l1_batch(number: u32) -> L1BatchHeader {
        let hex_str = "0000000000000000000000000000000000000000000000000000000000000000";
        let mut header = create_l1_batch(number);
        header.base_system_contracts_hashes = BaseSystemContractsHashes {
            bootloader: H256::from_str(hex_str).unwrap(),
            default_aa: H256::from_str(hex_str).unwrap(),
        };
        header.protocol_version = Some(ProtocolVersionId::latest());

        header
    }
}
