use async_trait::async_trait;
use bitcoin::{BlockHash, Network};

use crate::{
    traits::{BitcoinIndexerOpt, BitcoinRpc},
    types::{BitcoinClientResult, BitcoinInscriptionIndexerResult},
};

#[allow(unused)]
pub struct BitcoinInscriptionIndexer {
    pub rpc: Box<dyn BitcoinRpc>,
    network: Network,
}

#[async_trait]
impl BitcoinIndexerOpt for BitcoinInscriptionIndexer {
    async fn new() -> BitcoinClientResult<Self>
    where
        Self: Sized,
    {
        todo!()
    }

    async fn process_blocks(
        &self,
        starting_block: u128,
        ending_block: u128,
    ) -> BitcoinInscriptionIndexerResult<Vec<&str>> {
        todo!()
    }

    async fn process_block(&self, block: u128) -> BitcoinInscriptionIndexerResult<Vec<&str>> {
        todo!()
    }

    async fn are_blocks_connected(
        &self,
        parent_hash: &BlockHash,
        child_hash: &BlockHash,
    ) -> BitcoinInscriptionIndexerResult<bool> {
        todo!()
    }
}
