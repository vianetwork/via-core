use std::str::FromStr;

use bitcoin::Network;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct ViaBtcClientConfig {
    /// Name of the used Bitcoin network
    network: String,
}

impl ViaBtcClientConfig {
    /// Returns the Bitcoin network
    pub fn network(&self) -> Network {
        Network::from_str(&self.network).unwrap_or(Network::Regtest)
    }

    pub fn rpc_url(&self, base_rpc_url: String, wallet: String) -> String {
        if self.network() == Network::Regtest {
            return base_rpc_url;
        }
        // Include the wallet endpoint to fetch the utxos.
        format!("{}/wallet/{}", base_rpc_url, wallet)
    }
}

impl ViaBtcClientConfig {
    // Creates a config object suitable for use in unit tests.
    pub fn for_tests() -> Self {
        Self {
            network: Network::Regtest.to_string(),
        }
    }
}
