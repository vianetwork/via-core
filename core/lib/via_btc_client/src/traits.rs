use async_trait::async_trait;
use bitcoin::{Address, Block, BlockHash, Network, OutPoint, Transaction, TxOut, Txid};
use bitcoincore_rpc::Auth;

use crate::types::{
    BitcoinClientResult, BitcoinIndexerResult, BitcoinInscriberResult, BitcoinRpcResult,
    BitcoinSignerResult, Message,
};

#[allow(dead_code)]
#[async_trait]
pub trait BitcoinOps: Send + Sync {
    async fn new(rpc_url: &str, network: Network, auth: Auth) -> BitcoinClientResult<Self>
    where
        Self: Sized;
    async fn get_balance(&self, address: &Address) -> BitcoinClientResult<u128>;
    async fn broadcast_signed_transaction(
        &self,
        // TODO: change type here
        signed_transaction: &str,
    ) -> BitcoinClientResult<Txid>;
    async fn fetch_utxos(&self, address: &Address) -> BitcoinClientResult<Vec<(TxOut, Txid, u32)>>;
    async fn check_tx_confirmation(&self, txid: &Txid, conf_num: u32) -> BitcoinClientResult<bool>;
    async fn fetch_block_height(&self) -> BitcoinClientResult<u128>;
    async fn fetch_block(&self, block_height: u128) -> BitcoinClientResult<Block>;
    async fn get_fee_rate(&self, conf_target: u16) -> BitcoinClientResult<u64>;
    fn get_rpc_client(&self) -> &dyn BitcoinRpc;
    fn get_network(&self) -> Network;
}

#[allow(dead_code)]
#[async_trait]
pub trait BitcoinRpc: Send + Sync {
    async fn get_balance(&self, address: &Address) -> BitcoinRpcResult<u64>;
    async fn send_raw_transaction(&self, tx_hex: &str) -> BitcoinRpcResult<Txid>;
    async fn list_unspent(&self, address: &Address) -> BitcoinRpcResult<Vec<OutPoint>>;
    async fn get_transaction(&self, tx_id: &Txid) -> BitcoinRpcResult<Transaction>;
    async fn get_block_count(&self) -> BitcoinRpcResult<u64>;
    async fn get_block_by_height(&self, block_height: u128) -> BitcoinRpcResult<Block>;

    async fn get_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinRpcResult<Block>;
    async fn get_best_block_hash(&self) -> BitcoinRpcResult<bitcoin::BlockHash>;
    async fn get_raw_transaction_info(
        &self,
        txid: &Txid,
    ) -> BitcoinRpcResult<bitcoincore_rpc::json::GetRawTransactionResult>;
    async fn estimate_smart_fee(
        &self,
        conf_target: u16,
        estimate_mode: Option<bitcoincore_rpc::json::EstimateMode>,
    ) -> BitcoinRpcResult<bitcoincore_rpc::json::EstimateSmartFeeResult>;
}

#[allow(dead_code)]
#[async_trait]
pub trait BitcoinSigner<'a>: Send + Sync {
    fn new(private_key: &str, rpc_client: &'a dyn BitcoinRpc) -> BitcoinSignerResult<Self>
    where
        Self: Sized;

    async fn sign_ecdsa(
        &self,
        unsigned_tx: &Transaction,
        input_index: usize,
    ) -> BitcoinSignerResult<bitcoin::Witness>;

    async fn sign_reveal(
        &self,
        unsigned_tx: &Transaction,
        input_index: usize,
        tapscript: &bitcoin::ScriptBuf,
        leaf_version: bitcoin::taproot::LeafVersion,
        control_block: &bitcoin::taproot::ControlBlock,
    ) -> BitcoinSignerResult<bitcoin::Witness>;
}
#[allow(dead_code)]
#[async_trait]
pub trait BitcoinInscriber: Send + Sync {
    async fn new(config: &str) -> BitcoinInscriberResult<Self>
    where
        Self: Sized;
    async fn inscribe(&self, message_type: &str, data: &str) -> BitcoinInscriberResult<String>;
}

#[allow(dead_code)]
#[async_trait]
pub trait BitcoinIndexerOpt: Send + Sync {
    async fn new(rpc_url: &str, network: Network, txid: &Txid) -> BitcoinIndexerResult<Self>
    where
        Self: Sized;
    async fn process_blocks(
        &self,
        starting_block: u32,
        ending_block: u32,
    ) -> BitcoinIndexerResult<Vec<Message>>;
    async fn process_block(&self, block: u32) -> BitcoinIndexerResult<Vec<Message>>;

    async fn are_blocks_connected(
        &self,
        parent_hash: &BlockHash,
        child_hash: &BlockHash,
    ) -> BitcoinIndexerResult<bool>;
}
