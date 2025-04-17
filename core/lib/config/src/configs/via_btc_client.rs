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
}

impl ViaBtcClientConfig {
    // Creates a config object suitable for use in unit tests.
    pub fn for_tests() -> Self {
        Self {
            network: Network::Regtest.to_string(),
        }
    }
}
