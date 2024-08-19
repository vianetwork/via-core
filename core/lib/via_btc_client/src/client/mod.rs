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
