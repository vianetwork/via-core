use super::{
    via_btc_client::ViaBtcClientConfig, via_consensus::ViaGenesisConfig, via_secrets::ViaSecrets,
    ApiConfig, ObservabilityConfig, PostgresConfig, PrometheusConfig, ViaBtcWatchConfig,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ViaIndexerConfig {
    pub via_genesis_config: ViaGenesisConfig,
    pub via_btc_client_config: ViaBtcClientConfig,
    pub via_btc_watch_config: ViaBtcWatchConfig,
    pub observability_config: ObservabilityConfig,
    pub api_config: ApiConfig,
    pub prometheus_config: PrometheusConfig,
    pub postgres_config: PostgresConfig,
    pub secrets: ViaSecrets,
}
