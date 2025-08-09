use std::str::FromStr;

use bitcoin::Txid;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ViaGenesisConfig {
    /// List of transaction IDs to bootstrap the indexer.
    pub bootstrap_txids: Vec<String>,
}

impl ViaGenesisConfig {
    /// Get the bootstrap transaction IDs.
    pub fn bootstrap_txids(&self) -> anyhow::Result<Vec<Txid>> {
        self.bootstrap_txids
            .iter()
            .map(|txid| Txid::from_str(txid).map_err(anyhow::Error::from))
            .collect()
    }
}
