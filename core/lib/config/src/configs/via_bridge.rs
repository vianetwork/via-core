use std::str::FromStr;

use bitcoin::Address;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Default, Deserialize, Clone, PartialEq)]
pub struct ViaBridgeConfig {
    /// The verifiers public keys.
    pub verifiers_pub_keys: Vec<String>,

    /// The bridge address.
    pub bridge_address: String,

    /// The agreement threshold required for the verifier to finalize an L1 batch.
    pub zk_agreement_threshold: f64,
}

impl ViaBridgeConfig {
    pub fn bridge_address(&self) -> anyhow::Result<Address> {
        Ok(Address::from_str(&self.bridge_address)
            .expect("Invalid bridge address")
            .assume_checked())
    }
}
