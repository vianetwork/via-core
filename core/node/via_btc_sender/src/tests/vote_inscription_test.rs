#[cfg(test)]
mod tests {

    use chrono::Utc;
    use tokio::{sync::watch, time};
    use via_btc_client::{
        inscriber::test_utils::MockBitcoinOpsConfig, traits::Serializable,
        types::InscriptionMessage,
    };
    use zksync_config::ViaBtcSenderConfig;
    use zksync_contracts::BaseSystemContractsHashes;
    use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
    use zksync_node_test_utils::l1_batch_metadata_to_commitment_artifacts;
    use zksync_types::{
        block::{L1BatchHeader, L1BatchTreeData},
        commitment::L1BatchCommitmentArtifacts,
        protocol_version::{L1VerifierConfig, ProtocolSemanticVersion},
        L1BatchNumber, ProtocolVersionId, H256,
    };

    use crate::{
        btc_vote_inscription::ViaVoteInscription,
        tests::utils::{
            create_l1_batch, default_l1_batch_metadata, generate_random_bytes,
            get_btc_sender_config, get_inscription_manager_mock,
        },
    };

    pub struct ViaVoteInscriptionTest {
        pub aggregator: ViaVoteInscription,
        pub storage: Connection<'static, Core>,
    }

    impl ViaVoteInscriptionTest {
        pub async fn new(
            protocol_version: ProtocolVersionId,
            base_system_contracts_hashes: BaseSystemContractsHashes,
            pool: ConnectionPool<Core>,
            mut config: Option<ViaBtcSenderConfig>,
        ) -> Self {
            let mut storage = pool.connection().await.unwrap();

            if config.is_none() {
                config = Some(ViaBtcSenderConfig::for_tests());
            }
            let aggregator = ViaVoteInscription::new(pool, config.unwrap())
                .await
                .unwrap();

            let timestamp = Utc::now().timestamp() as u64;
            let protocol_version = zksync_types::ProtocolVersion {
                l1_verifier_config: L1VerifierConfig {
                    recursion_scheduler_level_vk_hash: H256::random(),
                },
                base_system_contracts_hashes,
                timestamp,
                tx: None,
                version: ProtocolSemanticVersion {
                    minor: protocol_version,
                    patch: 0.into(),
                },
            };

            storage
                .protocol_versions_dal()
                .save_protocol_version_with_tx(&protocol_version)
                .await
                .unwrap();

            Self {
                aggregator,
                storage,
            }
        }

        pub async fn insert_l1_batch(
            &mut self,
            header: L1BatchHeader,
            l1_commitment_artifacts: L1BatchCommitmentArtifacts,
        ) {
            self.storage
                .blocks_dal()
                .insert_mock_l1_batch(&header)
                .await
                .unwrap();

            self.storage
                .blocks_dal()
                .save_l1_batch_tree_data(
                    header.number,
                    &L1BatchTreeData {
                        hash: H256::random(),
                        rollup_last_leaf_index: 1,
                    },
                )
                .await
                .unwrap();

            self.storage
                .blocks_dal()
                .save_l1_batch_commitment_artifacts(header.number, &l1_commitment_artifacts)
                .await
                .unwrap();

            let time = Utc::now().naive_utc();

            self.storage
                .via_data_availability_dal()
                .insert_l1_batch_da(header.number, "blob_id", time)
                .await
                .expect("insert_l1_batch_da");

            let random_slice: &[u8] = &generate_random_bytes(32);

            self.storage
                .via_data_availability_dal()
                .save_l1_batch_inclusion_data(header.number, random_slice)
                .await
                .expect("save_l1_batch_inclusion_data");
        }
    }

    // Get the current operation (commitBatch or commitProof) to execute when there is no batches. Should return 'None'
    #[tokio::test]
    async fn test_get_next_ready_vote_operation() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let header = create_l1_batch(1);
        let mut aggregator_test = ViaVoteInscriptionTest::new(
            header.protocol_version.unwrap(),
            header.base_system_contracts_hashes,
            pool,
            None,
        )
        .await;

        aggregator_test
            .insert_l1_batch(
                header.clone(),
                l1_batch_metadata_to_commitment_artifacts(&default_l1_batch_metadata()),
            )
            .await;

        let tx_id = H256::random();
        let _ = aggregator_test
            .storage
            .via_votes_dal()
            .insert_votable_transaction(1, tx_id)
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
            .btc_sender_dal()
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
        let pool = ConnectionPool::<Core>::test_pool().await;
        let config = get_btc_sender_config(1, 1);
        let mut mock_btc_ops_config = MockBitcoinOpsConfig::default();
        mock_btc_ops_config.set_block_height(1);

        let header = create_l1_batch(1);
        let mut aggregator_test = ViaVoteInscriptionTest::new(
            header.protocol_version.unwrap(),
            header.base_system_contracts_hashes,
            pool.clone(),
            None,
        )
        .await;

        aggregator_test
            .insert_l1_batch(
                header.clone(),
                l1_batch_metadata_to_commitment_artifacts(&default_l1_batch_metadata()),
            )
            .await;
        let tx_id = H256::random();

        let _ = aggregator_test
            .storage
            .via_votes_dal()
            .insert_votable_transaction(1, tx_id)
            .await;

        let _ = aggregator_test
            .storage
            .via_votes_dal()
            .verify_votable_transaction(1, tx_id, true)
            .await;

        run_aggregator(header, pool.clone()).await;
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

        // THis should create a new inscription_history
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

        // Run the manager to make sure there is no unexpected behavior
        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;
    }

    async fn run_aggregator(header: L1BatchHeader, pool: ConnectionPool<Core>) {
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

            let aggregator_test = ViaVoteInscriptionTest::new(
                header.protocol_version.unwrap(),
                header.base_system_contracts_hashes,
                pool.clone(),
                None,
            )
            .await;

            aggregator_test.aggregator.run(receiver).await.unwrap();
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
}
