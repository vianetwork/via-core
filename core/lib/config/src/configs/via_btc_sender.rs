use std::time::Duration;

use serde::{Deserialize, Serialize};

const DEFAULT_DA_LAYER: &str = "celestia";

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct ViaBtcSenderConfig {
    /// Service interval in milliseconds.
    pub poll_interval: u64,

    // Number of blocks to commit at time, should be 'one'.
    pub max_aggregated_blocks_to_commit: i32,

    // Number of proofs to commit at time, should be 'one'.
    pub max_aggregated_proofs_to_commit: i32,

    // The max number of inscription in flight
    pub max_txs_in_flight: i64,

    /// Number of block confirmations required to mark the inscription request as confirmed.
    pub block_confirmations: u32,

    /// The identifier of the DA layer.
    pub da_identifier: Option<String>,

    /// The btc sender wallet address.
    pub wallet_address: String,
}

impl ViaBtcSenderConfig {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval)
    }

    pub fn da_identifier(&self) -> String {
        self.da_identifier
            .as_ref()
            .unwrap_or(&String::from(DEFAULT_DA_LAYER))
            .clone()
    }
}

impl ViaBtcSenderConfig {
    // Creates a config object suitable for use in unit tests.
    pub fn for_tests() -> Self {
        Self {
            poll_interval: 1000,
            max_aggregated_blocks_to_commit: 1,
            max_aggregated_proofs_to_commit: 1,
            max_txs_in_flight: 1,
            block_confirmations: 0,
            da_identifier: None,
            wallet_address: "".into(),
        }
    }
}
