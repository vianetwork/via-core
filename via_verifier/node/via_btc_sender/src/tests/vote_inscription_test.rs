#[cfg(test)]
mod tests {
    use bitcoin::BlockHash;

    use tokio::{sync::watch, time};
    use via_btc_client::{
        inscriber::test_utils::MockBitcoinOpsConfig, traits::Serializable, types::InscriptionMessage,
    };
    use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
    use zksync_config::ViaBtcSenderConfig;
    use zksync_types::H256;

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
                config = Some(get_btc_sender_config(1, 1));
            }
            let aggregator = ViaVoteInscription::new(pool, config.unwrap()).await.unwrap();

            Self { aggregator, storage }
        }
    }

    async fn seed_pending(storage: &mut Connection<'_, Verifier>, tx_id: H256) -> i64 {
        use bitcoin::{hashes::Hash, Txid};
        let mut tx = storage.start_transaction().await.unwrap();
        tx.via_indexer_dal().lock_source_writer().await.unwrap();
        let header = via_test_utils::reader_db::first_source(tx.conn(), 1).await.unwrap();
        let set = via_test_utils::reader_db::bootstrap_verifiers(
            tx.conn(),
            &[get_btc_sender_config(1, 1).wallet_address],
            0,
        )
        .await
        .unwrap();
        let mut raw = tx_id.0;
        raw.reverse();
        let proof = Txid::from_byte_array(raw);
        let input = via_test_utils::reader_db::ProofVerdict {
            batch: 1,
            batch_hash: H256::random(),
            parent_hash: H256::zero(),
            proof_txid: proof,
            proof_blob: "proof".into(),
            batch_txid: "batch-tx".into(),
            batch_blob: "batch".into(),
            da_identifier: "fixture".into(),
        };
        let id = via_test_utils::reader_db::verdict(
            tx.conn(),
            &input,
            via_reorg::chain::MessageRef {
                block: header.into(),
                carrier_txid: proof,
                tx_index: 1,
                message_index: 0,
            },
            set,
        )
        .await
        .unwrap();
        tx.via_indexer_dal().init_indexer_metadata("via_btc_watch", 1).await.unwrap();
        tx.commit().await.unwrap();
        id
    }

    async fn approve(storage: &mut Connection<'_, Verifier>, id: i64) {
        let mut tx = storage.start_transaction().await.unwrap();
        let inputs = tx.via_votes_dal().verification_snapshot(id, 0).await.unwrap().unwrap();
        tx.via_votes_dal().mark_proof_verified(id, &inputs).await.unwrap();
        tx.commit().await.unwrap();
    }

    // Get the current operation (commitBatch or commitProof) to execute when there is no batches. Should return 'None'
    #[tokio::test]
    async fn test_get_next_ready_vote_operation() {
        use bitcoin::{hashes::Hash, Txid};
        use zksync_types::via_wallet::{SystemWalletsDetails, WalletInfo, WalletRole};
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut aggregator_test = ViaVoteInscriptionTest::new(pool.clone(), None).await;

        let tx_id = H256::random();
        let id = seed_pending(&mut aggregator_test.storage, tx_id).await;

        let op = aggregator_test.aggregator.get_voting_operation(&mut aggregator_test.storage).await.unwrap();
        assert!(op.is_none());

        approve(&mut aggregator_test.storage, id).await;

        let other_address = via_test_utils::utils::random_bitcoin_wallet().1;
        let mut other_config = get_btc_sender_config(1, 1);
        other_config.wallet_address = other_address.to_string();
        let mut other = ViaVoteInscription::new(pool.clone(), other_config).await.unwrap();
        assert!(other.get_voting_operation(&mut aggregator_test.storage).await.unwrap().is_none());
        let mut transaction = aggregator_test.storage.start_transaction().await.unwrap();
        transaction.via_indexer_dal().lock_source_writer().await.unwrap();
        let header = via_reorg::chain::Header {
            height: 2,
            block_hash: BlockHash::from_byte_array([2; 32]),
            parent_hash: BlockHash::from_byte_array([1; 32]),
        };
        transaction.via_l1_block_dal().accept_header(header).await.unwrap();
        let mut latest = SystemWalletsDetails::default();
        latest.0.insert(
            WalletRole::Verifier,
            WalletInfo { addresses: vec![other_address], txid: Txid::from_byte_array([50; 32]) },
        );
        transaction
            .via_wallet_dal()
            .record_wallet_update(
                &latest,
                via_reorg::chain::MessageRef {
                    block: header.into(),
                    carrier_txid: Txid::from_byte_array([50; 32]),
                    tx_index: 0,
                    message_index: 0,
                },
                &serde_json::json!({"fixture":"newer-set"}),
                "accepted",
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        assert!(other.get_voting_operation(&mut aggregator_test.storage).await.unwrap().is_none());

        let op = aggregator_test.aggregator.get_voting_operation(&mut aggregator_test.storage).await.unwrap();
        assert!(op.is_some());
        let (votable_transaction_id, vote, tx_id_vec) = op.unwrap();
        assert_eq!(votable_transaction_id, 1);
        assert!(vote);
        assert_eq!(H256::from_slice(&tx_id_vec), tx_id);

        let mut transaction = aggregator_test.storage.start_transaction().await.unwrap();
        let observed_vote = Txid::from_byte_array([51; 32]);
        let mut proof = tx_id.0;
        proof.reverse();
        transaction
            .via_votes_dal()
            .record_reader_vote(
                observed_vote,
                Txid::from_byte_array(proof),
                &get_btc_sender_config(1, 1).wallet_address,
                true,
                &serde_json::json!({"fixture":"own-vote"}),
                via_reorg::chain::MessageRef {
                    block: header.into(),
                    carrier_txid: observed_vote,
                    tx_index: 1,
                    message_index: 0,
                },
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        assert!(aggregator_test
            .aggregator
            .get_voting_operation(&mut aggregator_test.storage)
            .await
            .unwrap()
            .is_none());
        let mut transaction = aggregator_test.storage.start_transaction().await.unwrap();
        transaction.via_votes_dal().revoke_reader_inclusions_after(1).await.unwrap();
        transaction.commit().await.unwrap();
        assert!(aggregator_test
            .aggregator
            .get_voting_operation(&mut aggregator_test.storage)
            .await
            .unwrap()
            .is_some());

        let inscription = aggregator_test.aggregator.construct_voting_inscription_message(vote, tx_id_vec).unwrap();

        aggregator_test.aggregator.loop_iteration(&mut aggregator_test.storage).await.unwrap();

        let inscriptions = aggregator_test.storage.via_btc_sender_dal().list_new_inscription_request(10).await.unwrap();
        assert_eq!(inscriptions.len(), 1);

        assert_eq!(
            InscriptionMessage::from_bytes(inscriptions.first().unwrap().inscription_message.as_ref().unwrap()),
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

        let id = seed_pending(&mut aggregator_test.storage, tx_id).await;

        approve(&mut aggregator_test.storage, id).await;

        run_aggregator(pool.clone()).await;
        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let inflight_inscriptions_before =
            aggregator_test.storage.via_btc_sender_dal().get_inflight_inscriptions().await.unwrap();

        assert!(!inflight_inscriptions_before.is_empty());

        let last_inscription_history_before = aggregator_test
            .storage
            .via_btc_sender_dal()
            .get_last_inscription_request_history(inflight_inscriptions_before[0].id)
            .await
            .unwrap();

        assert!(last_inscription_history_before.is_some());

        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        // Simulate the transaction was processed in next block
        mock_btc_ops_config.set_block_height(11);
        mock_btc_ops_config.set_tx_confirmation(true);

        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;

        let inflight_inscriptions_after =
            aggregator_test.storage.via_btc_sender_dal().get_inflight_inscriptions().await.unwrap();

        assert!(inflight_inscriptions_after.is_empty());

        let last_inscription_history_after = aggregator_test
            .storage
            .via_btc_sender_dal()
            .get_last_inscription_request_history(inflight_inscriptions_before[0].id)
            .await
            .unwrap();

        assert!(last_inscription_history_after.unwrap().confirmed_at.is_some());

        // Run the manager to make sure there is no unexpected behavior
        run_manager(pool.clone(), config.clone(), mock_btc_ops_config.clone()).await;
    }

    async fn run_aggregator(pool: ConnectionPool<Verifier>) {
        {
            let aggregator_test = ViaVoteInscriptionTest::new(pool.clone(), None).await;

            let (receiver, stop_task) = stop_after_poll();
            aggregator_test.aggregator.run(receiver).await.unwrap();
            if let Err(e) = stop_task.await {
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
            let inscription_manager_mock =
                get_inscription_manager_mock(pool.clone(), config.clone(), mock_btc_ops_config).await;

            let (receiver, stop_task) = stop_after_poll();
            inscription_manager_mock.run(receiver).await.unwrap();
            if let Err(e) = stop_task.await {
                eprintln!("Toggle task failed: {:?}", e);
            }
        }
    }
    fn stop_after_poll() -> (watch::Receiver<bool>, tokio::task::JoinHandle<()>) {
        let (sender, receiver) = watch::channel(false);
        let task = tokio::spawn(async move {
            time::sleep(time::Duration::from_secs(3)).await;
            let _ = sender.send(true);
        });
        (receiver, task)
    }
}
