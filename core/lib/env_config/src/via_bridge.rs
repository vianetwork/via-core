use zksync_config::configs::via_bridge::ViaBridgeConfig;

use crate::{envy_load, FromEnv};

impl FromEnv for ViaBridgeConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_bridge", "VIA_BRIDGE_")
    }
}
