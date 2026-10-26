use std::{num::NonZeroU32, sync::Arc};

use anyhow::Context;
use async_trait::async_trait;
use bitcoin::{Address, Block, BlockHash, Network, OutPoint, Transaction, TxOut, Txid};
use bitcoincore_rpc::json::{EstimateMode, GetBlockStatsResult};
use futures::future::join_all;
use tracing::{debug, error, instrument};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

mod fee_limits;
mod rpc_client;

use crate::{
    client::{fee_limits::FeeRateLimits, rpc_client::BitcoinRpcClient},
    metrics::{RpcMethodLabel, METRICS},
    traits::{BitcoinOps, BitcoinRpc},
    types::{
        BitcoinClientResult, BitcoinError, BitcoinNetwork, NodeAuth, BlockHeaderInfo,
        HeaderMembership, HeaderValidationError,
    },
};

#[derive(Debug)]
pub struct BitcoinClient {
    rpc: Arc<dyn BitcoinRpc>,
    pub config: ViaBtcClientConfig,
}

impl BitcoinClient {
    pub async fn best_block_hash(&self) -> BitcoinClientResult<BlockHash> {
        self.rpc.get_best_block_hash().await
    }

    pub async fn header_by_hash(
        &self,
        hash: &BlockHash,
    ) -> BitcoinClientResult<Option<BlockHeaderInfo>> {
        self.rpc.get_block_header(hash).await
    }

    /// checked_block fetches block by checked hash, then verifies hash, parent, merkle root and witness commitment.
    /// Corrupt or substituted body fails here, before parsing and before any SQL write.
    /// Idea only from BDK bitcoind_rpc Emitter, which fetches by hash and rechecks hash before emitting: https://github.com/bitcoindevkit/bdk/blob/5dc6e959379982a1661e8f57c3ee7af334627ca5/crates/bitcoind_rpc/src/lib.rs
    pub async fn checked_block(&self, header: &BlockHeaderInfo) -> BitcoinClientResult<Block> {
        crate::traits::checked_block(self, header).await
    }

    /// canonical_transaction treats getrawtransaction blockhash as hint and proves inclusion from checked canonical header and body.
    /// Transaction in orphaned or unknown block yields None instead of stale origin.
    /// Idea only from BDK bitcoind_rpc Emitter, which trusts membership only after header check: https://github.com/bitcoindevkit/bdk/blob/5dc6e959379982a1661e8f57c3ee7af334627ca5/crates/bitcoind_rpc/src/lib.rs
    pub async fn canonical_transaction(
        &self,
        txid: &Txid,
    ) -> BitcoinClientResult<Option<(Transaction, BlockHeaderInfo, usize)>> {
        let info = self.rpc.get_raw_transaction_info(txid).await?;
        let Some(hash) = info.blockhash else {
            return Ok(None);
        };
        let Some(header) = self.header_by_hash(&hash).await? else {
            return Ok(None);
        };
        if !matches!(header.membership, HeaderMembership::Canonical(_)) {
            return Ok(None);
        }
        let block = self.checked_block(&header).await?;
        let (index, transaction) = block
            .txdata
            .into_iter()
            .enumerate()
            .find(|(_, transaction)| transaction.compute_txid() == *txid)
            .ok_or(BitcoinError::InvalidBlockBody(hash))?;
        Ok(Some((transaction, header, index)))
    }

    /// Related inscriptions inherit the exact checked transaction's inclusion, never its RPC hint alone.
    pub async fn canonical_messages(
        &self,
        txid: &Txid,
    ) -> BitcoinClientResult<Option<Vec<crate::types::FullInscriptionMessage>>> {
        let Some((transaction, header, index)) = self.canonical_transaction(txid).await? else {
            return Ok(None);
        };
        let mut messages = crate::indexer::MessageParser::new(self.get_network())
            .parse_system_transaction(&transaction, header.height, None);
        for message in &mut messages {
            let common = message.common_mut();
            common.tx_id = *txid;
            common.tx_index = Some(index);
            common.block_hash = Some(header.hash);
        }
        Ok(Some(messages))
    }

