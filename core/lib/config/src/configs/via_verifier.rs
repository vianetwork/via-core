use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    time::Duration,
};

use bitcoin::{policy::MAX_STANDARD_TX_WEIGHT, Address, TapNodeHash};
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

    /// The bridge address merkle root.
    pub bridge_address_merkle_root: Option<String>,

    /// The session timeout.
    pub session_timeout: u64,

    /// Transaction weight limit.
    pub max_tx_weight: Option<u64>,
}

impl ViaVerifierConfig {
    pub fn polling_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval)
    }

    pub fn wallet_address(&self) -> anyhow::Result<Address> {
        Ok(Address::from_str(&self.wallet_address)?.assume_checked())
    }
}

impl ViaVerifierConfig {
    pub fn bind_addr(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), self.coordinator_port)
    }

    pub fn max_tx_weight(&self) -> u64 {
        // Reserve 20000 weight units below Bitcoin's standard limit as a safety buffer
        // to account for witness data variations, signature size differences, and
        // potential rounding errors during transaction construction, ensuring we stay
        // well within node acceptance limits
        self.max_tx_weight
            .unwrap_or((MAX_STANDARD_TX_WEIGHT - 20000).into())
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
            bridge_address_merkle_root: None,
        }
    }

    pub fn bridge_address_merkle_root(&self) -> Option<TapNodeHash> {
        if let Some(merkle_root) = self.bridge_address_merkle_root.clone() {
            if merkle_root.is_empty() {
                return None;
            }
            return Some(TapNodeHash::from_str(&merkle_root).expect("Invalid signer merkle root"));
        }
        None
    }
}
