use zksync_config::configs::via_celestia::ViaCelestiaConfig;

use crate::{envy_load, FromEnv};

impl FromEnv for ViaCelestiaConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_celestia_client", "VIA_CELESTIA_CLIENT_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvMutex;

    static MUTEX: EnvMutex = EnvMutex::new();

    #[test]
    fn celestia_client_from_env() {
        let mut lock = MUTEX.lock();
        let config = r#"
            VIA_CELESTIA_CLIENT_API_NODE_URL="ws://localhost:26658"
            VIA_CELESTIA_CLIENT_AUTH_TOKEN="eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJBbGxvdyI6WyJwdWJsaWMiLCJyZWFkIiwid3JpdGUiLCJhZG1pbiJdfQ.ut1X4u9XG5cbV0yaRAKfGp9xWVrz3NoEPGGRch13dFU
            VIA_CELESTIA_CLIENT_BLOB_SIZE_LIMIT=1973786
        "#;

        lock.set_env(config);

        let actual = ViaCelestiaConfig::from_env().unwrap();
        let for_tests = ViaCelestiaConfig::for_tests();

        assert_eq!(actual, for_tests);
    }
}