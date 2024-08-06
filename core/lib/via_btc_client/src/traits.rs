use async_trait::async_trait;
use bitcoin::{Address, Block, BlockHash, OutPoint, Transaction, TxOut, Txid};

use crate::types;

#[allow(dead_code)]
#[async_trait]
pub trait BitcoinOps: Send + Sync {
    async fn new(rpc_url: &str, network: &str) -> types::BitcoinClientResult<Self>
    where
        Self: Sized;
    async fn get_balance(&self, address: &Address) -> types::BitcoinClientResult<u128>;
    async fn broadcast_signed_transaction(
        &self,
        // TODO: change type here
        signed_transaction: &str,
    ) -> types::BitcoinClientResult<Txid>;
    async fn fetch_utxos(
        &self,
        address: &Address,
    ) -> types::BitcoinClientResult<Vec<(TxOut, Txid, u32)>>;
    async fn check_tx_confirmation(
        &self,
        txid: &Txid,
        conf_num: u32,
    ) -> types::BitcoinClientResult<bool>;
    async fn fetch_block_height(&self) -> types::BitcoinClientResult<u128>;
    async fn fetch_and_parse_block(&self, block_height: u128)
        -> types::BitcoinClientResult<String>;
    async fn get_fee_rate(&self, conf_target: u16) -> types::BitcoinClientResult<u64>;
    fn get_rpc_client(&self) -> &dyn BitcoinRpc;
}

#[allow(dead_code)]
#[async_trait]
pub trait BitcoinRpc: Send + Sync {
    async fn get_balance(&self, address: &Address) -> types::BitcoinRpcResult<u64>;
    async fn send_raw_transaction(&self, tx_hex: &str) -> types::BitcoinRpcResult<Txid>;
    async fn list_unspent(&self, address: &Address) -> types::BitcoinRpcResult<Vec<OutPoint>>;
    async fn get_transaction(&self, tx_id: &Txid) -> types::BitcoinRpcResult<Transaction>;
    async fn get_block_count(&self) -> types::BitcoinRpcResult<u64>;
    async fn get_block(&self, block_height: u128) -> types::BitcoinRpcResult<Block>;
    async fn get_best_block_hash(&self) -> types::BitcoinRpcResult<bitcoin::BlockHash>;
    async fn get_raw_transaction_info(
        &self,
        txid: &Txid,
        // block_hash: Option<&bitcoin::BlockHash>,
    ) -> types::BitcoinRpcResult<bitcoincore_rpc::json::GetRawTransactionResult>;
    async fn estimate_smart_fee(
        &self,
        conf_target: u16,
        estimate_mode: Option<bitcoincore_rpc::json::EstimateMode>,
    ) -> types::BitcoinRpcResult<bitcoincore_rpc::json::EstimateSmartFeeResult>;
}

#[allow(dead_code)]
#[async_trait]
pub trait BitcoinSigner<'a>: Send + Sync {
    fn new(private_key: &str, rpc_client: &'a dyn BitcoinRpc) -> types::BitcoinSignerResult<Self>
    where
        Self: Sized;

    async fn sign_ecdsa(
        &self,
        unsigned_tx: &Transaction,
        input_index: usize,
    ) -> types::BitcoinSignerResult<bitcoin::Witness>;

    async fn sign_reveal(
        &self,
        unsigned_tx: &Transaction,
        input_index: usize,
        tapscript: &bitcoin::ScriptBuf,
        leaf_version: bitcoin::taproot::LeafVersion,
        control_block: &bitcoin::taproot::ControlBlock,
    ) -> types::BitcoinSignerResult<bitcoin::Witness>;
}
#[allow(dead_code)]
#[async_trait]
pub trait BitcoinInscriber: Send + Sync {
    async fn new(config: &str) -> types::BitcoinInscriberResult<Self>
    where
        Self: Sized;
    async fn inscribe(
        &self,
        message_type: &str,
        data: &str,
    ) -> types::BitcoinInscriberResult<String>;
}

#[allow(dead_code)]
#[async_trait]
pub trait BitcoinIndexerOpt: Send + Sync {
    async fn new() -> types::BitcoinInscriptionIndexerResult<Self>
    where
        Self: Sized;
    async fn process_blocks(
        &self,
        starting_block: u128,
        ending_block: u128,
    ) -> types::BitcoinInscriptionIndexerResult<Vec<&str>>;
    async fn process_block(&self, block: u128)
        -> types::BitcoinInscriptionIndexerResult<Vec<&str>>;

    async fn are_blocks_connected(
        &self,
        parent_hash: &BlockHash,
        child_hash: &BlockHash,
    ) -> types::BitcoinInscriptionIndexerResult<bool>;
}

#[allow(dead_code)]
#[async_trait]
pub trait BitcoinWithdrawalTransactionBuilder: Send + Sync {
    async fn new(config: &str) -> types::BitcoinTransactionBuilderResult<Self>
    where
        Self: Sized;
    async fn build_withdrawal_transaction(
        &self,
        address: &str,
        amount: u128,
    ) -> types::BitcoinTransactionBuilderResult<String>;
}
