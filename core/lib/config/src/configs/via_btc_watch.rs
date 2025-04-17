use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Total L1 blocks to process at a time.
pub const L1_BLOCKS_CHUNK: u32 = 10;

/// Configuration for the Bitcoin watch crate.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct ViaBtcWatchConfig {
    /// Service interval in milliseconds.
    pub poll_interval: u64,

    /// Minimum confirmation blocks for an inscription to be processed.
    pub block_confirmations: u64,

    /// The starting L1 block number from which indexing begins
    pub start_l1_block_number: u32,

    /// When set to true, the btc_watch starts indexing L1 blocks from the "start_l1_block_number".
    pub restart_indexing: bool,
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
            start_l1_block_number: 1,
            restart_indexing: false,
        }
    }
}
