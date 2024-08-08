use async_trait::async_trait;
use bitcoin::{Address, Block, TxOut, Txid};
use bitcoincore_rpc::json::EstimateMode;

use crate::{
    traits::{BitcoinOps, BitcoinRpc},
    types::BitcoinClientResult,
};

mod rpc_client;
pub use bitcoin::Network;
pub use rpc_client::{Auth, BitcoinRpcClient};

use crate::types::BitcoinError;

#[allow(unused)]
pub struct BitcoinClient {
    pub rpc: Box<dyn BitcoinRpc>,
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

    async fn fetch_utxos(&self, address: &Address) -> BitcoinClientResult<Vec<(TxOut, Txid, u32)>> {
        let outpoints = self.rpc.list_unspent(address).await?;

        let mut txouts = Vec::new();
        for outpoint in outpoints {
            let tx = self.rpc.get_transaction(&outpoint.txid).await?;
            let txout = tx
                .output
                .get(outpoint.vout as usize)
                .ok_or(BitcoinError::InvalidOutpoint(outpoint.to_string()))?;
            txouts.push((txout.clone(), outpoint.txid, outpoint.vout));
        }

        Ok(txouts)
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

    fn get_rpc_client(&self) -> &dyn BitcoinRpc {
        self.rpc.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bitcoin::{
        absolute::LockTime, transaction::Version, Address, Amount, Block, Network, OutPoint,
        ScriptBuf, Transaction, Txid,
    };
    use bitcoincore_rpc::json::{EstimateSmartFeeResult, GetRawTransactionResult};
    use mockall::{mock, predicate::*};

    use super::*;
    use crate::types;

    mock! {
        BitcoinRpc {}
        #[async_trait]
        impl BitcoinRpc for BitcoinRpc {
            async fn get_balance(&self, address: &Address) -> types::BitcoinRpcResult<u64>;
            async fn send_raw_transaction(&self, tx_hex: &str) -> types::BitcoinRpcResult<Txid>;
            async fn list_unspent(&self, address: &Address) -> types::BitcoinRpcResult<Vec<OutPoint>>;
            async fn get_transaction(&self, tx_id: &Txid) -> types::BitcoinRpcResult<Transaction>;
            async fn get_block_count(&self) -> types::BitcoinRpcResult<u64>;
            async fn get_block(&self, block_height: u128) -> types::BitcoinRpcResult<Block>;
            async fn get_best_block_hash(&self) -> types::BitcoinRpcResult<bitcoin::BlockHash>;
            async fn get_raw_transaction_info(&self, txid: &Txid) -> types::BitcoinRpcResult<GetRawTransactionResult>;
            async fn estimate_smart_fee(&self, conf_target: u16, estimate_mode: Option<EstimateMode>) -> types::BitcoinRpcResult<EstimateSmartFeeResult>;
        }
    }

    #[tokio::test]
    async fn test_new() {
        let client = BitcoinClient::new("http://localhost:8332", "regtest")
            .await
            .unwrap();
        assert_eq!(client.network, Network::Regtest);
    }

    #[tokio::test]
    async fn test_get_balance() {
        let mut mock_rpc = MockBitcoinRpc::new();
        mock_rpc.expect_get_balance().returning(|_| Ok(100000));

        let client = BitcoinClient {
            rpc: Box::new(mock_rpc),
            network: Network::Regtest,
        };

        let address = Address::from_str("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080").unwrap();
        let balance = client.get_balance(&address.assume_checked()).await.unwrap();

        assert_eq!(balance, 100000);
    }

    #[tokio::test]
    async fn test_fetch_utxos() {
        let mut mock_rpc = MockBitcoinRpc::new();
        mock_rpc.expect_list_unspent().returning(|_| {
            Ok(vec![OutPoint {
                txid: Txid::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000000",
                )
                .unwrap(),
                vout: 0,
            }])
        });
        mock_rpc.expect_get_transaction().returning(|_| {
            Ok(Transaction {
                version: Version(2),
                lock_time: LockTime::ZERO,
                input: vec![],
                output: vec![TxOut {
                    value: Amount::from_sat(50000),
                    script_pubkey: ScriptBuf::new(),
                }],
            })
        });

        let client = BitcoinClient {
            rpc: Box::new(mock_rpc),
            network: Network::Regtest,
        };

        let address = Address::from_str("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080").unwrap();
        let utxos = client.fetch_utxos(&address.assume_checked()).await.unwrap();

        assert_eq!(utxos.len(), 1);
        assert_eq!(utxos[0].0.value, Amount::from_sat(50000));
    }

    #[tokio::test]
    async fn test_fetch_block_height() {
        let mut mock_rpc = MockBitcoinRpc::new();
        mock_rpc.expect_get_block_count().returning(|| Ok(654321));

        let client = BitcoinClient {
            rpc: Box::new(mock_rpc),
            network: Network::Regtest,
        };

        let block_height = client.fetch_block_height().await.unwrap();

        assert_eq!(block_height, 654321);
    }

    #[tokio::test]
    async fn test_get_fee_rate() {
        let mut mock_rpc = MockBitcoinRpc::new();
        mock_rpc.expect_estimate_smart_fee().returning(|_, _| {
            Ok(EstimateSmartFeeResult {
                fee_rate: Some(Amount::from_sat(500)),
                errors: None,
                blocks: 0,
            })
        });

        let client = BitcoinClient {
            rpc: Box::new(mock_rpc),
            network: Network::Regtest,
        };

        let fee = client.get_fee_rate(6).await.unwrap();

        assert_eq!(fee, 500);
    }
}
