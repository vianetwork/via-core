#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use chrono::Utc;
    use tokio::{sync::watch, time};
    use via_btc_client::inscriber::test_utils::MockBitcoinOpsConfig;
    use zksync_config::ViaBtcSenderConfig;
    use zksync_contracts::BaseSystemContractsHashes;
    use zksync_dal::{ConnectionPool, Core, CoreDal};
    use zksync_node_test_utils::l1_batch_metadata_to_commitment_artifacts;
    use zksync_types::{
        block::L1BatchHeader, btc_inscription_operations::ViaBtcInscriptionRequestType,
        ProtocolVersionId, H256,
    };

    use crate::tests::utils::{
        create_l1_batch, default_l1_batch_metadata, get_btc_sender_config,
        get_inscription_aggregator_mock, get_inscription_manager_mock, ViaAggregatorTest,
        BOOTLOADER_CODE_HASH_TEST, DEFAULT_AA_CODE_HASH_TEST,
    };

    #[tokio::test]
    async fn test_btc_inscription_manager_when_no_inscription_request() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let config = get_btc_sender_config(1, 1);
        run_manager(pool, config, MockBitcoinOpsConfig::default()).await;
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

        aggregator_test.create_genesis_l1_batch().await.unwrap();

        for header in l1_headers {
            aggregator_test
                .insert_l1_batch(
                    header.clone(),
                    l1_batch_metadata_to_commitment_artifacts(&default_l1_batch_metadata()),
                )
                .await;

            let sent_at = Utc::now().naive_utc();

            let _ = aggregator_test
                .storage
                .via_data_availability_dal()
                .insert_l1_batch_da(header.number, "blob_id", sent_at, 0)
                .await;

            let _ = aggregator_test
                .storage
                .via_data_availability_dal()
                .insert_proof_da(header.number, "blob_id", sent_at, 0)
                .await;
        }

        run_aggregator(pool.clone(), config.clone()).await;

        let inflight_inscriptions = aggregator_test
            .storage
            .btc_sender_dal()
            .list_inflight_inscription_ids()
            .await
            .unwrap();

        assert_eq!(inflight_inscriptions.len(), 0);

        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let inflight_inscription_ids = aggregator_test
            .storage
            .btc_sender_dal()
            .list_inflight_inscription_ids()
            .await
            .unwrap();

        assert_eq!(inflight_inscription_ids.len(), 1);

        let inscription_request = aggregator_test
            .storage
            .btc_sender_dal()
            .get_inscription_request(inflight_inscription_ids[0])
            .await
            .unwrap()
            .unwrap();

        assert!(inscription_request
            .confirmed_inscriptions_request_history_id
            .is_none());
        assert_eq!(
            inscription_request.request_type,
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain.to_string()
        );

        // Start the manager

        // Simulate block mint and transaction execution on chain
        mock_btc_ops_config.set_block_height(2);
        mock_btc_ops_config.set_tx_confirmation(true);

        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let inflight_inscription_ids = aggregator_test
            .storage
            .btc_sender_dal()
            .list_inflight_inscription_ids()
            .await
            .unwrap();
        assert_eq!(inflight_inscription_ids.len(), 0);

        // Start the manager

        mock_btc_ops_config.set_block_height(3);
        mock_btc_ops_config.set_tx_confirmation(false);

        // Create a commit proof inscription request
        run_aggregator(pool.clone(), config.clone()).await;

        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let inflight_inscription_ids = aggregator_test
            .storage
            .btc_sender_dal()
            .list_inflight_inscription_ids()
            .await
            .unwrap();

        assert_eq!(inflight_inscription_ids.len(), 1);

        let inscription_request = aggregator_test
            .storage
            .btc_sender_dal()
            .get_inscription_request(inflight_inscription_ids[0])
            .await
            .unwrap()
            .unwrap();

        assert!(inscription_request
            .confirmed_inscriptions_request_history_id
            .is_none());
        assert_eq!(
            inscription_request.request_type,
            ViaBtcInscriptionRequestType::CommitProofOnchain.to_string()
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
        let mut header = create_l1_batch(number);
        header.base_system_contracts_hashes = BaseSystemContractsHashes {
            bootloader: H256::from_str(BOOTLOADER_CODE_HASH_TEST).unwrap(),
            default_aa: H256::from_str(DEFAULT_AA_CODE_HASH_TEST).unwrap(),
            evm_emulator: None,
        };
        header.protocol_version = Some(ProtocolVersionId::latest());

        header
    }
}
