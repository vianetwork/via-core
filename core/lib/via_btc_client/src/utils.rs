use bitcoin::{hashes::Hash, Txid};

use crate::types;

pub fn bytes_to_txid(bytes: &[u8]) -> Result<Txid, types::IndexerError> {
    let txid = Txid::from_slice(bytes).map_err(types::IndexerError::TxIdParsingError)?;
    Ok(txid)
}
