use std::{str::FromStr, time::Instant};

use bitcoin::{hashes::Hash, Network};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::{JoinHandle, JoinSet},
    time::{sleep, timeout},
};
use vise::{Format, MetricsCollection};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;
use zksync_health_check::{AppHealthCheck, HealthStatus, ReactiveHealthCheck};

use super::*;
use crate::client::BitcoinClient;

type Request = (TcpStream, String, Value);

struct RpcServer {
    url: String,
    requests: mpsc::UnboundedReceiver<Request>,
    task: JoinHandle<()>,
}

impl RpcServer {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/wallet/fixture", listener.local_addr().unwrap());
        let (sender, requests) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut readers = JoinSet::new();
            loop {
                tokio::select! {
                    stream = listener.accept() => {
                        let sender = sender.clone();
                        readers.spawn(async move {
                            let mut reader = BufReader::new(stream.unwrap().0);
                            let mut headers = String::new();
                            let mut length = 0;
                            loop {
                                let mut line = String::new();
                                assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
                                if line == "\r\n" {
                                    break;
                                }
                                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                                    length = value.trim().parse().unwrap();
                                }
                                headers.push_str(&line);
                            }
                            let mut body = vec![0; length];
                            reader.read_exact(&mut body).await.unwrap();
                            let _ = sender.send((reader.into_inner(), headers, serde_json::from_slice(&body).unwrap()));
                        });
                    }
                    result = readers.join_next(), if !readers.is_empty() => { result.unwrap().unwrap(); }
                }
            }
        });
        Self { url, requests, task }
    }

    async fn next(&mut self) -> Request {
        timeout(Duration::from_secs(3), self.requests.recv()).await.unwrap().unwrap()
    }
}

impl Drop for RpcServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn respond((mut stream, _, request): Request, result: Value, error: Value) {
    let body = json!({"result": result, "error": error, "id": request["id"]}).to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}

fn exported_metrics() -> String {
    let registry = MetricsCollection::lazy().collect();
    let mut text = String::new();
    registry.encode(&mut text, Format::Prometheus).unwrap();
    text
}

#[tokio::test]
async fn hash_window_runs_in_parallel_while_health_and_metrics_stay_responsive() {
    let mut server = RpcServer::new().await;
    let client = BitcoinClient::new_with_component(
        &server.url,
        NodeAuth::None,
        ViaBtcClientConfig::for_tests(),
        "window_test",
    )
    .unwrap();
    let started = Instant::now();
    let fetch = tokio::spawn(async move { client.fetch_block_window(100, 101, 2).await });
    let first = server.next().await;
    let second = server.next().await;
    for (_, headers, request) in [&first, &second] {
        assert!(headers.starts_with("POST /wallet/fixture "));
        assert_eq!(request["method"], "getblockhash");
    }
    let health = AppHealthCheck::default();
    let (component, updater) = ReactiveHealthCheck::new("rpc_fixture");
    updater.update(HealthStatus::Ready.into());
    health.insert_component(component).unwrap();
    sleep(Duration::from_millis(50)).await;
    assert!(health.check_health().await.is_healthy());
    assert!(exported_metrics().contains("via_btc_client_rpc_duration_seconds"));
    assert!(!fetch.is_finished());
    assert!(started.elapsed() < Duration::from_secs(2));

    // Later request completes first. Public result stays height ordered.
    let hash = |height: u8| BlockHash::from_byte_array([height; 32]);
    let second_height = second.2["params"][0].as_u64().unwrap() as u8;
    respond(second, json!(hash(second_height)), Value::Null).await;
    let first_height = first.2["params"][0].as_u64().unwrap() as u8;
    respond(first, json!(hash(first_height)), Value::Null).await;
    assert_eq!(fetch.await.unwrap().unwrap(), vec![(100, hash(100)), (101, hash(101))]);
    let metrics = exported_metrics();
    assert!(metrics.contains("via_btc_client_rpc_duration_seconds_count{method=\"getblockhash\",component=\"window_test\"} 2"));
    for bucket in ["0.001", "30.0"] {
        assert!(metrics.lines().any(|line| {
            line.starts_with("via_btc_client_rpc_duration_seconds_bucket{")
                && line.contains(&format!("le=\"{bucket}\""))
        }));
    }
}

