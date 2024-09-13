use zksync_config::ViaBtcSenderConfig;

use crate::{envy_load, FromEnv};

impl FromEnv for ViaBtcSenderConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_btc_sender", "VIA_BTC_SENDER_")
    }
}
