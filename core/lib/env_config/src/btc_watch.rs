use zksync_config::BtcWatchConfig;

use crate::{envy_load, FromEnv};

impl FromEnv for BtcWatchConfig {
    fn from_env() -> anyhow::Result<Self> {
        // TODO: or the prefix should be VIA_BTC_WATCH_?
        envy_load("btc_watch", "BTC_WATCH_")
    }
}