    pub async fn prepare_proofs(
        &self,
        messages: &[crate::types::FullInscriptionMessage],
    ) -> anyhow::Result<Vec<crate::indexer::replay::Included<crate::indexer::replay::ProofInput>>>
    {
        use crate::{
            indexer::replay::{Included, ProofInput},
            types::FullInscriptionMessage,
        };
        let mut proofs = Vec::new();
        for message in messages {
            let FullInscriptionMessage::ProofDAReference(proof) = message else { continue };
            let related = self
                .canonical_messages(&proof.input.l1_batch_reveal_txid)
                .await
                .with_context(|| format!("pubdata for proof {}", proof.common.tx_id))?
                .context("proof pubdata has no canonical inclusion")?;
            let [FullInscriptionMessage::L1BatchDAReference(pubdata)] = related.as_slice() else {
                anyhow::bail!(
                    "proof {} does not reference one pubdata inscription",
                    proof.common.tx_id
                );
            };
            anyhow::ensure!(
                pubdata.common.block_height <= proof.common.block_height,
                "pubdata is outside proof prefix"
            );
            proofs.push(Included::new(
                &proof.common,
                ProofInput {
                    proof: proof.input.clone(),
                    pubdata: Included::new(&pubdata.common, pubdata.input.clone())?,
                },
            )?);
        }
        Ok(proofs)
    }

    pub fn reorg_safe_depth(&self) -> BitcoinClientResult<NonZeroU32> {
        let network = self
            .config
            .network
            .parse::<Network>()
            .map_err(|error| BitcoinError::InvalidNetwork(error.to_string()))?;
        Ok(crate::traits::network_reorg_safe_depth(network))
    }

    /// canonical_header_at resolves height to hash, then verifies header height and canonical membership.
    /// Core may switch chains between calls, so mismatch returns error and caller retries without progress.
    /// Idea only from BDK bitcoind_rpc Emitter, which compares get_block_hash twice around each fetch: https://github.com/bitcoindevkit/bdk/blob/5dc6e959379982a1661e8f57c3ee7af334627ca5/crates/bitcoind_rpc/src/lib.rs
    pub async fn canonical_header_at(&self, height: u32) -> BitcoinClientResult<BlockHeaderInfo> {
        let hash = self.rpc.get_block_hash(u64::from(height)).await?;
        let header = self.header_by_hash(&hash).await?.ok_or_else(|| {
            BitcoinError::Rpc(format!("Core does not know selected header {height}/{hash}"))
        })?;
        let reason = if header.height != height {
            Some(HeaderValidationError::HeightMismatch)
        } else if !matches!(header.membership, HeaderMembership::Canonical(_)) {
            Some(HeaderValidationError::Noncanonical)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(match reason {
                HeaderValidationError::Noncanonical => {
                    BitcoinError::Rpc(format!("Core selection changed at {height}/{hash}"))
                }
                reason => BitcoinError::InvalidBlockHeader { hash, reason },
            });
        }
        Ok(header)
    }

    #[instrument(skip(auth, rpc_url), target = "bitcoin_client")]
    pub fn new(
        rpc_url: &str,
        auth: NodeAuth,
        config: ViaBtcClientConfig,
    ) -> BitcoinClientResult<Self>
    where
        Self: Sized,
    {
        Self::new_with_component(rpc_url, auth, config, "client")
    }

    #[instrument(skip(auth, rpc_url), target = "bitcoin_client")]
    pub fn new_with_component(
        rpc_url: &str,
        auth: NodeAuth,
        config: ViaBtcClientConfig,
        component: &'static str,
    ) -> BitcoinClientResult<Self> {
        config
            .network
            .parse::<Network>()
            .map_err(|error| BitcoinError::InvalidNetwork(error.to_string()))?;
        debug!("Creating new BitcoinClient");
        let rpc = BitcoinRpcClient::new(rpc_url, auth, component)?;
        Ok(Self {
            rpc: Arc::new(rpc),
            config,
        })
    }
}

