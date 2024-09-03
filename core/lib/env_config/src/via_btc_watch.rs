use zksync_config::ViaBtcWatchConfig;

use crate::{envy_load, FromEnv};

impl FromEnv for ViaBtcWatchConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_btc_watch", "VIA_BTC_WATCH_")
    }
}
