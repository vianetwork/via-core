use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Configuration for the Bitcoin watch crate.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct BtcWatchConfig {
    /// How often we want to poll the Bitcoin node.
    /// Value in milliseconds.
    pub btc_node_poll_interval: u64,
}

impl BtcWatchConfig {
    /// Converts `self.btc_node_poll_interval` into `Duration`.
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.btc_node_poll_interval)
    }
}
