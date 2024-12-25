use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct ViaVerifierConfig {
    /// Max time of a single compilation (in s).
    pub compilation_timeout: u64,
    /// Interval between polling db for verification requests (in ms).
    pub polling_interval: Option<u64>,
    /// Port to which the Prometheus exporter server is listening.
    pub prometheus_port: u16,
    pub threads_per_server: Option<u16>,
    pub port: u16,
    pub url: String,
    pub private_key: String,
    pub verifiers_pub_keys_str: Vec<String>,
    pub bridge_address_str: String,
    pub required_signers: usize,
}

impl ViaVerifierConfig {
    pub fn compilation_timeout(&self) -> Duration {
        Duration::from_secs(self.compilation_timeout)
    }

    pub fn polling_interval(&self) -> Duration {
        Duration::from_millis(self.polling_interval.unwrap_or(1000))
    }
    pub fn bind_addr(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), self.port)
    }
}

impl ViaVerifierConfig {
    pub fn for_tests() -> Self {
        Self {
            private_key: "private".to_string(),
            compilation_timeout: 0,
            polling_interval: None,
            prometheus_port: 0,
            threads_per_server: None,
            port: 0,
            url: "".to_string(),
            verifiers_pub_keys_str: Vec::new(),
            bridge_address_str: "".to_string(),
            required_signers: 2,
        }
    }
}
