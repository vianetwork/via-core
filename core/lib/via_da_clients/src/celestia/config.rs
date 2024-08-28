use zksync_config::ViaCelestiaConfig;
use zksync_env_config::FromEnv;

#[derive(Debug)]
pub struct ViaCelestiaConf(pub ViaCelestiaConfig);

impl ViaCelestiaConf {
    pub fn from_env() -> anyhow::Result<Self> {
        let config = ViaCelestiaConfig::from_env()?;

        Ok(Self(config))
    }
}
