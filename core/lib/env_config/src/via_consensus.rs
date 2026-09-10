use std::env;

use anyhow::Context as _;
use serde::Deserialize;
use zksync_config::configs::via_consensus::{ViaBootstrapTxLocatorConfig, ViaGenesisConfig};

use crate::{envy_load, FromEnv};

const BOOTSTRAP_TX_LOCATORS_ENV: &str = "VIA_GENESIS_BOOTSTRAP_TX_LOCATORS";

#[derive(Debug, Deserialize)]
struct ViaGenesisEnvConfig {
    bootstrap_txids: Vec<String>,
}

impl FromEnv for ViaGenesisConfig {
    fn from_env() -> anyhow::Result<Self> {
        let config: ViaGenesisEnvConfig = envy_load("via_genesis", "VIA_GENESIS_")?;
        let bootstrap_tx_locators = load_bootstrap_tx_locators_from_env()?;

        Ok(Self {
            bootstrap_txids: config.bootstrap_txids,
            bootstrap_tx_locators,
        })
    }
}

fn load_bootstrap_tx_locators_from_env() -> anyhow::Result<Vec<ViaBootstrapTxLocatorConfig>> {
    match env::var(BOOTSTRAP_TX_LOCATORS_ENV) {
        Ok(raw_locators) if raw_locators.trim().is_empty() => Ok(vec![]),
        Ok(raw_locators) => serde_json::from_str(raw_locators.trim()).with_context(|| {
            format!(
                "{BOOTSTRAP_TX_LOCATORS_ENV} must be a JSON array of objects with txid, block_hash, and block_height"
            )
        }),
        Err(env::VarError::NotPresent) => Ok(vec![]),
        Err(err) => Err(err).with_context(|| format!("Cannot read {BOOTSTRAP_TX_LOCATORS_ENV}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvMutex;

    static MUTEX: EnvMutex = EnvMutex::new();

    #[test]
    fn from_env_loads_bootstrap_txids_without_locators() {
        let mut lock = MUTEX.lock();
        lock.set_env(
            "VIA_GENESIS_BOOTSTRAP_TXIDS=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa,bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );
        lock.remove_env(&[BOOTSTRAP_TX_LOCATORS_ENV]);

        let config = ViaGenesisConfig::from_env().unwrap();

        assert_eq!(
            config.bootstrap_txids,
            vec![
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            ]
        );
        assert!(config.bootstrap_tx_locators.is_empty());
    }

    #[test]
    fn from_env_requires_bootstrap_txids() {
        let mut lock = MUTEX.lock();
        lock.remove_env(&["VIA_GENESIS_BOOTSTRAP_TXIDS"]);
        lock.set_env(
            r#"VIA_GENESIS_BOOTSTRAP_TX_LOCATORS=[{"txid":"c89e5db75e74700582d106f1c0aa85f7b0df1436cecd3a6536f11bed9db0f407","block_hash":"000000000000000078ec39ca5db000fca155a66d8bc55326ac6e05dec025aacd","block_height":114552}]"#,
        );

        let err = ViaGenesisConfig::from_env().unwrap_err();

        let err = format!("{err:#}");
        assert!(err.contains("bootstrap_txids"), "{err}");
    }

    #[test]
    fn from_env_loads_bootstrap_tx_locators_from_json() {
        let mut lock = MUTEX.lock();
        lock.set_env(
            r#"VIA_GENESIS_BOOTSTRAP_TXIDS=c89e5db75e74700582d106f1c0aa85f7b0df1436cecd3a6536f11bed9db0f407
VIA_GENESIS_BOOTSTRAP_TX_LOCATORS=[{"txid":"c89e5db75e74700582d106f1c0aa85f7b0df1436cecd3a6536f11bed9db0f407","block_hash":"000000000000000078ec39ca5db000fca155a66d8bc55326ac6e05dec025aacd","block_height":114552}]"#,
        );

        let config = ViaGenesisConfig::from_env().unwrap();

        assert_eq!(config.bootstrap_tx_locators.len(), 1);
        assert_eq!(
            config.bootstrap_tx_locators[0],
            ViaBootstrapTxLocatorConfig {
                txid: "c89e5db75e74700582d106f1c0aa85f7b0df1436cecd3a6536f11bed9db0f407".to_owned(),
                block_hash: "000000000000000078ec39ca5db000fca155a66d8bc55326ac6e05dec025aacd"
                    .to_owned(),
                block_height: 114552,
            }
        );
    }

    #[test]
    fn from_env_rejects_malformed_bootstrap_tx_locators() {
        let mut lock = MUTEX.lock();
        lock.set_env(
            "VIA_GENESIS_BOOTSTRAP_TXIDS=c89e5db75e74700582d106f1c0aa85f7b0df1436cecd3a6536f11bed9db0f407
VIA_GENESIS_BOOTSTRAP_TX_LOCATORS=not-json",
        );

        let err = ViaGenesisConfig::from_env().unwrap_err();

        assert!(
            err.to_string().contains(BOOTSTRAP_TX_LOCATORS_ENV),
            "{err:#}"
        );
    }
}
