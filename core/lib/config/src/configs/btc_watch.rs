use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Configuration for the Bitcoin watch crate.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct BtcWatchConfig {
    /// How often we want to poll the Bitcoin node.
    /// Value in milliseconds.
    pub btc_node_poll_interval: u64,

    /// URL of the Bitcoin node RPC.
    pub rpc_url: String,

    /// Network of the Bitcoin node.
    pub network: String,

    /// List of transaction IDs to bootstrap the indexer.
    pub bootstrap_txids: Vec<String>,
}

impl BtcWatchConfig {
    /// Converts `self.btc_node_poll_interval` into `Duration`.
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.btc_node_poll_interval)
    }

    /// Returns the RPC URL of the Bitcoin node.
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// Returns the network of the Bitcoin node.
    pub fn network(&self) -> &str {
        &self.network
    }

    /// Returns the list of transaction IDs to bootstrap the indexer.
    pub fn bootstrap_txids(&self) -> Vec<String> {
        self.bootstrap_txids.clone()
    }
}

impl BtcWatchConfig {
    /// Creates a mock configuration object suitable for unit tests.
    /// Values inside match the config used for localhost development.
    pub fn for_tests() -> Self {
        Self {
            btc_node_poll_interval: 1000,
            rpc_url: "http://localhost:18332".to_string(),
            network: "regtest".to_string(),
            bootstrap_txids: vec![],
        }
    }
}
