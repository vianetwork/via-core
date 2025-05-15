use std::sync::Arc;

use async_trait::async_trait;
use bitcoin::{Address, Block, BlockHash, OutPoint, Transaction, Txid};
use bitcoincore_rpc::{
    bitcoincore_rpc_json::EstimateMode,
    json::{
        EstimateSmartFeeResult, GetBlockStatsResult, GetBlockchainInfoResult, GetMempoolInfoResult,
        ScanTxOutRequest,
    },
    Client, RpcApi,
};
use tracing::{debug, instrument};

use crate::{
    traits::BitcoinRpc,
    types::{BitcoinRpcResult, NodeAuth},
    utils::with_retry,
};

const RPC_MAX_RETRIES: u8 = 3;
const RPC_RETRY_DELAY_MS: u64 = 500;

#[derive(Debug)]
pub struct BitcoinRpcClient {
    client: Arc<Client>,
}

impl BitcoinRpcClient {
    #[instrument(skip(auth), target = "bitcoin_client::rpc_client")]
    pub fn new(url: &str, auth: NodeAuth) -> Result<Self, bitcoincore_rpc::Error> {
        let client = Client::new(url, auth.clone())?;
        Ok(Self {
            client: Arc::new(client),
        })
    }

    async fn retry_rpc<F, T>(f: F) -> BitcoinRpcResult<T>
    where
        F: Fn() -> BitcoinRpcResult<T> + Send + Sync,
    {
        with_retry(f, RPC_MAX_RETRIES, RPC_RETRY_DELAY_MS, "RPC call").await
    }
}

#[async_trait]
impl BitcoinRpc for BitcoinRpcClient {
    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_balance(&self, address: &Address) -> BitcoinRpcResult<u64> {
        Self::retry_rpc(|| {
            debug!("Getting balance");
            let result = self.client.list_unspent(
                Some(1),          // minconf
                None,             // maxconf
                Some(&[address]), // addresses
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
        Self::retry_rpc(|| {
            debug!("Sending raw transaction");
            self.client
                .send_raw_transaction(tx_hex)
                .map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn list_unspent_based_on_node_wallet(
        &self,
        address: &Address,
    ) -> BitcoinRpcResult<Vec<OutPoint>> {
        Self::retry_rpc(|| {
            debug!("Listing unspent outputs based on node wallet");
            let result = self.client.list_unspent(
                Some(1),          // minconf
                None,             // maxconf
                Some(&[address]), // addresses
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
        Self::retry_rpc(|| {
            debug!("Listing unspent outputs");
            let descriptor = format!("addr({})", address);
            let request = vec![ScanTxOutRequest::Single(descriptor)];
            let result = self.client.scan_tx_out_set_blocking(&request)?;
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
        Self::retry_rpc(|| {
            debug!("Getting transaction");
            self.client
                .get_raw_transaction(txid, None)
                .map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_block_count(&self) -> BitcoinRpcResult<u64> {
        Self::retry_rpc(|| {
            debug!("Getting block count");
            self.client.get_block_count().map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_block_by_height(&self, block_height: u128) -> BitcoinRpcResult<Block> {
        Self::retry_rpc(|| {
            debug!("Getting block by height");
            let block_hash = self.client.get_block_hash(block_height as u64)?;
            self.client.get_block(&block_hash).map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinRpcResult<Block> {
        Self::retry_rpc(|| {
            debug!("Getting block by hash");
            self.client.get_block(block_hash).map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_best_block_hash(&self) -> BitcoinRpcResult<BlockHash> {
        Self::retry_rpc(|| {
            debug!("Getting best block hash");
            self.client.get_best_block_hash().map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_raw_transaction_info(
        &self,
        txid: &Txid,
    ) -> BitcoinRpcResult<bitcoincore_rpc::json::GetRawTransactionResult> {
        Self::retry_rpc(|| {
            debug!("Getting raw transaction info");
            self.client
                .get_raw_transaction_info(txid, None)
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
        Self::retry_rpc(|| {
            debug!("Estimating smart fee");
            self.client
                .estimate_smart_fee(conf_target, estimate_mode)
                .map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_blockchain_info(&self) -> BitcoinRpcResult<GetBlockchainInfoResult> {
        Self::retry_rpc(|| {
            debug!("Getting blockchain info");
            self.client.get_blockchain_info().map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_block_stats(&self, height: u64) -> BitcoinRpcResult<GetBlockStatsResult> {
        Self::retry_rpc(|| {
            debug!("Getting block stats");
            self.client.get_block_stats(height).map_err(|e| e.into())
        })
        .await
    }

    #[instrument(skip(self), target = "bitcoin_client::rpc_client")]
    async fn get_mempool_info(&self) -> BitcoinRpcResult<GetMempoolInfoResult> {
        Self::retry_rpc(|| {
            debug!("Getting mempool info");
            self.client.get_mempool_info().map_err(|e| e.into())
        })
        .await
    }
}

impl Clone for BitcoinRpcClient {
    fn clone(&self) -> Self {
        Self {
            client: Arc::clone(&self.client),
        }
    }
}
