use std::{fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use bitcoin::{Address, Block, BlockHash, OutPoint, Transaction, Txid};
use bitcoincore_rpc::{
    bitcoincore_rpc_json::EstimateMode,
    json::{
        EstimateSmartFeeResult, GetBlockStatsResult, GetBlockchainInfoResult, GetMempoolInfoResult,
        ScanTxOutRequest,
    },
    jsonrpc::{self, client::Transport, minreq_http},
    Client, RpcApi,
};
use tokio::sync::Semaphore;
use tracing::{debug, instrument};

use crate::{
    traits::BitcoinRpc,
    metrics::{RpcCallLabels, RpcMethodLabel, METRICS},
    types::{BitcoinError, BitcoinRpcResult, NodeAuth},
};

const RPC_MAX_RETRIES: u8 = 3;
const RPC_RETRY_DELAY_MS: u64 = 500;

const RPC_CONCURRENCY: usize = 10;
const RPC_TIMEOUT: Duration = Duration::from_secs(15);
const SCAN_TIMEOUT: Duration = Duration::from_secs(300);

#[cfg(test)]
mod tests;

struct TimedTransport {
    inner: minreq_http::MinreqHttpTransport,
    component: &'static str,
}

impl Transport for TimedTransport {
    fn send_request(
        &self,
        request: jsonrpc::Request<'_>,
    ) -> Result<jsonrpc::Response, jsonrpc::Error> {
        let labels = RpcCallLabels { method: request.method.into(), component: self.component };
        let latency = METRICS.rpc_duration[&labels].start();
        let result = self.inner.send_request(request);
        latency.observe();
        result
    }

    fn send_batch(
        &self,
        requests: &[jsonrpc::Request<'_>],
    ) -> Result<Vec<jsonrpc::Response>, jsonrpc::Error> {
        self.inner.send_batch(requests)
    }

    fn fmt_target(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt_target(f)
    }
}

#[derive(Clone)]
pub struct BitcoinRpcClient {
    client: Arc<Client>,
    scan_client: Arc<Client>,
    // One scan slot shared by clones. Separate clients and processes are not covered.
    // Bitcoin Core rejects second scantxoutset while one runs: https://github.com/bitcoin/bitcoin/blob/cce32b16874e85e8a00cfa1a2190f0b6f4dcaf66/src/rpc/blockchain.cpp#L2437-L2440
    scan_permits: Arc<Semaphore>,
    permits: Arc<Semaphore>,
    component: &'static str,
}

impl fmt::Debug for BitcoinRpcClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BitcoinRpcClient")
            .field("component", &self.component)
            .finish_non_exhaustive()
    }
}

impl BitcoinRpcClient {
    #[instrument(skip(url, auth), target = "bitcoin_client::rpc_client")]
    pub fn new(
        url: &str,
        auth: NodeAuth,
        component: &'static str,
    ) -> Result<Self, bitcoincore_rpc::Error> {
        Self::with_timeouts(url, auth, component, RPC_TIMEOUT, SCAN_TIMEOUT)
    }

    fn with_timeouts(
        url: &str,
        auth: NodeAuth,
        component: &'static str,
        timeout: Duration,
        scan_timeout: Duration,
    ) -> Result<Self, bitcoincore_rpc::Error> {
        let (user, pass) = auth.get_user_pass()?;
        let make_client = |timeout| {
            let builder = minreq_http::Builder::new()
                .url(url)
                .map_err(|err| bitcoincore_rpc::Error::JsonRpc(err.into()))?
                .timeout(timeout);
            let builder = match user.clone() {
                Some(user) => builder.basic_auth(user, pass.clone()),
                None => builder,
            };
            Ok::<_, bitcoincore_rpc::Error>(Arc::new(Client::from_jsonrpc(
                jsonrpc::Client::with_transport(TimedTransport {
                    inner: builder.build(),
                    component,
                }),
            )))
        };
        Ok(Self {
            client: make_client(timeout)?,
            scan_client: make_client(scan_timeout)?,
            scan_permits: Arc::new(Semaphore::new(1)),
            permits: Arc::new(Semaphore::new(RPC_CONCURRENCY)),
            component,
        })
    }

