use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    time::Duration,
};

use bitcoin::{policy::MAX_STANDARD_TX_WEIGHT, Address, TapNodeHash};
use serde::{Deserialize, Serialize};
use zksync_basic_types::via_roles::ViaNodeRole;

use crate::{
    configs::{
        api::HealthCheckConfig, chain::CircuitBreakerConfig, via_bridge::ViaBridgeConfig,
        via_btc_client::ViaBtcClientConfig, via_consensus::ViaGenesisConfig,
        via_reorg_detector::ViaReorgDetectorConfig, via_secrets::ViaSecrets,
        via_wallets::ViaWallets, ObservabilityConfig, PrometheusConfig,
    },
    ObjectStoreConfig, PostgresConfig, ViaBtcSenderConfig, ViaBtcWatchConfig, ViaCelestiaConfig,
};

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

    /// The verifier btc wallet address.
    pub wallet_address: String,

    /// The bridge address merkle root.
    pub bridge_address_merkle_root: Option<String>,

    /// Transaction weight limit.
    pub max_tx_weight: Option<u64>,

    /// Enable only after withdrawal history and signer recovery have been reconciled.
    #[serde(default)]
    pub withdrawal_signing_enabled: bool,

    /// Authenticated coordinator identity; must be a configured verifier public key.
    #[serde(default)]
    pub coordinator_public_key: String,

    /// Explicit fulfillment depth, independent of the watcher ingestion cutoff.
    #[serde(default)]
    pub withdrawal_fulfillment_confirmations: Option<u32>,

    /// Approves a batch whose proof is absent, as an unverified development result, on regtest development stores only.
    /// The switch is verifier config because serving nodes rewrite the proof package's own skip flag.
    /// RISC Zero likewise enables fake receipts only through the verifier's own context, never through the receipt:
    /// https://github.com/risc0/risc0/blob/218e3bc4a8ffcd203a9cd4e46f921bf60aa7e2bd/risc0/zkvm/src/receipt.rs#L784-L792
    #[serde(default)]
    pub proof_verification_dev_mode: bool,
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
            wallet_address: "".into(),
            max_tx_weight: None,
            bridge_address_merkle_root: None,
            withdrawal_signing_enabled: false,
            coordinator_public_key: String::new(),
            withdrawal_fulfillment_confirmations: None,
            proof_verification_dev_mode: false,
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

#[derive(Debug, Clone, PartialEq)]
pub struct ViaGeneralVerifierConfig {
    pub via_bridge_config: ViaBridgeConfig,
    pub via_genesis_config: ViaGenesisConfig,
    pub via_btc_client_config: ViaBtcClientConfig,
    pub via_btc_watch_config: ViaBtcWatchConfig,
    pub via_btc_sender_config: ViaBtcSenderConfig,
    pub via_celestia_config: ViaCelestiaConfig,
    pub via_verifier_config: ViaVerifierConfig,
    pub via_reorg_detector_config: ViaReorgDetectorConfig,
    pub observability_config: ObservabilityConfig,
    pub health_check: HealthCheckConfig,
    pub prometheus_config: PrometheusConfig,
    pub postgres_config: PostgresConfig,
    pub circuit_breaker_config: CircuitBreakerConfig,
    pub core_object_store: ObjectStoreConfig,
    pub secrets: ViaSecrets,
    pub wallets: ViaWallets,
}
