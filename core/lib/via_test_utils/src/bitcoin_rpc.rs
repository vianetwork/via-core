use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use serde_json::{json, Value};

/// A local JSON-RPC server for Bitcoin test fixtures. Each connection runs on its own thread, and the test supplies the responses.
/// Dropping the server stops it and waits for open requests. Release any test barrier first, or the drop never returns.
// Lifecycle adapted from Go's `httptest.Server`, whose `Close` also waits for open requests:
// https://github.com/golang/go/blob/6e676ab2b809d46623acb5988248d95d1eb7939c/src/net/http/httptest/server.go
pub struct BitcoinRpcServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
}

impl BitcoinRpcServer {
    /// Serves each request's result with its original JSON-RPC ID.
    /// `allow_disconnect` permits a client to cancel a response without failing the fixture.
    pub fn new(
        read_timeout: Option<Duration>,
        allow_disconnect: bool,
        handler: impl Fn(&str, Value) -> Value + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(handler);
        let server = {
            let stop = stop.clone();
            thread::spawn(move || {
                let mut handlers = Vec::new();
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let handler = handler.clone();
                    handlers.push(thread::spawn(move || {
                        let mut reader = BufReader::new(stream.unwrap());
                        reader.get_mut().set_read_timeout(read_timeout).unwrap();
                        let mut request_line = String::new();
                        if reader.read_line(&mut request_line).unwrap() == 0 {
                            return;
                        }
                        let path = request_line.split_whitespace().nth(1).unwrap().to_owned();
                        let mut length = 0;
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap() == 0 {
                                return;
                            }
                            if line == "\r\n" {
                                break;
                            }
                            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                                length = value.trim().parse::<usize>().unwrap();
                            }
                        }
                        let mut body = vec![0; length];
                        reader.read_exact(&mut body).unwrap();
                        let request: Value = serde_json::from_slice(&body).unwrap();
                        let id = request["id"].clone();
                        let result = handler(&path, request);
                        let body = json!({"result": result, "error": null, "id": id}).to_string();
                        let response = write!(reader.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                        if !allow_disconnect {
                            response.unwrap();
                        }
                    }));
                }
                for handler in handlers {
                    handler.join().unwrap();
                }
            })
        };
        Ok(Self {
            address,
            stop,
            server: Some(server),
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}/{path}", self.address)
    }
}

impl Drop for BitcoinRpcServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        self.server.take().unwrap().join().unwrap();
    }
}
