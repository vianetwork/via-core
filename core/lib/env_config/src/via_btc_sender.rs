use zksync_config::{
    configs::via_secrets::{ViaDASecrets, ViaL1Secrets, ViaL2Secrets},
    ViaBtcSenderConfig,
};

use crate::{envy_load, FromEnv};

impl FromEnv for ViaBtcSenderConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_btc_sender", "VIA_BTC_SENDER_")
    }
}

impl FromEnv for ViaL1Secrets {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_l1_secrets", "VIA_BTC_CLIENT_")
    }
}

impl FromEnv for ViaL2Secrets {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_l2_secrets", "VIA_L2_CLIENT_")
    }
}

impl FromEnv for ViaDASecrets {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_da_secrets", "VIA_CELESTIA_CLIENT_")
    }
}
