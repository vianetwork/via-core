use async_trait::async_trait;
use bitcoin::{Address, Block, BlockHash, OutPoint, Transaction, TxOut, Txid};
use bitcoincore_rpc::json::EstimateMode;

mod rpc_client;

use crate::{
    client::rpc_client::BitcoinRpcClient,
    traits::{BitcoinOps, BitcoinRpc},
    types::{Auth, BitcoinClientResult, BitcoinError, Network},
};

#[allow(unused)]
pub struct BitcoinClient {
    rpc: Box<dyn BitcoinRpc>,
    network: Network,
}

#[async_trait]
impl BitcoinOps for BitcoinClient {
    async fn new(rpc_url: &str, network: Network, auth: Auth) -> BitcoinClientResult<Self>
    where
        Self: Sized,
    {
        let rpc = Box::new(BitcoinRpcClient::new(rpc_url, auth)?);

        Ok(Self { rpc, network })
    }

    async fn get_balance(&self, address: &Address) -> BitcoinClientResult<u128> {
        let balance = self.rpc.get_balance(address).await?;
        Ok(balance as u128)
    }

    async fn broadcast_signed_transaction(
        &self,
        signed_transaction: &str,
    ) -> BitcoinClientResult<Txid> {
        let txid = self.rpc.send_raw_transaction(signed_transaction).await?;
        Ok(txid)
    }

    async fn fetch_utxos(&self, address: &Address) -> BitcoinClientResult<Vec<(OutPoint, TxOut)>> {
        let outpoints = self.rpc.list_unspent(address).await?;

        let mut utxos: Vec<(OutPoint, TxOut)> = vec![];

        for outpoint in outpoints {
            let tx = self.rpc.get_transaction(&outpoint.txid).await?;
            let txout = tx
                .output
                .get(outpoint.vout as usize)
                .ok_or(BitcoinError::InvalidOutpoint(outpoint.to_string()))?;
            utxos.push((outpoint, txout.clone()));
        }

        Ok(utxos)
    }

    async fn check_tx_confirmation(&self, txid: &Txid, conf_num: u32) -> BitcoinClientResult<bool> {
        let tx_info = self.rpc.get_raw_transaction_info(txid).await?;

        match tx_info.confirmations {
            Some(confirmations) => Ok(confirmations > conf_num),
            None => Ok(false),
        }
    }

    async fn fetch_block_height(&self) -> BitcoinClientResult<u128> {
        let height = self.rpc.get_block_count().await?;
        Ok(height as u128)
    }

    async fn fetch_block(&self, block_height: u128) -> BitcoinClientResult<Block> {
        self.rpc.get_block_by_height(block_height).await
    }

    async fn fetch_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinClientResult<Block> {
        self.rpc.get_block_by_hash(block_hash).await
    }

    async fn get_fee_rate(&self, conf_target: u16) -> BitcoinClientResult<u64> {
        let estimation = self
            .rpc
            .estimate_smart_fee(conf_target, Some(EstimateMode::Economical))
            .await?;

        match estimation.fee_rate {
            Some(fee_rate) => Ok(fee_rate.to_sat()),
            None => {
                let err = match estimation.errors {
                    Some(errors) => errors.join(", "),
                    None => "Unknown error during fee estimation".to_string(),
                };
                Err(BitcoinError::FeeEstimationFailed(err))
            }
        }
    }

    async fn get_transaction(&self, txid: &Txid) -> BitcoinClientResult<Transaction> {
        self.rpc.get_transaction(txid).await
    }

    fn get_network(&self) -> Network {
        self.network
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::Network;

    use super::*;
    use crate::{regtest::BitcoinRegtest, traits::BitcoinOps};

    #[tokio::test]
    async fn test_new() {
        let context = BitcoinRegtest::new().expect("Failed to create BitcoinRegtest");
        let client = BitcoinClient::new(&context.get_url(), Network::Regtest, Auth::None)
            .await
            .expect("Failed to create BitcoinClient");

        assert_eq!(client.network, Network::Regtest);
    }

    #[ignore]
    #[tokio::test]
    async fn test_get_balance() {
        let context = BitcoinRegtest::new().expect("Failed to create BitcoinRegtest");
        let _client = BitcoinClient::new(&context.get_url(), Network::Regtest, Auth::None)
            .await
            .expect("Failed to create BitcoinClient");

        // random test address fails
        // let balance = client
        //     .get_balance(&context.test_address)
        //     .await
        //     .expect("Failed to get balance");
        //
        // assert!(balance > 0, "Balance should be greater than 0");
    }

    #[ignore]
    #[tokio::test]
    async fn test_fetch_utxos() {
        let context = BitcoinRegtest::new().expect("Failed to create BitcoinRegtest");
        let _client = BitcoinClient::new(&context.get_url(), Network::Regtest, Auth::None)
            .await
            .expect("Failed to create BitcoinClient");

        // random test address fails
        // let utxos = client
        //     .fetch_utxos(&context.test_address)
        //     .await
        //     .expect("Failed to fetch UTXOs");

        // assert!(!utxos.is_empty(), "UTXOs should not be empty");
    }

    #[tokio::test]
    async fn test_fetch_block_height() {
        let context = BitcoinRegtest::new().expect("Failed to create BitcoinRegtest");
        let client = BitcoinClient::new(&context.get_url(), Network::Regtest, Auth::None)
            .await
            .expect("Failed to create BitcoinClient");

        let block_height = client
            .fetch_block_height()
            .await
            .expect("Failed to fetch block height");

        assert!(block_height > 0, "Block height should be greater than 0");
    }

    #[ignore]
    #[tokio::test]
    async fn test_estimate_fee() {
        let context = BitcoinRegtest::new().expect("Failed to create BitcoinRegtest");
        let client = BitcoinClient::new(&context.get_url(), Network::Regtest, Auth::None)
            .await
            .expect("Failed to create BitcoinClient");

        // error: Insufficient data or no feerate found
        let fee = client
            .get_fee_rate(6)
            .await
            .expect("Failed to estimate fee");

        assert!(fee > 0, "Estimated fee should be greater than 0");
    }
}
