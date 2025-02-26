use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum VerifierMode {
    VERIFIER = 0,
    COORDINATOR = 1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ViaVerifierConfig {
    /// Interval between polling db for verification requests (in ms).
    pub poll_interval: u64,
    /// Coordinator server port.
    pub port: u16,
    /// Coordinator server url.
    pub url: String,
    /// The signer private key.
    pub private_key: String,
    /// The verifiers public keys.
    pub verifiers_pub_keys_str: Vec<String>,
    /// The bridge address.
    pub bridge_address_str: String,
    /// The minimum required signers.
    pub required_signers: usize,
    /// Verifier Request Timeout (in seconds)
    pub verifier_request_timeout: u8,
    /// The role.
    pub verifier_mode: VerifierMode,
}

impl ViaVerifierConfig {
    pub fn polling_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval)
    }
    pub fn bind_addr(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), self.port)
    }
}

impl ViaVerifierConfig {
    pub fn for_tests() -> Self {
        Self {
            private_key: "private".to_string(),
            poll_interval: 1000,
            port: 0,
            url: "".to_string(),
            verifiers_pub_keys_str: Vec::new(),
            bridge_address_str: "".to_string(),
            required_signers: 2,
            verifier_request_timeout: 10,
            verifier_mode: VerifierMode::VERIFIER,
        }
    }
}