#[tokio::test]
async fn full_block_retry_rechecks_height_and_times_each_rpc_method() {
    let mut server = RpcServer::new().await;
    let client = BitcoinRpcClient::new(&server.url, NodeAuth::None, "full_block_test").unwrap();
    let call = tokio::spawn(async move { client.get_block_by_height(0).await });
    let mut block = bitcoin::blockdata::constants::genesis_block(Network::Regtest);
    for attempt in 0..2 {
        let request = server.next().await;
        assert_eq!(request.2["method"], "getblockhash");
        assert_eq!(request.2["params"][0], 0);
        block.header.nonce = attempt;
        respond(request, json!(block.block_hash()), Value::Null).await;
        let request = server.next().await;
        assert_eq!(request.2["method"], "getblock");
        assert_eq!(request.2["params"][0], json!(block.block_hash()));
        if attempt == 0 {
            respond(request, Value::Null, json!({"code": -28, "message": "fixture retry"})).await;
        } else {
            respond(request, json!(bitcoin::consensus::encode::serialize_hex(&block)), Value::Null)
                .await;
        }
    }
    assert_eq!(call.await.unwrap().unwrap(), block);
    let metrics = exported_metrics();
    for method in ["getblockhash", "getblock"] {
        assert!(metrics.contains(&format!("via_btc_client_rpc_duration_seconds_count{{method=\"{method}\",component=\"full_block_test\"}} 2")));
    }
}

#[tokio::test]
async fn cancellation_keeps_running_calls_bounded_across_clones() {
    let mut server = RpcServer::new().await;
    let client = BitcoinRpcClient::new(&server.url, NodeAuth::None, "cancellation_test").unwrap();
    let mut calls = Vec::new();
    for height in 0..10 {
        let client = client.clone();
        calls.push(tokio::spawn(async move { client.get_block_hash(height).await }));
    }
    let mut pending = Vec::new();
    for _ in 0..10 {
        pending.push(server.next().await);
    }
    calls[0].abort();
    assert!(calls.remove(0).await.unwrap_err().is_cancelled());
    let extra = {
        let client = client.clone();
        tokio::spawn(async move { client.get_block_hash(10).await })
    };
    let cancelled_waiter = tokio::spawn(async move { client.get_best_block_hash().await });
    assert!(timeout(Duration::from_millis(100), server.requests.recv()).await.is_err());
    cancelled_waiter.abort();
    assert!(cancelled_waiter.await.unwrap_err().is_cancelled());
    let cancelled = pending.iter().position(|(_, _, request)| request["params"][0] == 0).unwrap();
    respond(pending.remove(cancelled), json!(BlockHash::all_zeros()), Value::Null).await;
    let admitted = server.next().await;
    assert_eq!(admitted.2["method"], "getblockhash");
    assert_eq!(admitted.2["params"][0], 10);
    pending.push(admitted);
    for request in pending {
        respond(request, json!(BlockHash::all_zeros()), Value::Null).await;
    }
    for call in calls {
        assert_eq!(call.await.unwrap().unwrap(), BlockHash::all_zeros());
    }
    assert_eq!(extra.await.unwrap().unwrap(), BlockHash::all_zeros());
    assert!(timeout(Duration::from_millis(50), server.requests.recv()).await.is_err());
    assert!(exported_metrics().contains("via_btc_client_rpc_duration_seconds_count{method=\"getblockhash\",component=\"cancellation_test\"} 11"));
}

#[tokio::test]
async fn slow_stream_hits_whole_request_deadline_and_retries_four_times() {
    let mut server = RpcServer::new().await;
    let client = BitcoinRpcClient::with_timeouts(
        &server.url,
        NodeAuth::None,
        "deadline_test",
        Duration::from_secs(1),
        Duration::from_secs(3),
    )
    .unwrap();
    let serve = tokio::spawn(async move {
        let mut streams = JoinSet::new();
        for _ in 0..4 {
            let (mut stream, _, request) = server.next().await;
            assert_eq!(request["method"], "getblockcount");
            streams.spawn(async move {
                let body = json!({"result": 7, "error": null, "id": request["id"]}).to_string();
                let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                stream.write_all(header.as_bytes()).await.unwrap();
                for byte in body.bytes() {
                    if stream.write_all(&[byte]).await.is_err() {
                        break;
                    }
                    sleep(Duration::from_millis(100)).await;
                }
            });
        }
        while let Some(result) = streams.join_next().await {
            result.unwrap();
        }
    });
    let started = Instant::now();
    let call = tokio::spawn(async move { client.get_block_count().await });
    sleep(Duration::from_millis(100)).await;
    assert!(AppHealthCheck::default().check_health().await.is_healthy());
    assert!(exported_metrics().contains("via_btc_client_rpc_duration_seconds"));
    assert!(!call.is_finished());
    assert!(started.elapsed() < Duration::from_secs(1));
    let error = timeout(Duration::from_secs(9), call).await.unwrap().unwrap().unwrap_err();
    assert!(matches!(error, BitcoinError::Rpc(_)));
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert!(started.elapsed() < Duration::from_secs(8));
    serve.await.unwrap();
    assert!(exported_metrics().contains("via_btc_client_rpc_duration_seconds_count{method=\"getblockcount\",component=\"deadline_test\"} 4"));
}

