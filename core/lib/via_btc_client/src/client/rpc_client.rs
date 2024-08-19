use async_trait::async_trait;
use bitcoin::{Address, Block, BlockHash, OutPoint, Transaction, Txid};
use bitcoincore_rpc::{
    bitcoincore_rpc_json::EstimateMode,
    json::{EstimateSmartFeeResult, ScanTxOutRequest},
    Client, RpcApi,
};

use crate::{
    traits::BitcoinRpc,
    types::{Auth, BitcoinRpcResult},
};

pub struct BitcoinRpcClient {
    client: Client,
}

#[allow(unused)]
impl BitcoinRpcClient {
    pub fn new(url: &str, auth: Auth) -> Result<Self, bitcoincore_rpc::Error> {
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

    async fn get_block_by_height(&self, block_height: u128) -> BitcoinRpcResult<Block> {
        let block_hash = self.client.get_block_hash(block_height as u64)?;
        self.client.get_block(&block_hash).map_err(|e| e.into())
    }

    async fn get_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinRpcResult<Block> {
        self.client.get_block(block_hash).map_err(|e| e.into())
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
mod tests {
    use std::str::FromStr;

    use bitcoin::{
        absolute::LockTime, hashes::sha256d::Hash, transaction::Version, Address, Amount, Block,
        BlockHash, Network, OutPoint, Transaction, TxMerkleNode, Txid, Wtxid,
    };
    use bitcoincore_rpc::json::{EstimateSmartFeeResult, GetRawTransactionResult};
    use mockall::{mock, predicate::*};

    use super::*;

    mock! {
        BitcoinRpcClient {}

        #[async_trait]
        impl BitcoinRpc for BitcoinRpcClient {
            async fn get_balance(&self, address: &Address) -> BitcoinRpcResult<u64>;
            async fn send_raw_transaction(&self, tx_hex: &str) -> BitcoinRpcResult<Txid>;
            async fn list_unspent(&self, address: &Address) -> BitcoinRpcResult<Vec<OutPoint>>;
            async fn get_transaction(&self, txid: &Txid) -> BitcoinRpcResult<Transaction>;
            async fn get_block_count(&self) -> BitcoinRpcResult<u64>;
            async fn get_block_by_height(&self, block_height: u128) -> BitcoinRpcResult<Block>;
            async fn get_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinRpcResult<Block>;
            async fn get_best_block_hash(&self) -> BitcoinRpcResult<bitcoin::BlockHash>;
            async fn get_raw_transaction_info(&self, txid: &Txid) -> BitcoinRpcResult<GetRawTransactionResult>;
            async fn estimate_smart_fee(&self, conf_target: u16, estimate_mode: Option<EstimateMode>) -> BitcoinRpcResult<EstimateSmartFeeResult>;
        }
    }

    #[tokio::test]
    async fn test_get_balance() {
        let mut mock = MockBitcoinRpcClient::new();
        let address = Address::from_str("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq")
            .unwrap()
            .require_network(Network::Bitcoin)
            .unwrap();
        let expected_balance = 1000000;

        mock.expect_get_balance()
            .with(eq(address.clone()))
            .return_once(move |_| Ok(expected_balance));

        let result = mock.get_balance(&address).await.unwrap();
        assert_eq!(result, expected_balance);
    }

    #[tokio::test]
    async fn test_send_raw_transaction() {
        let mut mock = MockBitcoinRpcClient::new();
        let tx_hex = "0200000001...";
        let expected_txid =
            Txid::from_str("1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef")
                .unwrap();

        mock.expect_send_raw_transaction()
            .with(eq(tx_hex))
            .return_once(move |_| Ok(expected_txid));

        let result = mock.send_raw_transaction(tx_hex).await.unwrap();
        assert_eq!(result, expected_txid);
    }

    #[tokio::test]
    async fn test_list_unspent() {
        let mut mock = MockBitcoinRpcClient::new();
        let address = Address::from_str("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq")
            .unwrap()
            .require_network(Network::Bitcoin)
            .unwrap();
        let expected_unspent = vec![
            OutPoint {
                txid: Txid::from_str(
                    "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                )
                .unwrap(),
                vout: 0,
            },
            OutPoint {
                txid: Txid::from_str(
                    "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
                )
                .unwrap(),
                vout: 1,
            },
        ];

        let expected_cloned = expected_unspent.clone();
        mock.expect_list_unspent()
            .with(eq(address.clone()))
            .return_once(move |_| Ok(expected_cloned));

        let result = mock.list_unspent(&address).await.unwrap();
        assert_eq!(result, expected_unspent);
    }

    #[tokio::test]
    async fn test_get_transaction() {
        let mut mock = MockBitcoinRpcClient::new();
        let txid =
            Txid::from_str("1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef")
                .unwrap();
        let expected_tx = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![],
            output: vec![],
        };

        let expected_cloned = expected_tx.clone();
        mock.expect_get_transaction()
            .with(eq(txid))
            .return_once(move |_| Ok(expected_cloned));

        let result = mock.get_transaction(&txid).await.unwrap();
        assert_eq!(result, expected_tx);
    }

    #[tokio::test]
    async fn test_get_block_count() {
        let mut mock = MockBitcoinRpcClient::new();
        let expected_count = 654321;

        mock.expect_get_block_count()
            .return_once(move || Ok(expected_count));

        let result = mock.get_block_count().await.unwrap();
        assert_eq!(result, expected_count);
    }

    #[tokio::test]
    async fn test_get_block_by_height() {
        let mut mock = MockBitcoinRpcClient::new();
        let block_height = 654321;
        let expected_block = Block {
            header: bitcoin::block::Header {
                version: Default::default(),
                prev_blockhash: BlockHash::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000000",
                )
                .unwrap(),
                merkle_root: TxMerkleNode::from_raw_hash(
                    Hash::from_str(
                        "0000000000000000000000000000000000000000000000000000000000000000",
                    )
                    .unwrap(),
                ),
                time: 0,
                bits: Default::default(),
                nonce: 0,
            },
            txdata: vec![],
        };
        let expected_cloned = expected_block.clone();
        mock.expect_get_block_by_height()
            .with(eq(block_height))
            .return_once(move |_| Ok(expected_cloned));

        let result = mock.get_block_by_height(block_height).await.unwrap();
        assert_eq!(result, expected_block);
    }

    #[tokio::test]
    async fn test_get_block_by_hash() {
        let mut mock = MockBitcoinRpcClient::new();
        let block_hash =
            BlockHash::from_str("000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f")
                .unwrap();
        let expected_block = Block {
            header: bitcoin::block::Header {
                version: Default::default(),
                prev_blockhash: BlockHash::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000000",
                )
                .unwrap(),
                merkle_root: TxMerkleNode::from_raw_hash(
                    Hash::from_str(
                        "0000000000000000000000000000000000000000000000000000000000000000",
                    )
                    .unwrap(),
                ),
                time: 0,
                bits: Default::default(),
                nonce: 0,
            },
            txdata: vec![],
        };
        let expected_cloned = expected_block.clone();
        mock.expect_get_block_by_hash()
            .with(eq(block_hash))
            .return_once(move |_| Ok(expected_cloned));

        let result = mock.get_block_by_hash(&block_hash).await.unwrap();
        assert_eq!(result, expected_block);
    }

    #[tokio::test]
    async fn test_get_best_block_hash() {
        let mut mock = MockBitcoinRpcClient::new();
        let expected_hash =
            BlockHash::from_str("000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f")
                .unwrap();

        mock.expect_get_best_block_hash()
            .return_once(move || Ok(expected_hash));

        let result = mock.get_best_block_hash().await.unwrap();
        assert_eq!(result, expected_hash);
    }

    #[tokio::test]
    async fn test_get_raw_transaction_info() {
        let mut mock = MockBitcoinRpcClient::new();
        let txid =
            Txid::from_str("1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef")
                .unwrap();
        let expected_info = GetRawTransactionResult {
            in_active_chain: None,
            hex: vec![],
            txid,
            hash: Wtxid::from_raw_hash(
                Hash::from_str("0000000000000000000000000000000000000000000000000000000000000000")
                    .unwrap(),
            ),
            size: 0,
            vsize: 0,
            version: 0,
            locktime: 0,
            vin: vec![],
            vout: vec![],
            blockhash: None,
            confirmations: None,
            time: None,
            blocktime: None,
        };
        let expected_cloned = expected_info.clone();
        mock.expect_get_raw_transaction_info()
            .with(eq(txid))
            .return_once(move |_| Ok(expected_cloned));

        let result = mock.get_raw_transaction_info(&txid).await.unwrap();
        assert_eq!(result, expected_info);
    }

    #[tokio::test]
    async fn test_estimate_smart_fee() {
        let mut mock = MockBitcoinRpcClient::new();
        let conf_target = 6;
        let estimate_mode = Some(EstimateMode::Conservative);
        let expected_result = EstimateSmartFeeResult {
            fee_rate: Some(Amount::from_sat(12345)),
            errors: None,
            blocks: 6,
        };

        let expected_cloned = expected_result.clone();
        mock.expect_estimate_smart_fee()
            .with(eq(conf_target), eq(estimate_mode))
            .return_once(move |_, _| Ok(expected_cloned));

        let result = mock
            .estimate_smart_fee(conf_target, estimate_mode)
            .await
            .unwrap();
        assert_eq!(result.fee_rate, expected_result.fee_rate);
    }
}
