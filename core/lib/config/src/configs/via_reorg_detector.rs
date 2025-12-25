use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Default, Serialize, Clone, PartialEq)]
pub struct ViaReorgDetectorConfig {
    /// Service interval in milliseconds.
    pub poll_interval_ms: u64,
}

impl ViaReorgDetectorConfig {
    /// Converts `self.poll_interval` into `Duration`.
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms)
    }

    /// The number of blocks to process per iteration.
    pub fn block_limit(&self) -> i64 {
        50
    }

    /// The reorg window the reorg detector will use to check for reorgs.
    pub fn reorg_window(&self) -> i64 {
        100
    }

    /// Maximum number of concurrent block fetches.
    pub fn max_concurrent_fetches(&self) -> usize {
        10
    }
}

impl ViaReorgDetectorConfig {
    pub fn for_tests() -> Self {
        Self {
            poll_interval_ms: 1000,
        }
    }
}
