#![allow(dead_code)]

use async_trait::async_trait;
use bitcoin::{
    key::UntweakedPublicKey,
    secp256k1::{All, Secp256k1},
    Address, Block, BlockHash, Network, OutPoint, ScriptBuf, Transaction, TxOut, Txid,
};
use bitcoincore_rpc::bitcoincore_rpc_json::GetBlockchainInfoResult;
use secp256k1::{
    ecdsa::Signature as ECDSASignature, schnorr::Signature as SchnorrSignature, Message, PublicKey,
};
use types::BitcoinRpcResult;

use crate::{types, types::BitcoinClientResult};

#[async_trait]
pub trait BitcoinOps: Send + Sync {
    async fn get_balance(&self, address: &Address) -> BitcoinClientResult<u128>;
    async fn broadcast_signed_transaction(
        &self,
        signed_transaction: &str,
    ) -> types::BitcoinClientResult<Txid>;
    async fn fetch_utxos(
        &self,
        address: &Address,
    ) -> types::BitcoinClientResult<Vec<(OutPoint, TxOut)>>;
    async fn check_tx_confirmation(
        &self,
        txid: &Txid,
        conf_num: u32,
    ) -> types::BitcoinClientResult<bool>;
    async fn fetch_block_height(&self) -> types::BitcoinClientResult<u128>;
    async fn get_fee_rate(&self, conf_target: u16) -> types::BitcoinClientResult<u64>;
    fn get_network(&self) -> Network;
    async fn fetch_block(&self, block_height: u128) -> BitcoinClientResult<Block>;

    async fn get_transaction(&self, txid: &Txid) -> BitcoinClientResult<Transaction>;
    async fn fetch_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinClientResult<Block>;
    async fn get_fee_history(
        &self,
        from_block_height: usize,
        to_block_height: usize,
    ) -> BitcoinClientResult<Vec<u64>>;
    async fn calculate_tx_fee_per_byte(
        &self,
        block_height: u128,
        tx: Transaction,
    ) -> BitcoinClientResult<(u128, u64)>;
}

impl std::fmt::Debug for dyn BitcoinOps + 'static {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitcoinOps").finish()
    }
}

#[async_trait]
pub trait BitcoinRpc: Send + Sync {
    async fn get_balance(&self, address: &Address) -> BitcoinRpcResult<u64>;
    async fn get_balance_scan(&self, address: &Address) -> BitcoinRpcResult<u64>;
    async fn send_raw_transaction(&self, tx_hex: &str) -> BitcoinRpcResult<Txid>;
    async fn list_unspent_based_on_node_wallet(
        &self,
        address: &Address,
    ) -> BitcoinRpcResult<Vec<OutPoint>>;
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
    async fn get_blockchain_info(&self) -> BitcoinRpcResult<GetBlockchainInfoResult>;
}

pub(crate) trait BitcoinSigner: Send + Sync {
    fn sign_ecdsa(&self, msg: Message) -> types::BitcoinSignerResult<ECDSASignature>;

    fn sign_schnorr(&self, msg: Message) -> types::BitcoinSignerResult<SchnorrSignature>;

    fn get_p2wpkh_address(&self) -> types::BitcoinSignerResult<Address>;

    fn get_p2wpkh_script_pubkey(&self) -> &ScriptBuf;

    fn get_secp_ref(&self) -> &Secp256k1<All>;

    fn get_internal_key(&self) -> types::BitcoinSignerResult<UntweakedPublicKey>;

    fn get_public_key(&self) -> PublicKey;
}

impl std::fmt::Debug for dyn BitcoinSigner + 'static {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitcoinSigner").finish()
    }
}

pub trait Serializable {
    fn to_bytes(&self) -> Vec<u8>;
    fn from_bytes(bytes: &[u8]) -> Self
    where
        Self: Sized;
}
