use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Configuration for the Bitcoin watch crate.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct ViaBtcWatchConfig {
    /// Service interval in milliseconds.
    pub poll_interval: u64,

    /// Minimum confirmation blocks for an inscription to be processed.
    pub block_confirmations: u64,

    /// Number of blocks that we should wait before processing the new blocks.
    pub btc_blocks_lag: u32,
}

impl ViaBtcWatchConfig {
    /// Converts `self.poll_interval` into `Duration`.
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval)
    }
}

impl ViaBtcWatchConfig {
    /// Creates a mock configuration object suitable for unit tests.
    /// Values inside match the config used for localhost development.
    pub fn for_tests() -> Self {
        Self {
            poll_interval: 1000,
            block_confirmations: 0,
            btc_blocks_lag: 1,
        }
    }
}
