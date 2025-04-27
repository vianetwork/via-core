use anyhow::Context;
use zksync_config::configs::{
    via_wallets::{ViaWallet, ViaWallets},
    wallets::Wallets,
};

use crate::FromEnv;

fn pk_from_env(env_var: &str, context: &str) -> anyhow::Result<Option<String>> {
    std::env::var(env_var)
        .ok()
        .map(|pk| pk.parse().context(context.to_string()))
        .transpose()
}

impl FromEnv for ViaWallets {
    fn from_env() -> anyhow::Result<Self> {
        let wallets = Wallets::from_env()?;

        let btc_sender_address = pk_from_env(
            "VIA_BTC_SENDER_WALLET_ADDRESS",
            "Malformed operator address",
        )?;
        let btc_sender_pk = pk_from_env("VIA_BTC_SENDER_PRIVATE_KEY", "Malformed operator pk")?;

        let verifier_address =
            pk_from_env("VIA_VERIFIER_WALLET_ADDRESS", "Malformed verifier address")?;
        let verifier_pk = pk_from_env("VIA_VERIFIER_PRIVATE_KEY", "Malformed verifier pk")?;

        Ok(Self {
            state_keeper: wallets.state_keeper,
            token_multiplier_setter: wallets.token_multiplier_setter,
            btc_sender: Some(ViaWallet {
                address: btc_sender_address.unwrap_or_default(),
                private_key: btc_sender_pk.clone().unwrap_or_default(),
            }),
            vote_operator: Some(ViaWallet {
                address: verifier_address.unwrap_or_default(),
                private_key: verifier_pk.unwrap_or_default(),
            }),
        })
    }
}
