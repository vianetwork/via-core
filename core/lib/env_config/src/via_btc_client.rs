use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

use crate::{envy_load, FromEnv};

impl FromEnv for ViaBtcClientConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_btc_client", "VIA_BTC_CLIENT_")
    }
}
