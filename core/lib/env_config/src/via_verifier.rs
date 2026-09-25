use zksync_config::ViaVerifierConfig;

use crate::{envy_load, FromEnv};

impl FromEnv for ViaVerifierConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_verifier", "VIA_VERIFIER_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvMutex;

    static MUTEX: EnvMutex = EnvMutex::new();

    #[test]
    fn proof_verification_dev_mode_is_off_unless_set() {
        let mut lock = MUTEX.lock();
        lock.set_env(
            r#"
            VIA_VERIFIER_ROLE=Verifier
            VIA_VERIFIER_POLL_INTERVAL=1000
            VIA_VERIFIER_COORDINATOR_PORT=6060
            VIA_VERIFIER_COORDINATOR_HTTP_URL=http://localhost:6060
            VIA_VERIFIER_VERIFIER_REQUEST_TIMEOUT=10
            VIA_VERIFIER_WALLET_ADDRESS=bcrt1qk8mkhrmgtq24nylzyzejznfzws6d98g4kmuuh4
        "#,
        );
        assert!(
            !ViaVerifierConfig::from_env()
                .unwrap()
                .proof_verification_dev_mode
        );

        lock.set_env("VIA_VERIFIER_PROOF_VERIFICATION_DEV_MODE=true");
        assert!(
            ViaVerifierConfig::from_env()
                .unwrap()
                .proof_verification_dev_mode
        );
    }
}
