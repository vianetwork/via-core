mod system_wallets;
mod verifier;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rotated_wallet_stops_reconciliation_before_bitcoin_rpc() -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use via_btc_client::{client::BitcoinClient, indexer::BitcoinInscriptionIndexer, types::NodeAuth};
    use via_test_utils::utils::test_wallets;
    use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
    use zksync_config::{configs::via_btc_client::ViaBtcClientConfig, ViaBtcWatchConfig};
    use zksync_types::via_wallet::SystemWalletsDetails;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let client = Arc::new(BitcoinClient::new(
        &format!("http://{}", listener.local_addr()?),
        NodeAuth::None,
        ViaBtcClientConfig {
            network: "regtest".into(),
            external_apis: vec![],
            fee_strategies: vec![],
            use_rpc_for_fee_rate: Some(true),
        },
    )?);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        count.fetch_add(1, Ordering::SeqCst);
        let mut buffer = [0; 4096];
        stream.read(&mut buffer).await.unwrap();
        stream
            .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    for initialized in [false, true] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await?;
        let mut wallets = test_wallets();
        let original = wallets.bridge.clone();
        if initialized {
            storage.via_withdrawal_dal().verify_withdrawal_wallet(original.script_pubkey().as_bytes()).await?;
        }
        wallets.bridge = wallets.governance.clone();
        let fulfillment_wallet = if initialized { wallets.bridge.clone() } else { original };
        storage.via_wallet_dal().insert_wallets(&SystemWalletsDetails::try_from(wallets.clone())?, 1).await?;
        storage.via_indexer_dal().init_indexer_metadata("via_btc_watch", 1).await?;
        storage.via_l1_block_dal().insert_l1_block(1, "11".repeat(32)).await?;
        let indexer = BitcoinInscriptionIndexer::new(client.clone(), Arc::new(wallets));
        let mut watcher =
            crate::VerifierBtcWatch::new(ViaBtcWatchConfig::for_tests(), indexer, client.clone(), pool.clone())
                .await?
                .with_withdrawal_fulfillment(
                    via_musig2::types::TransactionBuilderConfig::withdrawal(fulfillment_wallet),
                    1,
                )?;
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(2), watcher.loop_iteration(&mut storage)).await;
        assert!(result?.is_err(), "a rotated wallet must stop the watcher");
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        assert_eq!(storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?, 1);
    }
    server.abort();
    Ok(())
}