    async fn retry_rpc<F, T>(&self, method: &'static str, f: F) -> BitcoinRpcResult<T>
    where
        F: Fn(&Client) -> BitcoinRpcResult<T> + Send + Sync + 'static,
        T: Send + 'static,
    {
        let f = Arc::new(f);
        let scan = method == "scantxoutset";
        let mut attempt = 0;
        loop {
            let scan_permit = if scan {
                Some(
                    self.scan_permits
                        .clone()
                        .acquire_owned()
                        .await
                        .expect("scan semaphore is never closed"),
                )
            } else {
                None
            };
            let permit =
                self.permits.clone().acquire_owned().await.expect("RPC semaphore is never closed");
            let client = if scan { self.scan_client.clone() } else { self.client.clone() };
            let f = f.clone();
            let span = tracing::Span::current();
            // Blocking RPC runs off async workers with caller's span, as deadpool-sync does: https://github.com/deadpool-rs/deadpool/blob/f4efd12e65b2cc9347f6ca0ee42800cd376ea658/crates/deadpool-sync/src/lib.rs#L123-L140
            let result = tokio::task::spawn_blocking(move || {
                // Cancellation must not admit another call until this attempt exits.
                let _scan_permit = scan_permit;
                let _permit = permit;
                let _span = span.enter();
                f(&client)
            })
            .await
            .map_err(|err| {
                BitcoinError::Rpc(format!("RPC {method} blocking task failed: {err}"))
            })?;

            match result {
                Ok(value) => return Ok(value),
                Err(err) if attempt < RPC_MAX_RETRIES => {
                    tracing::warn!(error = ?err, retries = attempt, method, "RPC call failed, retrying");
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(RPC_RETRY_DELAY_MS)).await;
                }
                Err(err) => {
                    METRICS.rpc_max_retries_exceeded[&RpcMethodLabel { method: "RPC call".into() }]
                        .inc();
                    return Err(err);
                }
            }
        }
    }
}

