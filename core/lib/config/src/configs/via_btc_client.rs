use std::str::FromStr;

use bitcoin::Network;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct ViaBtcClientConfig {
    /// Name of the used Bitcoin network
    pub network: String,
    /// External fee APIs
    pub external_apis: Vec<String>,
    /// Fee strategies
    pub fee_strategies: Vec<String>,
    /// Use external api to get the inscription fee rate.
    pub use_rpc_for_fee_rate: Option<bool>,
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
        format!("{}wallet/{}", base_rpc_url, wallet)
    }

    pub fn use_rpc_for_fee_rate(&self) -> bool {
        if let Some(use_external_api) = self.use_rpc_for_fee_rate {
            return use_external_api;
        }
        true
    }
}

impl ViaBtcClientConfig {
    // Creates a config object suitable for use in unit tests.
    pub fn for_tests() -> Self {
        Self {
            network: Network::Regtest.to_string(),
            external_apis: vec![],
            fee_strategies: vec![],
            use_rpc_for_fee_rate: Some(false),
        }
    }
}
