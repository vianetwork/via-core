use chrono::{DateTime, Utc};
use zksync_node_sync::batch_status_updater::BatchStatusUpdater;
use zksync_types::{
    aggregated_operations::AggregatedActionType,
    btc_inscription_operations::ViaBtcInscriptionRequestType, SLChainId,
};

use super::*;

fn stage_time(batch: u32, stage: u32) -> DateTime<Utc> {
    DateTime::from_timestamp(i64::from(100 * stage + batch), 0).unwrap()
}

fn stage_hash(batch: u32, stage: u32) -> H256 {
    H256::from_low_u64_be(u64::from(100 * stage + batch))
}

async fn confirm_inscription(
    storage: &mut Connection<'_, Core>,
    batch: u32,
    stage: u32,
) -> anyhow::Result<()> {
    let request_type = if stage == 1 {
        ViaBtcInscriptionRequestType::CommitL1BatchOnchain
    } else {
        ViaBtcInscriptionRequestType::CommitProofOnchain
    };
    let request_id = storage
        .btc_sender_dal()
        .via_save_btc_inscriptions_request(
            L1BatchNumber(batch),
            request_type.to_string(),
            vec![],
            0,
        )
        .await?;
    let mut reveal_txid = stage_hash(batch, stage).as_bytes().to_vec();
    reveal_txid.reverse();
    let history_id = storage
        .btc_sender_dal()
        .insert_inscription_request_history(
            stage_hash(batch, stage + 10).as_bytes(),
            &reveal_txid,
            request_id,
            &[],
            &[],
            0,
            1,
        )
        .await?;
    storage
        .btc_sender_dal()
        .confirm_inscription(request_id, history_id)
        .await?;
    storage
        .via_blocks_dal()
        .insert_l1_batch_inscription_request_id(L1BatchNumber(batch), request_id, request_type)
        .await?;
    sqlx::query("UPDATE via_btc_inscriptions_request_history SET confirmed_at = $1 WHERE id = $2")
        .bind(stage_time(batch, stage).naive_utc())
        .bind(history_id)
        .execute(storage.conn())
        .await?;
    Ok(())
}

async fn synchronize_stage(
    main_client: &DynClient<L2>,
    en_pool: &ConnectionPool<Core>,
    stage: u32,
) -> anyhow::Result<()> {
    let updater = BatchStatusUpdater::new(main_client.clone_boxed(), en_pool.clone());
    let (stop_sender, stop_receiver) = watch::channel(false);
    let task = tokio::spawn(updater.run(stop_receiver));
    let result = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let mut storage = en_pool.connection().await?;
            let last = match stage {
                1 => {
                    storage
                        .blocks_dal()
                        .get_number_of_last_l1_batch_committed_on_eth()
                        .await?
                }
                2 => {
                    storage
                        .blocks_dal()
                        .get_number_of_last_l1_batch_proven_on_eth()
                        .await?
                }
                _ => {
                    storage
                        .blocks_dal()
                        .get_number_of_last_l1_batch_executed_on_eth()
                        .await?
                }
            };
            if last == Some(L1BatchNumber(if stage == 3 { 2 } else { 3 })) {
                return anyhow::Ok(());
            }
            anyhow::ensure!(
                !task.is_finished(),
                "status updater exited before synchronization"
            );
            drop(storage);
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await;
    stop_sender.send_replace(true);
    task.await??;
    result??;
    Ok(())
}

async fn assert_details(
    client: &DynClient<L2>,
    batch: u32,
    tx_hash: H256,
    stage: u32,
    external: bool,
) -> anyhow::Result<()> {
    let batch_details = client
        .get_l1_batch_details(L1BatchNumber(batch))
        .await?
        .unwrap();
    let block_details = client
        .get_block_details(L2BlockNumber(batch))
        .await?
        .unwrap();
    let transaction = client.get_transaction_details(tx_hash).await?.unwrap();
    let committed = stage >= 1 && batch <= 3;
    let proven = stage >= 2 && batch <= 3;
    let accepted = stage == 3 && batch <= 2;
    let execute_hash = if accepted {
        Some(H256::repeat_byte(0x11))
    } else if external {
        None
    } else {
        Some(H256::zero())
    };
    for base in [&batch_details.base, &block_details.base] {
        assert_eq!(base.commit_tx_hash, committed.then(|| stage_hash(batch, 1)));
        assert_eq!(base.committed_at, committed.then(|| stage_time(batch, 1)));
        assert_eq!(base.prove_tx_hash, proven.then(|| stage_hash(batch, 2)));
        assert_eq!(base.proven_at, proven.then(|| stage_time(batch, 2)));
        assert_eq!(base.execute_tx_hash, execute_hash);
        assert_eq!(base.executed_at, accepted.then(|| stage_time(batch, 3)));
        assert_eq!(
            base.execute_chain_id,
            (external && accepted).then_some(SLChainId(9))
        );
        assert_matches!(
            (&base.status, accepted),
            (api::BlockStatus::Verified, true) | (api::BlockStatus::Sealed, false)
        );
    }
    assert_eq!(
        transaction.commit_tx_hash,
        batch_details.base.commit_tx_hash
    );
    assert_eq!(transaction.prove_tx_hash, batch_details.base.prove_tx_hash);
    assert_eq!(transaction.execute_tx_hash, execute_hash);
    assert_matches!(
        (transaction.status, accepted),
        (api::TransactionStatus::Verified, true) | (api::TransactionStatus::Included, false)
    );
    let verdict = if accepted {
        Some(true)
    } else if !external && stage == 3 && batch == 3 {
        Some(false)
    } else {
        None
    };
    assert_eq!(batch_details.via_is_finalized, verdict);
    Ok(())
}

