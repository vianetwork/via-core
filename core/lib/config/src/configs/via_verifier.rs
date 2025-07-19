use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use bitcoin::policy::MAX_STANDARD_TX_WEIGHT;
use serde::{Deserialize, Serialize};
use zksync_basic_types::via_roles::ViaNodeRole;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ViaVerifierConfig {
    /// The verifier role.
    pub role: ViaNodeRole,

    /// Service interval in milliseconds.
    pub poll_interval: u64,

    /// Port to which the coordinator server is listening.
    pub coordinator_port: u16,

    /// The coordinator url.
    pub coordinator_http_url: String,

    /// Verifier Request Timeout (in seconds)
    pub verifier_request_timeout: u8,

    /// (TEST ONLY) returns the proof verification result.
    pub test_zk_proof_invalid_l1_batch_numbers: Vec<i64>,

    /// The verifier btc wallet address.
    pub wallet_address: String,

    /// The session timeout.
    pub session_timeout: u64,

    /// Transaction weight limit.
    pub max_tx_weight: Option<u64>,
}

impl ViaVerifierConfig {
    pub fn polling_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval)
    }
}

impl ViaVerifierConfig {
    pub fn bind_addr(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), self.coordinator_port)
    }

    pub fn max_tx_weight(&self) -> u64 {
        self.max_tx_weight.unwrap_or(380000)
    }

    pub fn for_tests() -> Self {
        Self {
            role: ViaNodeRole::Verifier,
            poll_interval: 1000,
            coordinator_http_url: "http://localhost:3000".into(),
            coordinator_port: 3000,
            verifier_request_timeout: 10,
            test_zk_proof_invalid_l1_batch_numbers: vec![],
            wallet_address: "".into(),
            session_timeout: 30,
            max_tx_weight: None,
        }
    }
}
