use std::{collections::HashMap, str::FromStr, u64};

use async_trait::async_trait;
use bitcoin::{Address, Block, BlockHash, Network, OutPoint, Transaction, TxOut, Txid};
use bitcoincore_rpc::json::EstimateMode;
use futures::future::join_all;
use tracing::{debug, error, instrument};

mod rpc_client;

use crate::{
    client::rpc_client::BitcoinRpcClient,
    traits::{BitcoinOps, BitcoinRpc},
    types::{BitcoinClientResult, BitcoinError, BitcoinNetwork, NodeAuth},
};

pub struct BitcoinClient {
    rpc: Box<dyn BitcoinRpc>,
    network: BitcoinNetwork,
}

impl BitcoinClient {
    #[instrument(skip(auth), target = "bitcoin_client")]
    pub fn new(rpc_url: &str, network: BitcoinNetwork, auth: NodeAuth) -> BitcoinClientResult<Self>
    where
        Self: Sized,
    {
        debug!("Creating new BitcoinClient");
        let rpc = Box::new(BitcoinRpcClient::new(rpc_url, auth)?);
        Ok(Self { rpc, network })
    }
}

#[async_trait]
impl BitcoinOps for BitcoinClient {
    #[instrument(skip(self), target = "bitcoin_client")]
    async fn get_balance(&self, address: &Address) -> BitcoinClientResult<u128> {
        debug!("Getting balance");
        match self.network {
            BitcoinNetwork::Regtest => {
                let balance = self.rpc.get_balance_scan(address).await?;
                Ok(balance as u128)
            }
            _ => {
                let balance = self.rpc.get_balance(address).await?;
                Ok(balance as u128)
            }
        }
    }

    #[instrument(skip(self, signed_transaction), target = "bitcoin_client")]
    async fn broadcast_signed_transaction(
        &self,
        signed_transaction: &str,
    ) -> BitcoinClientResult<Txid> {
        debug!("Broadcasting signed transaction");
        let txid = self.rpc.send_raw_transaction(signed_transaction).await?;
        Ok(txid)
    }

    // The address should be imported to the node
    // bitcoin-cli createwallet "watch-only" true
    // bitcoin-cli getdescriptorinfo "addr(p2wpkh address)"
    // bitcoin-cli importdescriptors '[{"desc": "addr(p2wpkh address)", "timestamp": "now", "range": 1000, "watchonly": true, "label": "watch-only"}]'
    #[instrument(skip(self), target = "bitcoin_client")]
    async fn fetch_utxos(&self, address: &Address) -> BitcoinClientResult<Vec<(OutPoint, TxOut)>> {
        debug!("Fetching UTXOs");
        let outpoints = match self.network {
            Network::Regtest => self.rpc.list_unspent(address).await?,
            _ => self.rpc.list_unspent_based_on_node_wallet(address).await?,
        };
        let mut utxos = Vec::with_capacity(outpoints.len());

        for outpoint in outpoints {
            debug!("Fetching transaction for outpoint");
            let tx = self.rpc.get_transaction(&outpoint.txid).await?;
            let txout = tx.output.get(outpoint.vout as usize).ok_or_else(|| {
                error!("Invalid outpoint");
                BitcoinError::InvalidOutpoint(outpoint.to_string())
            })?;
            utxos.push((outpoint, txout.clone()));
        }

        Ok(utxos)
    }

    #[instrument(skip(self), target = "bitcoin_client")]
    async fn check_tx_confirmation(&self, txid: &Txid, conf_num: u32) -> BitcoinClientResult<bool> {
        debug!("Checking transaction confirmation");
        let tx_info = self.rpc.get_raw_transaction_info(txid).await?;

        match tx_info.confirmations {
            Some(confirmations) => Ok(confirmations >= conf_num),
            None => Ok(false),
        }
    }

    #[instrument(skip(self), target = "bitcoin_client")]
    async fn fetch_block_height(&self) -> BitcoinClientResult<u128> {
        debug!("Fetching block height");
        let height = self.rpc.get_block_count().await?;
        Ok(height as u128)
    }

