use async_trait::async_trait;
use bitcoin::{script::Instruction, BlockHash, Transaction};

use crate::{
    traits::BitcoinIndexerOpt,
    types::{BitcoinIndexerResult, BitcoinMessage},
    BitcoinOps,
};

pub struct BitcoinInscriptionIndexer {
    client: Box<dyn BitcoinOps>,
}

#[async_trait]
impl BitcoinIndexerOpt for BitcoinInscriptionIndexer {
    async fn new() -> Self
    where
        Self: Sized,
    {
        todo!()
    }

    async fn process_blocks(
        &self,
        starting_block: u128,
        ending_block: u128,
    ) -> BitcoinIndexerResult<Vec<BitcoinMessage>> {
        let mut res = Vec::with_capacity((ending_block - starting_block + 1) as usize);
        for block in starting_block..=ending_block {
            res.extend(self.process_block(block).await?);
        }
        Ok(res)
    }

    async fn process_block(&self, block: u128) -> BitcoinIndexerResult<Vec<BitcoinMessage>> {
        let block = self
            .client
            .get_rpc_client()
            .get_block_by_height(block)
            .await?;
        let res: Vec<_> = block
            .txdata
            .iter()
            .filter_map(|tx| self.process_tx(tx))
            .flatten()
            .collect();
        Ok(res)
    }

    async fn are_blocks_connected(
        &self,
        parent_hash: &BlockHash,
        child_hash: &BlockHash,
    ) -> BitcoinIndexerResult<bool> {
        let child_block = self
            .client
            .get_rpc_client()
            .get_block_by_hash(child_hash)
            .await?;
        Ok(child_block.header.prev_blockhash == *parent_hash)
    }
}

impl BitcoinInscriptionIndexer {
    fn process_tx(&self, tx: &Transaction) -> Option<Vec<BitcoinMessage>> {
        // only first?
        let witness = tx.input.get(0)?.witness.to_vec();
        let script = bitcoin::Script::from_bytes(witness.last()?);
        let instructions: Vec<_> = script.instructions().filter_map(Result::ok).collect();

        let via_index = match is_via_inscription_protocol(&instructions) {
            Some(pos) => pos,
            None => return None,
        };
        // TODO: collect and parse
        let _msg_type = instructions.get(via_index + 1);
        None
    }
}

fn is_via_inscription_protocol(instructions: &[Instruction]) -> Option<usize> {
    instructions.iter().position(|instr| {
        matches!(instr, Instruction::PushBytes(bytes) if bytes.as_bytes() == b"Str('via_inscription_protocol')")
    })
}
