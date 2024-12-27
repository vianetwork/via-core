use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ViaVerifierConfig {
    /// How often the verifier should run its checks (milliseconds).
    pub poll_interval: u64,
}

impl ViaVerifierConfig {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval)
    }

    pub fn for_tests() -> Self {
        Self {
            poll_interval: 1000,
        }
    }
}