#[async_trait]
impl BitcoinOps for BitcoinClient {
    fn reorg_safe_depth(&self) -> BitcoinClientResult<NonZeroU32> {
        self.reorg_safe_depth()
    }
    async fn best_block_hash(&self) -> BitcoinClientResult<BlockHash> {
        self.best_block_hash().await
    }
    async fn header_by_hash(
        &self,
        hash: &BlockHash,
    ) -> BitcoinClientResult<Option<BlockHeaderInfo>> {
        self.header_by_hash(hash).await
    }
    async fn canonical_header_at(&self, height: u32) -> BitcoinClientResult<BlockHeaderInfo> {
        self.canonical_header_at(height).await
    }
    #[instrument(skip(self), target = "bitcoin_client")]
    async fn get_balance(&self, address: &Address) -> BitcoinClientResult<u128> {
        debug!("Getting balance");
        match self.config.network() {
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
        let outpoints = match self.config.network() {
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

    async fn is_reorg_safe(&self, txid: &Txid) -> BitcoinClientResult<bool> {
        let info = self.rpc.get_raw_transaction_info(txid).await?;
        let Some(hash) = info.blockhash else { return Ok(false) };
        let depth = self.reorg_safe_depth()?.get();
        if !info.confirmations.is_some_and(|confirmations| confirmations >= depth) {
            return Ok(false);
        }
        Ok(self.header_by_hash(&hash).await?.is_some_and(|header|
            matches!(header.membership, HeaderMembership::Canonical(n) if n.get() >= depth)))
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
    async fn fetch_block_height(&self) -> BitcoinClientResult<u64> {
        debug!("Fetching block height");
        let height = self.rpc.get_block_count().await?;
        Ok(height)
    }

    #[instrument(skip(self), target = "bitcoin_client")]
    async fn get_fee_rate(&self, conf_target: u16) -> BitcoinClientResult<u64> {
        debug!("Estimating fee rate");
        let mut fee_rate_sat_kb: Option<u64> = None;

        if self.config.use_rpc_for_fee_rate() {
            let estimation_result = self
                .rpc
                .estimate_smart_fee(conf_target, Some(EstimateMode::Economical))
                .await;

            fee_rate_sat_kb = match estimation_result {
                Ok(estimation) => {
                    if let Some(fee_rate) = estimation.fee_rate {
                        Some(fee_rate.to_sat())
                    } else {
                        error!(
                            "RPC fee estimate missing value: {}",
                            estimation
                                .errors
                                .map(|e| e.join(", "))
                                .unwrap_or_else(|| "Unknown error".to_string())
                        );
                        None
                    }
                }
                Err(err) => {
                    METRICS.rpc_errors[&RpcMethodLabel {
                        method: "rpc_estimate_smart_fee".into(),
                    }]
                        .inc();
                    error!("Failed to estimate smart fee via RPC: {:?}", err);
                    None
                }
            };
        }

        // Fallback to external APIs if RPC failed or returned no fee
        if fee_rate_sat_kb.is_none() {
            let client = reqwest::Client::new();

            for (api_url, fee_target_key) in self
                .config
                .external_apis
                .iter()
                .zip(self.config.fee_strategies.iter())
            {
                match client.get(api_url).send().await {
                    Ok(resp) => match resp.json::<serde_json::Value>().await {
                        Ok(json) => {
                            if let Some(fee_rate_vb) =
                                json.get(fee_target_key).and_then(|v| v.as_f64())
                            {
                                fee_rate_sat_kb = Some((fee_rate_vb * 1000.0).round() as u64);
                                debug!(
                                    "Fee rate from API {} [{}]: {} sat/vB",
                                    api_url, fee_target_key, fee_rate_vb
                                );
                                break;
                            } else {
                                error!("Missing '{}' in response from {}", fee_target_key, api_url);
                            }
                        }
                        Err(e) => {
                            error!("Failed to parse JSON from {}: {:?}", api_url, e);
                        }
                    },
                    Err(e) => {
                        METRICS.rpc_errors[&RpcMethodLabel {
                            method: "estimate_smart_fee".into(),
                        }]
                            .inc();
                        error!("Failed to fetch from {}: {:?}", api_url, e);
                    }
                }
            }
        }

        // If no fee estimate was obtained
        let mut fee_rate_sat_kb = fee_rate_sat_kb.ok_or_else(|| {
            BitcoinError::FeeEstimationFailed("All fee estimation methods failed".into())
        })?;

        // Add a small buffer to avoid precision loss
        fee_rate_sat_kb += 1000;

        // Get mempool minimum fee
        let mempool_info = self.rpc.get_mempool_info().await?;
        let mempool_min_fee_sat_kb = mempool_info.mempool_min_fee.to_sat();
        fee_rate_sat_kb = std::cmp::max(fee_rate_sat_kb, mempool_min_fee_sat_kb);

        // Convert to sat/vB
        let fee_rate_sat_vb = fee_rate_sat_kb.checked_div(1000).ok_or_else(|| {
            BitcoinError::FeeEstimationFailed("Failed to convert sat/kB to sat/vB".into())
        })?;

        // Apply network-specific caps
        let limits = FeeRateLimits::from_network(self.config.network());
        let capped = std::cmp::min(fee_rate_sat_vb, limits.max_fee_rate());
        let final_rate = std::cmp::max(capped, limits.min_fee_rate());

        debug!("Final fee rate used: {} sat/vB", final_rate);
        Ok(final_rate)
    }

    fn get_network(&self) -> BitcoinNetwork {
        self.config.network()
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

    #[instrument(skip(self), target = "bitcoin_client")]
    async fn get_block_stats(&self, height: u64) -> BitcoinClientResult<GetBlockStatsResult> {
        debug!("Fetching block by hash");
        self.rpc.get_block_stats(height).await
    }

    /// Retrieve the "fee_history" for the Bitcoin blockchain between provided blocks 'from_block_height' and 'to_block_height'.
    #[instrument(skip(self), target = "bitcoin_client")]
    async fn get_fee_history(
        &self,
        from_block_height: usize,
        to_block_height: usize,
    ) -> BitcoinClientResult<Vec<u64>> {
        debug!("Fetching blocks fee history");

        let mut fetch_blocks_futures = Vec::new();
        for block_height in from_block_height..to_block_height {
            fetch_blocks_futures.push(self.get_block_stats(block_height as u64));
        }

        let blocks = join_all(fetch_blocks_futures).await;
        let mut fee_history: Vec<u64> = Vec::new();

        for block_result in blocks {
            match block_result {
                Ok(block) => {
                    fee_history.push(std::cmp::max(block.min_fee_rate.to_sat(), 1));
                }
                Err(err) => {
                    return BitcoinClientResult::Err(err.clone());
                }
            }
        }
        Ok(fee_history)
    }
}

impl Clone for BitcoinClient {
    fn clone(&self) -> Self {
        Self {
            rpc: Arc::clone(&self.rpc),
            config: ViaBtcClientConfig::for_tests(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bitcoin::{absolute::LockTime, hashes::Hash, transaction::Version, Amount, Wtxid};
    use bitcoincore_rpc::{
        bitcoincore_rpc_json::GetBlockchainInfoResult,
        json::{EstimateSmartFeeResult, GetMempoolInfoResult, GetRawTransactionResult},
    };
    use mockall::{mock, predicate::*};

    use super::*;
    use crate::types::BitcoinRpcResult;

    mock! {
        #[derive(Debug)]
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
            async fn get_block_header(&self, hash: &BlockHash) -> BitcoinRpcResult<Option<BlockHeaderInfo>>;
            async fn get_block_by_height(&self, block_height: u128) -> BitcoinClientResult<Block>;
            fn get_block_hash<'life0, 'async_trait>(
                &'life0 self,
                height: u64,
            ) -> futures::future::BoxFuture<'static, BitcoinClientResult<BlockHash>>
            where
                'life0: 'async_trait,
                Self: 'async_trait;
            async fn get_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinClientResult<Block>;
            async fn get_best_block_hash(&self) -> BitcoinClientResult<BlockHash>;
            async fn get_raw_transaction_info(&self, txid: &Txid) -> BitcoinClientResult<GetRawTransactionResult>;
            async fn estimate_smart_fee(&self, conf_target: u16, estimate_mode: Option<EstimateMode>) -> BitcoinClientResult<EstimateSmartFeeResult>;
            async fn get_blockchain_info(&self) -> BitcoinRpcResult<GetBlockchainInfoResult>;
            async fn get_block_stats(&self, height: u64) -> BitcoinClientResult<GetBlockStatsResult>;
            async fn get_mempool_info(&self) -> BitcoinRpcResult<GetMempoolInfoResult>;
        }
    }

    fn get_client_with_mock(mock_bitcoin_rpc: MockBitcoinRpc) -> BitcoinClient {
        BitcoinClient {
            rpc: Arc::new(mock_bitcoin_rpc),
            config: ViaBtcClientConfig::for_tests(),
        }
    }

    #[test]
    fn confirmation_policy_rejects_an_unknown_network_instead_of_using_regtest() {
        let mut client = get_client_with_mock(MockBitcoinRpc::new());
        for (network, depth) in [
            (Network::Bitcoin, 6),
            (Network::Testnet4, 100),
            (Network::Signet, 100),
            (Network::Regtest, 1),
        ] {
            client.config.network = network.to_string();
            assert_eq!(client.reorg_safe_depth().unwrap().get(), depth);
        }
        client.config.network = "unknown".into();
        assert!(matches!(client.reorg_safe_depth(), Err(BitcoinError::InvalidNetwork(_))));
        assert!(matches!(
            BitcoinClient::new("http://127.0.0.1:1", NodeAuth::None, client.config.clone()),
            Err(BitcoinError::InvalidNetwork(_))
        ));
    }

    #[tokio::test]
    async fn canonical_header_at_retries_only_noncanonical_headers() {
        for (height, membership, expected) in [
            (
                8,
                HeaderMembership::Canonical(1.try_into().unwrap()),
                Some(HeaderValidationError::HeightMismatch),
            ),
            (7, HeaderMembership::Noncanonical, Some(HeaderValidationError::Noncanonical)),
            (7, HeaderMembership::Canonical(1.try_into().unwrap()), None),
        ] {
            let hash = BlockHash::from_byte_array([7; 32]);
            let mut rpc = MockBitcoinRpc::new();
            rpc.expect_get_block_hash().returning(move |_| Box::pin(async move { Ok(hash) }));
            rpc.expect_get_block_header().returning(move |_| {
                Ok(Some(BlockHeaderInfo {
                    hash,
                    parent_hash: BlockHash::from_byte_array([6; 32]),
                    height,
                    membership,
                }))
            });
            let result = get_client_with_mock(rpc).canonical_header_at(7).await;
            match expected {
                Some(HeaderValidationError::Noncanonical) => {
                    assert!(matches!(result, Err(BitcoinError::Rpc(_))))
                }
                Some(expected) => assert!(matches!(result,
                    Err(BitcoinError::InvalidBlockHeader { hash: actual, reason })
                        if actual == hash && reason == expected)),
                None => assert_eq!(result.unwrap().height, 7),
            }
        }
    }

    #[tokio::test]
    async fn test_get_balance() {
        let mut mock_rpc = MockBitcoinRpc::new();
        mock_rpc.expect_get_balance_scan().return_once(|_| Ok(1000000));

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
            .expect_list_unspent()
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
    async fn confirmation_maturity_checks_membership_without_downloading_body() {
        for (confirmations, canonical, expected) in
            [(6, true, true), (6, false, false), (5, true, false)]
        {
            let hash = BlockHash::from_byte_array([7; 32]);
            let mut rpc = MockBitcoinRpc::new();
            rpc.expect_get_raw_transaction_info().return_once(move |_| {
                Ok(serde_json::from_value(serde_json::json!({
                    "hex":"", "txid":Txid::all_zeros(), "hash":Wtxid::all_zeros(),
                    "size":0, "vsize":0, "version":2, "locktime":0, "vin":[], "vout":[],
                    "blockhash":hash, "confirmations":confirmations,
                }))
                .unwrap())
            });
            rpc.expect_get_block_header().returning(move |_| {
                Ok(Some(BlockHeaderInfo {
                    height: 7,
                    hash,
                    parent_hash: BlockHash::from_byte_array([6; 32]),
                    membership: if canonical {
                        HeaderMembership::Canonical(6.try_into().unwrap())
                    } else {
                        HeaderMembership::Noncanonical
                    },
                }))
            });
            let mut client = get_client_with_mock(rpc);
            client.config.network = Network::Bitcoin.to_string();
            assert_eq!(client.is_reorg_safe(&Txid::all_zeros()).await.unwrap(), expected);
        }
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
        for (mempool_min_fee, expected_rate) in [(1000, 2), (5000, 5)] {
            let mut mock_rpc = MockBitcoinRpc::new();
            mock_rpc.expect_estimate_smart_fee().return_once(|_, _| {
                Ok(EstimateSmartFeeResult {
                    fee_rate: Some(Amount::from_sat(1000)),
                    errors: None,
                    blocks: 0,
                })
            });
            mock_rpc.expect_get_mempool_info().return_once(move || {
                Ok(GetMempoolInfoResult {
                    loaded: Some(true),
                    size: 0,
                    bytes: 0,
                    usage: 0,
                    total_fee: None,
                    max_mempool: 0,
                    mempool_min_fee: Amount::from_sat(mempool_min_fee),
                    min_relay_tx_fee: Amount::ZERO,
                    incremental_relay_fee: None,
                    unbroadcast_count: None,
                    full_rbf: None,
                })
            });

            let client = get_client_with_mock(mock_rpc);
            assert_eq!(client.get_fee_rate(6).await.unwrap(), expected_rate);
        }
    }
}
