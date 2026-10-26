use bitcoin::hashes::Hash;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::{JoinHandle, JoinSet},
    time::timeout,
};

use super::*;

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

#[tokio::test]
async fn checked_header_distinguishes_orphan_unknown_and_malformed_source() {
    use crate::types::{HeaderMembership, HeaderValidationError};

    let mut server = RpcServer::new().await;
    let rpc = BitcoinRpcClient::new(&server.url, NodeAuth::None, "header_test").unwrap();
    let hash = BlockHash::from_byte_array([7; 32]);
    let parent = BlockHash::from_byte_array([6; 32]);
    let header = json!({
        "hash":hash,"height":7,"confirmations":2,"version":1,"merkleroot":"00".repeat(32),
        "time":1,"nonce":0,"bits":"207fffff","difficulty":1.0,"chainwork":"00",
        "nTx":1,"previousblockhash":parent,
    });
    for (field, value, expected) in [
        ("confirmations", json!(2), None),
        ("confirmations", json!(-1), None),
        ("hash", json!(parent), Some(HeaderValidationError::HashMismatch)),
        ("confirmations", json!(0), Some(HeaderValidationError::InvalidConfirmations(0))),
        ("previousblockhash", Value::Null, Some(HeaderValidationError::ParentMismatch)),
        ("unknown", Value::Null, None),
    ] {
        let rpc = rpc.clone();
        let mut task = tokio::spawn(async move { rpc.get_block_header(&hash).await });
        let mut response = header.clone();
        response[field] = value.clone();
        let error = if field == "unknown" {
            response = Value::Null;
            json!({"code":-5,"message":"Block not found"})
        } else {
            Value::Null
        };
        let result = loop {
            tokio::select! {
                result = &mut task => break result.unwrap(),
                request = server.next() => respond(request, response.clone(), error.clone()).await,
            }
        };
        if field == "unknown" {
            assert_eq!(result.unwrap(), None);
        } else if let Some(expected) = expected {
            assert!(
                matches!(result, Err(BitcoinError::InvalidBlockHeader { reason, .. }) if reason == expected)
            );
        } else {
            let result = result.unwrap().unwrap();
            assert_eq!((result.hash, result.height, result.parent_hash), (hash, 7, parent));
            assert_eq!(matches!(result.membership, HeaderMembership::Noncanonical), value == -1);
        }
    }
}
