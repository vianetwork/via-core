//! One hash-retaining chain model for both reader adapters. Transport barriers remain test-local.
// Chain adapts rust-lightning's competing-fork and missing-data fixture behaviour to existing JSON-RPC server: https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/test_utils.rs

use std::collections::{BTreeMap, HashMap, HashSet};

use bitcoin::{
    blockdata::constants::genesis_block, consensus::encode::serialize_hex, hashes::Hash, Block,
    BlockHash, Network, Transaction, TxMerkleNode,
};
use serde_json::{json, Value};

use crate::bitcoin_rpc::RpcError;

#[derive(Default)]
pub struct Chain {
    pub blocks: BTreeMap<u32, Block>,
    pub transactions: HashMap<String, Transaction>,
    retained: HashMap<String, (u32, Block)>,
    pub missing_headers: HashSet<BlockHash>,
    pub missing_bodies: HashSet<BlockHash>,
    pub unavailable: bool,
}

impl Chain {
    pub fn insert(&mut self, height: u32, mut block: Block) -> BlockHash {
        block.header.prev_blockhash = height
            .checked_sub(1)
            .and_then(|parent| self.blocks.get(&parent))
            .map(Block::block_hash)
            .unwrap_or_else(BlockHash::all_zeros);
        block.header.merkle_root =
            block.compute_merkle_root().unwrap_or_else(TxMerkleNode::all_zeros);
        let hash = block.block_hash();
        self.retained.insert(hash.to_string(), (height, block.clone()));
        self.blocks.retain(|&stored, _| stored <= height);
        self.blocks.insert(height, block);
        hash
    }

    pub fn with_height(height: u32) -> Self {
        let mut chain = Self::default();
        chain.blocks.insert(0, genesis_block(Network::Regtest));
        for number in 1..=height {
            let mut block = genesis_block(Network::Regtest);
            block.header.nonce = number;
            chain.insert(number, block);
        }
        chain
    }

    pub fn fork_at_height(&mut self, height: u32) {
        let Some(first) = height.checked_add(1) else {
            return;
        };
        let replacements = self
            .blocks
            .range(first..)
            .map(|(&height, block)| (height, block.clone()))
            .collect::<Vec<_>>();
        for (height, mut block) in replacements {
            block.header.nonce = block.header.nonce.wrapping_add(1);
            self.insert(height, block);
        }
    }

    fn by_hash(&self, hash: &str) -> Option<(u32, &Block, bool)> {
        self.blocks
            .iter()
            .find(|(_, block)| block.block_hash().to_string() == hash)
            .map(|(&height, block)| (height, block, true))
            .or_else(|| self.retained.get(hash).map(|(height, block)| (*height, block, false)))
    }

    pub fn rpc(&self, request: &Value) -> Result<Value, RpcError> {
        let error = |code, message: &str| RpcError { code, message: message.into() };
        if self.unavailable {
            return Err(error(-28, "fixture source unavailable"));
        }
        let tip =
            *self.blocks.keys().next_back().ok_or_else(|| error(-28, "fixture has no tip"))?;
        let hash_arg = || request["params"][0].as_str().ok_or_else(|| error(-8, "hash required"));
        match request["method"].as_str() {
            Some("getblockcount") => Ok(json!(tip)),
            Some("getbestblockhash") => Ok(json!(self.blocks[&tip].block_hash())),
            Some("getblockhash") => {
                let height = request["params"][0]
                    .as_u64()
                    .and_then(|height| u32::try_from(height).ok())
                    .ok_or_else(|| error(-8, "invalid height"))?;
                self.blocks
                    .get(&height)
                    .map(|block| json!(block.block_hash()))
                    .ok_or_else(|| error(-8, "block height out of range"))
            }
            Some("getblock") | Some("getblockheader") => {
                let (height, block, canonical) =
                    self.by_hash(hash_arg()?).ok_or_else(|| error(-5, "block not found"))?;
                let hash = block.block_hash();
                if request["method"] == "getblock" {
                    if self.missing_bodies.contains(&hash) {
                        return Err(error(-5, "body unavailable"));
                    }
                    return Ok(json!(serialize_hex(block)));
                }
                if self.missing_headers.contains(&hash) {
                    return Err(error(-5, "header unavailable"));
                }
                Ok(json!({
                    "hash": hash, "confirmations": if canonical { i64::from(tip) - i64::from(height) + 1 } else { -1 },
                    "height":height,"version":block.header.version.to_consensus(),"merkleroot":block.header.merkle_root,
                    "time":block.header.time,"nonce":block.header.nonce,"bits":format!("{:08x}",block.header.bits.to_consensus()),
                    "difficulty":1.0,"chainwork":"00","nTx":block.txdata.len(),
                    "previousblockhash": if height == 0 { None } else { Some(block.header.prev_blockhash) },
                }))
            }
            Some("getrawtransaction") => {
                let hash = hash_arg()?;
                let tx = self
                    .transactions
                    .get(hash)
                    .or_else(|| {
                        self.blocks
                            .values()
                            .flat_map(|block| &block.txdata)
                            .find(|tx| tx.compute_txid().to_string() == hash)
                    })
                    .ok_or_else(|| error(-5, "transaction not found"))?;
                Ok(json!(serialize_hex(tx)))
            }
            _ => Err(error(-32601, "unsupported fixture method")),
        }
    }
}