    #[instrument(skip(self), target = "bitcoin_client")]
    async fn get_fee_rate(&self, conf_target: u16) -> BitcoinClientResult<u64> {
        debug!("Estimating fee rate");
        let estimation = self
            .rpc
            .estimate_smart_fee(conf_target, Some(EstimateMode::Economical))
            .await?;

        match estimation.fee_rate {
            Some(fee_rate) => {
                // convert btc/kb to sat/byte
                let fee_rate_sat_kb = fee_rate.to_sat();
                let fee_rate_sat_byte = fee_rate_sat_kb.checked_div(1000);
                match fee_rate_sat_byte {
                    Some(fee_rate_sat_byte) => Ok(fee_rate_sat_byte),
                    None => Err(BitcoinError::FeeEstimationFailed(
                        "Invalid fee rate".to_string(),
                    )),
                }
            }
            None => {
                let err = estimation
                    .errors
                    .map(|errors| errors.join(", "))
                    .unwrap_or_else(|| "Unknown error during fee estimation".to_string());
                error!("Fee estimation failed: {}", err);
                Err(BitcoinError::FeeEstimationFailed(err))
            }
        }
    }

    fn get_network(&self) -> BitcoinNetwork {
        self.network
    }

    #[instrument(skip(self), target = "bitcoin_client")]
    async fn fetch_block(&self, block_height: u128) -> BitcoinClientResult<Block> {
        debug!("Fetching block");
        self.rpc.get_block_by_height(block_height).await
    }

    #[instrument(skip(self), target = "bitcoin_client")]
    async fn get_transaction(&self, txid: &Txid) -> BitcoinClientResult<Transaction> {
        debug!("Getting transaction");
        self.rpc.get_transaction(txid).await
    }

    #[instrument(skip(self), target = "bitcoin_client")]
    async fn fetch_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinClientResult<Block> {
        debug!("Fetching block by hash");
        self.rpc.get_block_by_hash(block_hash).await
    }

    /*
    Retrieve the "fee_history" for the Bitcoin blockchain between the specified 'from_block_height' and 'to_block_height'.
    Transaction fees are calculated on a per-transaction basis using the following formula:

    **fee_per_tx = (total_amount_in - total_amount_out) / vbyte**.

    The "base_fee" represents the minimum fee (in satoshis per vbyte) that users must pay to ensure their transactions are included
    in a block. The "fee_history" list contains the calculated base_fee for each block, reflecting the minimum fee paid per vbyte.

    Example:

    Input:
    - Block1 [tx1 (5 sat/vbyte), tx2 (10 sat/vbyte)]
    - Block2 [tx1 (6 sat/vbyte), tx2 (3 sat/vbyte)]

    Output:
    fee_history = [5, 3]
     */
    #[instrument(skip(self), target = "bitcoin_client")]
    async fn get_fee_history(
        &self,
        from_block_height: usize,
        to_block_height: usize,
    ) -> BitcoinClientResult<Vec<u64>> {
        debug!("Fetching blocks fee history");

        let mut fetch_blocks_futures = Vec::new();

        for block_height in from_block_height..to_block_height {
            fetch_blocks_futures.push(self.fetch_block(block_height as u128));
        }

        let blocks = join_all(fetch_blocks_futures).await;

        let mut fees_per_block: HashMap<u128, u64> = HashMap::new();

        // To minimize the number of asynchronous calls, we will process each block sequentially, as a single Bitcoin block can contain up to 2,000 transactions.
        for (index, block_result) in blocks.iter().enumerate() {
            match block_result {
                Ok(block) => {
                    let current_block = from_block_height as u128 + index as u128;

                    let mut fetch_fee_futures = Vec::new();
                    for tx in block.txdata.clone() {
                        fetch_fee_futures
                            .push(self.calculate_tx_fee_per_byte(current_block, tx.clone()));
                    }

                    let txs_fee_result = join_all(fetch_fee_futures).await;

                    for tx_fee in txs_fee_result {
                        match tx_fee {
                            Ok((block_height, fees)) => {
                                let base_fee =
                                    *fees_per_block.get(&block_height).unwrap_or(&u64::MAX);
                                if base_fee > fees && fees != 0 {
                                    fees_per_block.insert(block_height, fees);
                                }
                            }
                            Err(err) => {
                                return BitcoinClientResult::Err(err.clone());
                            }
                        }
                    }
                }
                Err(err) => {
                    return BitcoinClientResult::Err(err.clone());
                }
            }
        }

        let mut fee_history: Vec<u64> = Vec::new();
        for (_, fees) in fees_per_block {
            fee_history.push(fees);
        }

        Ok(fee_history)
    }

