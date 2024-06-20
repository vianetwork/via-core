use zksync_config::configs::clients::CelestiaConfig;

use crate::{envy_load, FromEnv};

impl FromEnv for CelestiaConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("celestia_client", "CELESTIA_CLIENT_")
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
            CELESTIA_CLIENT_API_NODE_URL="localhost:26658"
            CELESTIA_CLIENT_PRIVATE_KEY="0xf55baf7c0e4e33b1d78fbf52f069c426bc36cff1aceb9bc8f45d14c07f034d73"
            CELESTIA_CLIENT_AUTH_TOKEN="eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJBbGxvdyI6WyJwdWJsaWMiLCJyZWFkIiwid3JpdGUiLCJhZG1pbiJdfQ.ut1X4u9XG5cbV0yaRAKfGp9xWVrz3NoEPGGRch13dFU
            NODE_STORE=$HOME/.celestia-light-arabica-11"
        "#;

        lock.set_env(config);

        let actual = CelestiaConfig::from_env().unwrap();
        let for_tests = CelestiaConfig::for_tests();

        assert_eq!(actual, for_tests);
    }
}