struct ViaSettlementSyncTest;

#[async_trait]
impl HttpTest for ViaSettlementSyncTest {
    async fn test(
        &self,
        main_client: &DynClient<L2>,
        main_pool: &ConnectionPool<Core>,
    ) -> anyhow::Result<()> {
        let en_pool = ConnectionPool::<Core>::test_pool().await;
        let mut en_storage = en_pool.connection().await?;
        StorageInitialization::genesis()
            .prepare_storage(&NetworkConfig::for_tests(), &mut en_storage)
            .await?;
        let mut main_storage = main_pool.connection().await?;
        let transactions: Vec<_> = (0..4).map(|_| create_l2_transaction(10, 200)).collect();
        let mut failed = execute_l2_transaction(create_l2_transaction(10, 200));
        failed.execution_status = TxExecutionStatus::Failure;
        for (index, tx) in transactions.iter().enumerate() {
            let number = index as u32 + 1;
            let mut results = vec![execute_l2_transaction(tx.clone())];
            if number == 1 {
                results.push(failed.clone());
            }
            for storage in [&mut main_storage, &mut en_storage] {
                store_l2_block(storage, L2BlockNumber(number), &results).await?;
                seal_l1_batch(storage, L1BatchNumber(number)).await?;
            }
        }
        let pending = create_l2_transaction(10, 200);
        let mut pending_header = create_l2_block(5);
        pending_header.base_system_contracts_hashes.evm_emulator = Some(H256::repeat_byte(0xee));
        for storage in [&mut main_storage, &mut en_storage] {
            storage
                .transactions_dal()
                .insert_transaction_l2(
                    &pending,
                    TransactionExecutionMetrics::default(),
                    ValidationTraces::default(),
                )
                .await?;
            store_custom_l2_block(storage, &pending_header, &[]).await?;
        }

        // Main-node detail reads remain Bitcoin-backed even when an ETH mirror is present.
        for (stage, action) in [
            AggregatedActionType::Commit,
            AggregatedActionType::PublishProofOnchain,
            AggregatedActionType::Execute,
        ]
        .into_iter()
        .enumerate()
        {
            main_storage
                .eth_sender_dal()
                .insert_bogus_confirmed_eth_tx(
                    L1BatchNumber(1),
                    action,
                    H256::repeat_byte(0xa0 + stage as u8),
                    stage_time(1, stage as u32 + 1),
                    None,
                )
                .await?;
        }

        let mut config = InternalApiConfig::new(
            &Web3JsonRpcConfig::for_tests(),
            &ContractsConfig::for_tests(),
            &GenesisConfig::for_tests(),
            Some(bitcoin::Network::Regtest),
            false,
        );
        config.use_synced_settlement = true;
        let (stop_sender, stop_receiver) = watch::channel(false);
        let mut server = TestServerBuilder::new(en_pool.clone(), config)
            .build_http(stop_receiver)
            .await;
        let address = server.wait_until_ready().await;
        let en_client = Client::http(format!("http://{address}/").parse()?)?.build();

        for stage in 0..=3 {
            if stage == 1 || stage == 2 {
                for batch in 1..=3 {
                    confirm_inscription(&mut main_storage, batch, stage).await?;
                }
            } else if stage == 3 {
                for batch in 1..=3 {
                    let mut reveal_txid = stage_hash(batch, 2).as_bytes().to_vec();
                    reveal_txid.reverse();
                    main_storage
                        .via_votes_dal()
                        .insert_vote(batch, &reveal_txid, "verifier", batch != 3)
                        .await?;
                    assert!(
                        main_storage
                            .via_votes_dal()
                            .finalize_transaction_if_needed(batch, 1.0, 1)
                            .await?
                    );
                    sqlx::query("UPDATE via_l1_batch_inscription_request SET updated_at = $1 WHERE l1_batch_number = $2")
                        .bind(stage_time(batch, 3).naive_utc())
                        .bind(i64::from(batch))
                        .execute(main_storage.conn())
                        .await?;
                }
            }
            if stage > 0 {
                synchronize_stage(main_client, &en_pool, stage).await?;
            }
            if stage == 3 {
                sqlx::query("UPDATE eth_txs SET chain_id = 9 WHERE id IN (SELECT eth_execute_tx_id FROM l1_batches)")
                    .execute(en_storage.conn())
                    .await?;
            }
            for (index, tx) in transactions.iter().enumerate() {
                let batch = index as u32 + 1;
                assert_details(main_client, batch, tx.hash(), stage, false).await?;
                assert_details(&en_client, batch, tx.hash(), stage, true).await?;
            }
        }
        for client in [main_client, &en_client as &DynClient<L2>] {
            assert_matches!(
                client
                    .get_transaction_details(failed.hash)
                    .await?
                    .unwrap()
                    .status,
                api::TransactionStatus::Failed
            );
            assert_matches!(
                client
                    .get_transaction_details(pending.hash())
                    .await?
                    .unwrap()
                    .status,
                api::TransactionStatus::Pending
            );
            let pending_block = client.get_block_details(L2BlockNumber(5)).await?.unwrap();
            assert_eq!(pending_block.l1_batch_number, L1BatchNumber(5));
            assert_matches!(pending_block.base.status, api::BlockStatus::Sealed);
            assert_eq!(
                pending_block.base.base_system_contracts_hashes.evm_emulator,
                Some(H256::repeat_byte(0xee))
            );
            assert!(pending_block.base.commit_tx_hash.is_none());
            assert!(client.get_block_details(L2BlockNumber(99)).await?.is_none());
            assert!(client
                .get_transaction_details(H256::repeat_byte(0xff))
                .await?
                .is_none());
            assert_matches!(
                client
                    .get_l1_batch_details(L1BatchNumber(0))
                    .await?
                    .unwrap()
                    .base
                    .status,
                api::BlockStatus::Verified
            );
        }

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM via_l1_batch_inscription_request WHERE l1_batch_number > 0",
        )
        .fetch_one(en_storage.conn())
        .await?;
        assert_eq!(count, 0);
        let shared_time: chrono::NaiveDateTime =
            sqlx::query_scalar("SELECT confirmed_at FROM eth_txs_history WHERE tx_hash = $1")
                .bind(format!("{:#x}", H256::repeat_byte(0x11)))
                .fetch_one(en_storage.conn())
                .await?;
        assert_eq!(shared_time, stage_time(1, 3).naive_utc());

