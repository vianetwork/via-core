use anyhow::Context;
use zksync_config::configs::{
    self,
    wallets::{AddressWallet, StateKeeper},
};
use zksync_protobuf::{required, ProtoRepr};

use crate::{parse_h160, proto, read_optional_repr};

impl ProtoRepr for proto::via_wallets::ViaWallet {
    type Type = configs::via_wallets::ViaWallet;
    fn read(&self) -> anyhow::Result<Self::Type> {
        Ok(Self::Type {
            address: self.address.clone().unwrap_or_default(),
            private_key: self.private_key.clone().unwrap_or_default(),
        })
    }

    fn build(this: &Self::Type) -> Self {
        Self {
            address: Some(this.address.clone()),
            private_key: Some(this.private_key.clone()),
        }
    }
}

impl ProtoRepr for proto::via_wallets::ViaWallets {
    type Type = configs::via_wallets::ViaWallets;
    fn read(&self) -> anyhow::Result<Self::Type> {
        let state_keeper = if let Some(fee_account) = &self.fee_account {
            let address =
                parse_h160(required(&fee_account.address).context("fee_account.address required")?)
                    .context("fee_account.address")?;
            Some(StateKeeper {
                fee_account: AddressWallet::from_address(address),
            })
        } else {
            None
        };

        Ok(Self::Type {
            state_keeper,
            btc_sender: read_optional_repr(&self.btc_sender),
            vote_operator: read_optional_repr(&self.vote_operator),
            token_multiplier_setter: None,
        })
    }

    fn build(this: &Self::Type) -> Self {
        let fee_account =
            this.state_keeper
                .as_ref()
                .map(|state_keeper| proto::wallets::AddressWallet {
                    address: Some(format!("{:?}", state_keeper.fee_account.address())),
                });

        Self {
            fee_account,
            btc_sender: this.btc_sender.as_ref().map(ProtoRepr::build),
            vote_operator: this.vote_operator.as_ref().map(ProtoRepr::build),
        }
    }
}
