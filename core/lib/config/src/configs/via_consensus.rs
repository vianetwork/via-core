use std::str::FromStr;

use bitcoin::{Address, Txid};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ViaGenesisConfig {
    /// The coordinator public key.
    pub coordinator_pub_key: String,

    /// The verifiers public keys.
    pub verifiers_pub_keys: Vec<String>,

    /// The bridge address.
    pub bridge_address: String,

    /// The minimum required signers to process a musig2 session.
    pub required_signers: usize,

    /// The agreement threshold required for the verifier to finalize an L1 batch.
    pub zk_agreement_threshold: f64,

    /// List of transaction IDs to bootstrap the indexer.
    pub bootstrap_txids: Vec<String>,
}

impl ViaGenesisConfig {
    pub fn bridge_address(&self) -> anyhow::Result<Address> {
        Ok(Address::from_str(&self.bridge_address)
            .expect("Invalid bridge address")
            .assume_checked())
    }

    pub fn bootstrap_txids(&self) -> anyhow::Result<Vec<Txid>> {
        self.bootstrap_txids
            .iter()
            .map(|txid| Txid::from_str(txid).map_err(anyhow::Error::from))
            .collect()
    }
}