        // An execution link without a synchronized acceptance timestamp is inconclusive, including legacy links.
        let execution_id: i32 =
            sqlx::query_scalar("SELECT eth_execute_tx_id FROM l1_batches WHERE number = 2")
                .fetch_one(en_storage.conn())
                .await?;
        sqlx::query("UPDATE l1_batches SET eth_execute_tx_id = NULL WHERE number = 2")
            .execute(en_storage.conn())
            .await?;
        en_storage
            .blocks_dal()
            .set_eth_tx_id(
                L1BatchNumber(2)..=L1BatchNumber(2),
                execution_id as u32,
                AggregatedActionType::Execute,
            )
            .await?;
        assert_details(&en_client, 2, transactions[1].hash(), 2, true).await?;
        assert_details(&en_client, 1, transactions[0].hash(), 3, true).await?;

        en_storage
            .eth_sender_dal()
            .delete_eth_txs(L1BatchNumber(1))
            .await?;
        assert_details(&en_client, 1, transactions[0].hash(), 3, true).await?;
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM eth_txs")
            .fetch_one(en_storage.conn())
            .await?;
        assert_eq!(remaining, 3);

        sqlx::query("UPDATE transactions SET data = '{}'::jsonb WHERE hash = $1")
            .bind(transactions[1].hash().as_bytes())
            .execute(en_storage.conn())
            .await?;
        assert!(en_client
            .get_transaction_details(transactions[1].hash())
            .await?
            .is_none());

        stop_sender.send_replace(true);
        server.shutdown().await;
        Ok(())
    }
}

#[tokio::test]
async fn via_settlement_status_sync_to_all_detail_apis() {
    test_http_server(ViaSettlementSyncTest).await;
}
