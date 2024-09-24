#[cfg(test)]
mod tests {

    use std::str::FromStr;

    use tokio::{sync::watch, time};
    use zksync_config::ViaBtcSenderConfig;
    use zksync_contracts::BaseSystemContractsHashes;
    use zksync_dal::{ConnectionPool, Core, CoreDal};
    use zksync_node_test_utils::{create_l1_batch, l1_batch_metadata_to_commitment_artifacts};
    use zksync_types::{
        block::L1BatchHeader, btc_inscription_operations::ViaBtcInscriptionRequestType,
        btc_sender::ViaBtcInscriptionRequest, ProtocolVersionId, H256,
    };

    use crate::tests::utils::{
        default_l1_batch_metadata, get_btc_sender_config, get_inscription_aggregator_mock,
        ViaAggregatorTest,
    };

    #[tokio::test]
    async fn test_btc_inscription_aggregator_run_multiple_batch() {
        let pool = ConnectionPool::<Core>::test_pool().await;

        let number_of_batches: u32 = 4;
        let config = get_btc_sender_config(1, 1);

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

        // ----------------------------------------- EXECTION 1 -----------------------------------------
        // Commit pubdata batch 1
        run(pool.clone(), config.clone()).await;

        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 1);

        // The first batch pubdata inscription_request was created, so the next batch to create an inscription request is 2
        let op = aggregator_test.get_next_ready_operation().await.unwrap();
        assert_eq!(
            op.get_inscription_request_type(),
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain
        );
        assert_eq!(op.get_l1_batches_detail()[0].number.0, 2);

        // We confirm that the first inscription to commit batch pub data was processed.
        aggregator_test
            .confirme_inscription_request(inscription_request_list[0].id)
            .await;

        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 0);

        // ----------------------------------------- EXECTION 2 -----------------------------------------
        // The next operation should be 'CommitProofOnchain' of the block 1.
        let op = aggregator_test.get_next_ready_operation().await.unwrap();
        assert_eq!(
            op.get_inscription_request_type(),
            ViaBtcInscriptionRequestType::CommitProofOnchain
        );
        assert_eq!(op.get_l1_batches_detail()[0].number.0, 1);

        // Commit proof batch 1 and confirm the inscription
        run(pool.clone(), config.clone()).await;

        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 1);
        assert_eq!(
            inscription_request_list[0].request_type,
            ViaBtcInscriptionRequestType::CommitProofOnchain
        );

        // We confirm that the proof inscription was processed.
        aggregator_test
            .confirme_inscription_request(inscription_request_list[0].id)
            .await;

        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 0);

        // ----------------------------------------- EXECTION 3 -----------------------------------------
        // The next operation should be 'CommitL1BatchOnchain' of the block 2.
        let op = aggregator_test.get_next_ready_operation().await.unwrap();
        assert_eq!(
            op.get_inscription_request_type(),
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain
        );
        assert_eq!(op.get_l1_batches_detail()[0].number.0, 2);

        // Commit pubdata batch 2 without confirm the inscription wa sent to btc chain
        run(pool.clone(), config.clone()).await;

        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 1);

        // Commit pubdata batch 3 without confirm the inscription wa sent to btc chain
        run(pool.clone(), config.clone()).await;

        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 2);

        let op = aggregator_test.get_next_ready_operation().await.unwrap();
        assert_eq!(
            op.get_inscription_request_type(),
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain
        );
        assert_eq!(op.get_l1_batches_detail()[0].number.0, 4);

        // We confirm that the pubdata inscription was processed of batch 2.
        aggregator_test
            .confirme_inscription_request(inscription_request_list[0].id)
            .await;

        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 1);

        // We confirm that the pubdata inscription was processed of batch 3.
        aggregator_test
            .confirme_inscription_request(inscription_request_list[0].id)
            .await;

        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 0);

        // We confirm that the proof inscription was processed of batch 3.
        let op = aggregator_test.get_next_ready_operation().await.unwrap();
        assert_eq!(
            op.get_inscription_request_type(),
            ViaBtcInscriptionRequestType::CommitProofOnchain
        );
        assert_eq!(op.get_l1_batches_detail()[0].number.0, 2);

        // Commit proof batch 2 and confirm the inscription wa sent to btc chain
        run(pool.clone(), config.clone()).await;
        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 1);
        aggregator_test
            .confirme_inscription_request(inscription_request_list[0].id)
            .await;

        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 0);

        // We confirm that the proof inscription was processed of batch 3.
        let op = aggregator_test.get_next_ready_operation().await.unwrap();
        assert_eq!(
            op.get_inscription_request_type(),
            ViaBtcInscriptionRequestType::CommitProofOnchain
        );
        assert_eq!(op.get_l1_batches_detail()[0].number.0, 3);

        // Commit proof batch 3 and confirm the inscription wa sent to btc chain
        run(pool.clone(), config.clone()).await;
        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 1);
        aggregator_test
            .confirme_inscription_request(inscription_request_list[0].id)
            .await;

        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 0);

        // The next operation should be 'CommitL1BatchOnchain' batch 4.
        let op = aggregator_test.get_next_ready_operation().await.unwrap();
        assert_eq!(
            op.get_inscription_request_type(),
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain
        );
        assert_eq!(op.get_l1_batches_detail()[0].number.0, 4);

        // Commit pubdata batch 4 and confirm the inscription wa sent to btc chain
        run(pool.clone(), config.clone()).await;

        // The next operation should be 'CommitL1BatchOnchain' batch 4.
        let op = aggregator_test.get_next_ready_operation().await;
        assert!(op.is_none());

        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 1);

        aggregator_test
            .confirme_inscription_request(inscription_request_list[0].id)
            .await;

        let inscription_request_list = list_new_inscription_request(&mut aggregator_test, 10).await;
        assert_eq!(inscription_request_list.len(), 0);

        let op = aggregator_test.get_next_ready_operation().await.unwrap();
        assert_eq!(
            op.get_inscription_request_type(),
            ViaBtcInscriptionRequestType::CommitProofOnchain
        );
        assert_eq!(op.get_l1_batches_detail()[0].number.0, 4);
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

    async fn run(pool: ConnectionPool<Core>, config: ViaBtcSenderConfig) {
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
                    break;
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

    async fn list_new_inscription_request(
        aggregator_test: &mut ViaAggregatorTest,
        limit: i64,
    ) -> Vec<ViaBtcInscriptionRequest> {
        // check/Validate the execution
        aggregator_test
            .storage
            .btc_sender_dal()
            .list_new_inscription_request(limit)
            .await
            .unwrap()
    }
}
