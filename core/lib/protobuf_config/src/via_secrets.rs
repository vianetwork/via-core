use std::str::FromStr;

use anyhow::Context;
use zksync_basic_types::url::SensitiveUrl;
use zksync_config::configs::via_secrets::{ViaDASecrets, ViaL1Secrets, ViaSecrets};
use zksync_protobuf::{required, ProtoRepr};

use crate::{
    proto::{
        secrets::Secrets,
        via_secrets::{self as proto},
    },
    read_optional_repr,
};

impl ProtoRepr for proto::ViaSecrets {
    type Type = ViaSecrets;

    fn read(&self) -> anyhow::Result<Self::Type> {
        Ok(Self::Type {
            base_secrets: read_optional_repr(&self.base_secrets).unwrap(),
            via_da: read_optional_repr(&self.via_da),
            via_l1: read_optional_repr(&self.via_l1),
        })
    }

    fn build(this: &Self::Type) -> Self {
        Self {
            base_secrets: Some(Secrets::build(&this.base_secrets)),
            via_da: this.via_da.as_ref().map(ProtoRepr::build),
            via_l1: this.via_l1.as_ref().map(ProtoRepr::build),
        }
    }
}

impl ProtoRepr for proto::ViaL1Secrets {
    type Type = ViaL1Secrets;

    fn read(&self) -> anyhow::Result<Self::Type> {
        Ok(Self::Type {
            rpc_url: SensitiveUrl::from_str(required(&self.rpc_url).context("rpc_url")?)?,
            rpc_password: self.rpc_password.clone().unwrap(),
            rpc_user: self.rpc_user.clone().unwrap(),
        })
    }

    fn build(this: &Self::Type) -> Self {
        Self {
            rpc_url: Some(this.rpc_url.expose_str().to_string()),
            rpc_password: Some(this.rpc_password.clone()),
            rpc_user: Some(this.rpc_user.clone()),
        }
    }
}

impl ProtoRepr for proto::ViaDaSecrets {
    type Type = ViaDASecrets;

    fn read(&self) -> anyhow::Result<Self::Type> {
        Ok(Self::Type {
            api_node_url: SensitiveUrl::from_str(
                required(&self.api_node_url).context("api_node_url")?,
            )?,
            auth_token: self.auth_token.clone().expect("auth_token"),
        })
    }

    fn build(this: &Self::Type) -> Self {
        Self {
            api_node_url: Some(this.api_node_url.expose_str().to_string()),
            auth_token: Some(this.auth_token.clone()),
        }
    }
}