#[tokio::test]
async fn scan_has_a_longer_finite_deadline_than_ordinary_calls() {
    let mut server = RpcServer::new().await;
    let client = BitcoinRpcClient::with_timeouts(
        &server.url,
        NodeAuth::None,
        "scan_test",
        Duration::from_secs(1),
        Duration::from_secs(4),
    )
    .unwrap();
    let address = Address::from_str("bcrt1qg9ly0933msv97jpgrlqwn0h6743y0u6zrvh020")
        .unwrap()
        .require_network(Network::Regtest)
        .unwrap();
    let scan_address = address.clone();
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.list_unspent(&scan_address).await });
    let request = server.next().await;
    assert_eq!(request.2["method"], "scantxoutset");
    assert_eq!(request.2["params"][0], "start");
    let second = tokio::spawn(async move { client.list_unspent(&address).await });
    let serve = tokio::spawn(async move {
        let _ = timeout(Duration::from_secs(3), async {
            while let Some(overlap) = server.requests.recv().await {
                assert_eq!(overlap.2["method"], "scantxoutset");
                assert_eq!(overlap.2["params"][0], "start");
                respond(overlap, Value::Null, json!({"code": -8, "message": "Scan already in progress, use action abort or status"})).await;
            }
        }).await;
        let result = json!({"success": true, "txouts": 0, "height": 100,
            "bestblock": BlockHash::all_zeros(), "unspents": [], "total_amount": 0});
        respond(request, result.clone(), Value::Null).await;
        let request = server.next().await;
        assert_eq!(request.2["method"], "scantxoutset");
        respond(request, result, Value::Null).await;
    });
    assert!(first.await.unwrap().unwrap().is_empty());
    assert!(second.await.unwrap().unwrap().is_empty());
    serve.await.unwrap();
}

#[tokio::test]
async fn retries_preserve_auth_and_exact_broadcast_hex() {
    let mut server = RpcServer::new().await;
    let txid = Txid::from_byte_array([42; 32]);
    for cookie in [false, true] {
        let file = tempfile::NamedTempFile::new().unwrap();
        let auth = if cookie {
            std::fs::write(file.path(), "fixture:password").unwrap();
            NodeAuth::CookieFile(file.path().to_owned())
        } else {
            NodeAuth::UserPass("fixture".into(), "password".into())
        };
        let client = BitcoinRpcClient::new(&server.url, auth, "auth_test").unwrap();
        let call = tokio::spawn(async move { client.send_raw_transaction("00aAbBff").await });
        for attempt in 0..4 {
            let request = server.next().await;
            assert!(request.1.contains("Authorization: Basic Zml4dHVyZTpwYXNzd29yZA=="));
            assert_eq!(request.2["method"], "sendrawtransaction");
            assert_eq!(request.2["params"][0], "00aAbBff");
            if attempt < 3 {
                respond(request, Value::Null, json!({"code": -28, "message": "fixture retry"}))
                    .await;
            } else {
                respond(request, json!(txid), Value::Null).await;
            }
        }
        assert_eq!(call.await.unwrap().unwrap(), txid);
    }
}

#[tokio::test]
async fn blocking_panic_is_returned_without_retry() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let client = BitcoinRpcClient::new("http://127.0.0.1:1", NodeAuth::None, "panic_test").unwrap();
    let result: BitcoinRpcResult<()> = client
        .retry_rpc("getblockcount", move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            panic!("fixture panic");
        })
        .await;
    assert!(matches!(result, Err(BitcoinError::Rpc(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn runtime_shutdown_waits_only_for_the_admitted_attempt_deadline() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (accepted, receiver) = tokio::sync::oneshot::channel();
    let server = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        accepted.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(3));
    });
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async move {
        let client = BitcoinRpcClient::with_timeouts(
            &url,
            NodeAuth::None,
            "shutdown_test",
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap();
        let call = tokio::spawn(async move { client.get_block_count().await });
        timeout(Duration::from_secs(3), receiver).await.unwrap().unwrap();
        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
    });
    let started = Instant::now();
    drop(runtime);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(started.elapsed() >= Duration::from_millis(500));
    server.join().unwrap();
}