    async fn calculate_tx_fee_per_byte(
        &self,
        block_height: u128,
        tx: Transaction,
    ) -> BitcoinClientResult<(u128, u64)> {
        let mut total_amount_in: u64 = 0;
        let mut total_amount_out: u64 = 0;

        for input in tx.input.clone() {
            // When regtest the transaction responsible to transfer the block fee to the user has 0x0 previous tx hash
            if input.previous_output.txid
                == Txid::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000000",
                )
                .unwrap()
            {
                return Ok((block_height, 0));
            };
            let intx = self.get_transaction(&input.previous_output.txid).await?;
            let txout = intx.output[input.previous_output.vout as usize].clone();
            total_amount_in += txout.value.to_sat();
        }

        for output in tx.output.clone() {
            total_amount_out += output.value.to_sat();
        }
        Ok((
            block_height,
            (total_amount_in - total_amount_out) / tx.vsize() as u64,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bitcoin::{absolute::LockTime, hashes::Hash, transaction::Version, Amount, Wtxid};
    use bitcoincore_rpc::{
        bitcoincore_rpc_json::GetBlockchainInfoResult,
        json::{EstimateSmartFeeResult, GetRawTransactionResult},
    };
    use mockall::{mock, predicate::*};

    use super::*;
    use crate::types::BitcoinRpcResult;

    mock! {
        BitcoinRpc {}
        #[async_trait]
        impl BitcoinRpc for BitcoinRpc {
            async fn get_balance(&self, address: &Address) -> BitcoinClientResult<u64>;
            async fn get_balance_scan(&self, address: &Address) -> BitcoinClientResult<u64>;
            async fn send_raw_transaction(&self, tx_hex: &str) -> BitcoinClientResult<Txid>;
            async fn list_unspent_based_on_node_wallet(&self, address: &Address) -> BitcoinClientResult<Vec<OutPoint>>;
            async fn list_unspent(&self, address: &Address) -> BitcoinClientResult<Vec<OutPoint>>;
            async fn get_transaction(&self, txid: &Txid) -> BitcoinClientResult<Transaction>;
            async fn get_block_count(&self) -> BitcoinClientResult<u64>;
            async fn get_block_by_height(&self, block_height: u128) -> BitcoinClientResult<Block>;
            async fn get_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinClientResult<Block>;
            async fn get_best_block_hash(&self) -> BitcoinClientResult<BlockHash>;
            async fn get_raw_transaction_info(&self, txid: &Txid) -> BitcoinClientResult<GetRawTransactionResult>;
            async fn estimate_smart_fee(&self, conf_target: u16, estimate_mode: Option<EstimateMode>) -> BitcoinClientResult<EstimateSmartFeeResult>;
            async fn get_blockchain_info(&self) -> BitcoinRpcResult<GetBlockchainInfoResult>;
        }
    }

    fn get_client_with_mock(mock_bitcoin_rpc: MockBitcoinRpc) -> BitcoinClient {
        BitcoinClient {
            rpc: Box::new(mock_bitcoin_rpc),
            network: BitcoinNetwork::Bitcoin,
        }
    }

    #[tokio::test]
    async fn test_get_balance() {
        let mut mock_rpc = MockBitcoinRpc::new();
        mock_rpc.expect_get_balance().return_once(|_| Ok(1000000));

        let client = get_client_with_mock(mock_rpc);
        let address = Address::from_str("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq")
            .unwrap()
            .require_network(BitcoinNetwork::Bitcoin)
            .unwrap();
        let balance = client.get_balance(&address).await.unwrap();
        assert_eq!(balance, 1000000);
    }

    #[tokio::test]
    async fn test_broadcast_signed_transaction() {
        let mut mock_rpc = MockBitcoinRpc::new();
        let expected_txid =
            Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                .unwrap();
        mock_rpc
            .expect_send_raw_transaction()
            .return_once(move |_| Ok(expected_txid));

        let client = get_client_with_mock(mock_rpc);

        let txid = client
            .broadcast_signed_transaction("dummy_hex")
            .await
            .unwrap();
        assert_eq!(txid, expected_txid);
    }

    #[tokio::test]
    async fn test_fetch_utxos() {
        let mut mock_rpc = MockBitcoinRpc::new();
        let outpoint = OutPoint {
            txid: Txid::all_zeros(),
            vout: 0,
        };
        mock_rpc
            .expect_list_unspent_based_on_node_wallet()
            .return_once(move |_| Ok(vec![outpoint]));
        mock_rpc.expect_get_transaction().return_once(|_| {
            Ok(Transaction {
                version: Version::TWO,
                lock_time: LockTime::from_height(0u32).unwrap(),
                input: vec![],
                output: vec![TxOut {
                    value: Amount::from_sat(50000),
                    script_pubkey: Default::default(),
                }],
            })
        });

        let client = get_client_with_mock(mock_rpc);

        let address = Address::from_str("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq")
            .unwrap()
            .require_network(BitcoinNetwork::Bitcoin)
            .unwrap();
        let utxos = client.fetch_utxos(&address).await.unwrap();
        assert_eq!(utxos.len(), 1);
        assert_eq!(utxos[0].0, outpoint);
        assert_eq!(utxos[0].1.value.to_sat(), 50000);
    }

    #[tokio::test]
    async fn test_check_tx_confirmation() {
        let mut mock_rpc = MockBitcoinRpc::new();
        mock_rpc.expect_get_raw_transaction_info().return_once(|_| {
            Ok(GetRawTransactionResult {
                in_active_chain: None,
                hex: vec![],
                txid: Txid::all_zeros(),
                hash: Wtxid::all_zeros(),
                size: 0,
                vsize: 0,
                version: 0,
                locktime: 0,
                vin: vec![],
                vout: vec![],
                blockhash: None,
                confirmations: Some(3),

                time: None,
                blocktime: None,
            })
        });

        let client = get_client_with_mock(mock_rpc);

        let txid = Txid::all_zeros();
        let confirmed = client.check_tx_confirmation(&txid, 2).await.unwrap();
        assert!(confirmed);
    }

    #[tokio::test]
    async fn test_fetch_block_height() {
        let mut mock_rpc = MockBitcoinRpc::new();
        mock_rpc.expect_get_block_count().return_once(|| Ok(654321));

        let client = get_client_with_mock(mock_rpc);

        let height = client.fetch_block_height().await.unwrap();
        assert_eq!(height, 654321);
    }

    #[tokio::test]
    async fn test_get_fee_rate() {
        let mut mock_rpc = MockBitcoinRpc::new();
        mock_rpc.expect_estimate_smart_fee().return_once(|_, _| {
            Ok(EstimateSmartFeeResult {
                fee_rate: Some(Amount::from_sat(1000)),
                errors: None,
                blocks: 0,
            })
        });

        let client = get_client_with_mock(mock_rpc);

        let fee_rate = client.get_fee_rate(6).await.unwrap();
        // 1000 sat/kb = 1 sat/byte
        assert_eq!(fee_rate, 1);
    }
}
