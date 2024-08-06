use async_trait::async_trait;
use bitcoin::{Address, Block, OutPoint, Transaction, Txid};
use bitcoincore_rpc::{
    bitcoincore_rpc_json::EstimateMode,
    json::{EstimateSmartFeeResult, ScanTxOutRequest},
    Auth, Client, RpcApi,
};

use crate::{traits::BitcoinRpc, types::BitcoinRpcResult};

pub struct BitcoinRpcClient {
    client: Client,
}

#[allow(unused)]
impl BitcoinRpcClient {
    pub fn new(
        url: &str,
        rpc_user: &str,
        rpc_password: &str,
    ) -> Result<Self, bitcoincore_rpc::Error> {
        let auth = Auth::UserPass(rpc_user.to_string(), rpc_password.to_string());
        let client = Client::new(url, auth)?;
        Ok(Self { client })
    }
}

#[allow(unused)]
#[async_trait]
impl BitcoinRpc for BitcoinRpcClient {
    async fn get_balance(&self, address: &Address) -> BitcoinRpcResult<u64> {
        let descriptor = format!("addr({})", address);
        let request = vec![ScanTxOutRequest::Single(descriptor)];

        let result = self.client.scan_tx_out_set_blocking(&request)?;

        Ok(result.total_amount.to_sat())
    }

    async fn send_raw_transaction(&self, tx_hex: &str) -> BitcoinRpcResult<Txid> {
        self.client
            .send_raw_transaction(tx_hex)
            .map_err(|e| e.into())
    }

    async fn list_unspent(&self, address: &Address) -> BitcoinRpcResult<Vec<OutPoint>> {
        let descriptor = format!("addr({})", address);
        let request = vec![ScanTxOutRequest::Single(descriptor)];

        let result = self.client.scan_tx_out_set_blocking(&request)?;

        let unspent: Vec<OutPoint> = result
            .unspents
            .into_iter()
            .map(|unspent| OutPoint {
                txid: unspent.txid,
                vout: unspent.vout,
            })
            .collect();

        Ok(unspent)
    }

    async fn get_transaction(&self, txid: &Txid) -> BitcoinRpcResult<Transaction> {
        self.client
            .get_raw_transaction(txid, None)
            .map_err(|e| e.into())
    }

    async fn get_block_count(&self) -> BitcoinRpcResult<u64> {
        self.client.get_block_count().map_err(|e| e.into())
    }

    async fn get_block(&self, block_height: u128) -> BitcoinRpcResult<Block> {
        let block_hash = self.client.get_block_hash(block_height as u64)?;
        self.client.get_block(&block_hash).map_err(|e| e.into())
    }

    async fn get_best_block_hash(&self) -> BitcoinRpcResult<bitcoin::BlockHash> {
        self.client.get_best_block_hash().map_err(|e| e.into())
    }

    async fn get_raw_transaction_info(
        &self,
        txid: &Txid,
    ) -> BitcoinRpcResult<bitcoincore_rpc::json::GetRawTransactionResult> {
        self.client
            .get_raw_transaction_info(txid, None)
            .map_err(|e| e.into())
    }

    async fn estimate_smart_fee(
        &self,
        conf_target: u16,
        estimate_mode: Option<EstimateMode>,
    ) -> BitcoinRpcResult<EstimateSmartFeeResult> {
        self.client
            .estimate_smart_fee(conf_target, estimate_mode)
            .map_err(|e| e.into())
    }
}

#[cfg(test)]
mod tests {}