#[async_trait]
impl BitcoinRpc for BitcoinRpcClient {
    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_balance(&self, address: &Address) -> BitcoinRpcResult<u64> {
        let address = address.clone();
        self.retry_rpc("listunspent", move |client| {
            debug!("Getting balance");
            let result = client.list_unspent(
                Some(1),          // minconf
                None,             // maxconf
                Some(&[&address]), // addresses
                None,             // include_unsafe
                None,             // query_options
            )?;

            let total_amount: u64 = result
                .into_iter()
                .map(|unspent| unspent.amount.to_sat())
                .sum();

            Ok(total_amount)
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_balance_scan(&self, address: &Address) -> BitcoinRpcResult<u64> {
        debug!("Getting balance by scanning");
        let result = self.list_unspent(address).await?;
        let mut sum = 0u64;
        for unspent in &result {
            sum += self.get_transaction(&unspent.txid).await?.output[unspent.vout as usize]
                .value
                .to_sat();
        }

        Ok(sum)
    }

    #[instrument(skip(self, tx_hex), target = "bitcoin_client::rpc_client")]
    async fn send_raw_transaction(&self, tx_hex: &str) -> BitcoinRpcResult<Txid> {
        let tx_hex = tx_hex.to_owned();
        self.retry_rpc("sendrawtransaction", move |client| {
            debug!("Sending raw transaction");
            client
                .send_raw_transaction(tx_hex.as_str())
                .map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn list_unspent_based_on_node_wallet(
        &self,
        address: &Address,
    ) -> BitcoinRpcResult<Vec<OutPoint>> {
        let address = address.clone();
        self.retry_rpc("listunspent", move |client| {
            debug!("Listing unspent outputs based on node wallet");
            let result = client.list_unspent(
                Some(1),          // minconf
                None,             // maxconf
                Some(&[&address]), // addresses
                None,             // include_unsafe
                None,
            )?;

            let unspent: Vec<OutPoint> = result
                .into_iter()
                .map(|unspent| OutPoint {
                    txid: unspent.txid,
                    vout: unspent.vout,
                })
                .collect();

            Ok(unspent)
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn list_unspent(&self, address: &Address) -> BitcoinRpcResult<Vec<OutPoint>> {
        let address = address.clone();
        self.retry_rpc("scantxoutset", move |client| {
            debug!("Listing unspent outputs");
            let descriptor = format!("addr({})", address);
            let request = vec![ScanTxOutRequest::Single(descriptor)];
            let result = client.scan_tx_out_set_blocking(&request)?;
            let unspent = result
                .unspents
                .into_iter()
                .map(|unspent| OutPoint {
                    txid: unspent.txid,
                    vout: unspent.vout,
                })
                .collect();
            Ok(unspent)
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_transaction(&self, txid: &Txid) -> BitcoinRpcResult<Transaction> {
        let txid = *txid;
        self.retry_rpc("getrawtransaction", move |client| {
            debug!("Getting transaction");
            client
                .get_raw_transaction(&txid, None)
                .map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_block_count(&self) -> BitcoinRpcResult<u64> {
        self.retry_rpc("getblockcount", move |client| {
            debug!("Getting block count");
            client.get_block_count().map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_block_hash(&self, height: u64) -> BitcoinRpcResult<BlockHash> {
        self.retry_rpc("getblockhash", move |client| {
            client.get_block_hash(height).map_err(Into::into)
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_block_by_height(&self, block_height: u128) -> BitcoinRpcResult<Block> {
        self.retry_rpc("getblock", move |client| {
            let hash = client.get_block_hash(block_height as u64)?;
            client.get_block(&hash).map_err(Into::into)
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinRpcResult<Block> {
        let block_hash = *block_hash;
        self.retry_rpc("getblock", move |client| {
            debug!("Getting block by hash");
            client.get_block(&block_hash).map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_best_block_hash(&self) -> BitcoinRpcResult<BlockHash> {
        self.retry_rpc("getbestblockhash", move |client| {
            debug!("Getting best block hash");
            client.get_best_block_hash().map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_raw_transaction_info(
        &self,
        txid: &Txid,
    ) -> BitcoinRpcResult<bitcoincore_rpc::json::GetRawTransactionResult> {
        let txid = *txid;
        self.retry_rpc("getrawtransaction", move |client| {
            debug!("Getting raw transaction info");
            client
                .get_raw_transaction_info(&txid, None)
                .map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn estimate_smart_fee(
        &self,
        conf_target: u16,
        estimate_mode: Option<EstimateMode>,
    ) -> BitcoinRpcResult<EstimateSmartFeeResult> {
        self.retry_rpc("estimatesmartfee", move |client| {
            debug!("Estimating smart fee");
            client
                .estimate_smart_fee(conf_target, estimate_mode)
                .map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_blockchain_info(&self) -> BitcoinRpcResult<GetBlockchainInfoResult> {
        self.retry_rpc("getblockchaininfo", move |client| {
            debug!("Getting blockchain info");
            client.get_blockchain_info().map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_block_stats(&self, height: u64) -> BitcoinRpcResult<GetBlockStatsResult> {
        self.retry_rpc("getblockstats", move |client| {
            debug!("Getting block stats");
            client.get_block_stats(height).map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_mempool_info(&self) -> BitcoinRpcResult<GetMempoolInfoResult> {
        self.retry_rpc("getmempoolinfo", move |client| {
            debug!("Getting mempool info");
            client.get_mempool_info().map_err(|e| e.into())
        })
        .await
    }
}
