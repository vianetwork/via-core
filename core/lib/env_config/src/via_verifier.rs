use zksync_config::ViaVerifierConfig;

use crate::{envy_load, FromEnv};

impl FromEnv for ViaVerifierConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_verifier", "VIA_VERIFIER_")
    }
}
