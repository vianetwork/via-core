use anyhow::Context;
use zksync_config::{
    configs::via_secrets::{ViaDASecrets, ViaL1Secrets},
    ViaBtcSenderConfig,
};

use crate::{envy_load, FromEnv};

impl FromEnv for ViaBtcSenderConfig {
    fn from_env() -> anyhow::Result<Self> {
        envy_load("via_btc_sender", "VIA_BTC_SENDER_")
    }
}

impl FromEnv for ViaL1Secrets {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            rpc_url: std::env::var("VIA_BTC_CLIENT_RPC_URL")
                .context("VIA_BTC_CLIENT_RPC_URL")?
                .parse()
                .context("VIA_BTC_CLIENT_RPC_URL")?,
            rpc_password: std::env::var("VIA_BTC_CLIENT_RPC_PASSWORD")
                .context("VIA_BTC_CLIENT_RPC_PASSWORD")?
                .parse()
                .context("VIA_BTC_CLIENT_RPC_PASSWORD")?,
            rpc_user: std::env::var("VIA_BTC_CLIENT_RPC_USER")
                .context("VIA_BTC_CLIENT_RPC_USER")?
                .parse()
                .context("VIA_BTC_CLIENT_RPC_USER")?,
        })
    }
}

impl FromEnv for ViaDASecrets {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            rpc_url: std::env::var("VIA_CELESTIA_CLIENT_API_NODE_URL")
                .context("VIA_CELESTIA_CLIENT_API_NODE_URL")?
                .parse()
                .context("VIA_CELESTIA_CLIENT_API_NODE_URL")?,
            auth_token: std::env::var("VIA_CELESTIA_CLIENT_AUTH_TOKEN")
                .context("VIA_CELESTIA_CLIENT_AUTH_TOKEN")?
                .parse()
                .context("VIA_CELESTIA_CLIENT_AUTH_TOKEN")?,
        })
    }
}
