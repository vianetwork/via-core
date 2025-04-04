use zksync_config::configs::via_consensus::ViaGenesisConfig;

use crate::{envy_load, FromEnv};

impl FromEnv for ViaGenesisConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_genesis", "VIA_GENESIS_")
    }
}
