use zksync_config::configs::clients::CelestiaConfig;

use crate::{envy_load, FromEnv};

impl FromEnv for CelestiaConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("celestia", "CELESTIA_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvMutex;

    static MUTEX: EnvMutex = EnvMutex::new();

    fn expected_celestia_client_config() -> CelestiaConfig {
        CelestiaConfig {
            api_node_url: "localhost".parse().unwrap(),
            api_private_key: "localhost".to_string(),
        }
    }

    #[test]
    fn celestia_client_from_env() {
        let mut lock = MUTEX.lock();
        let config = r#"
            CHAIN_ETH_NETWORK="localhost"
            CHAIN_ETH_ZKSYNC_NETWORK="localhost"
        "#;
        lock.set_env(config);

        let actual = CelestiaConfig::from_env().unwrap();
        assert_eq!(actual, expected_celestia_client_config());
    }
}
