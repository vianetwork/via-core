use zksync_config::configs::via_reorg_detector::ViaReorgDetectorConfig;

use crate::{envy_load, FromEnv};

impl FromEnv for ViaReorgDetectorConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_reorg_detector", "VIA_REORG_DETECTOR_")
    }
}
