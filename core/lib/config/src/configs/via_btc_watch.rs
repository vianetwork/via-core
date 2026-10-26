use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Configuration for the Bitcoin watch crate.
#[derive(Debug, Deserialize, Default, Serialize, Clone, PartialEq)]
pub struct ViaBtcWatchConfig {
    /// Service interval in milliseconds.
    pub poll_interval: u64,
    /// start_l1_block_number starts only independent indexer. Protocol readers start from checked bootstrap anchor.
    pub start_l1_block_number: u32,
    /// restart_indexing resets only independent indexer. Protocol readers reset through reader format check.
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
            start_l1_block_number: 1,
            restart_indexing: false,
        }
    }
}
