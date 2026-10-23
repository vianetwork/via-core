use std::sync::Arc;

use via_btc_client::{client::BitcoinClient, types::NodeAuth};
use zksync_config::configs::{
    via_btc_client::ViaBtcClientConfig, via_secrets::ViaL1Secrets, via_wallets::ViaWallets,
};

use crate::{
    implementations::resources::via_btc_client::BtcClientResource,
    wiring_layer::{WiringError, WiringLayer},
    IntoContext,
};

#[derive(Debug)]
pub struct BtcClientLayer {
    via_btc_client: ViaBtcClientConfig,
    secrets: ViaL1Secrets,
    wallets: ViaWallets,
    bridge_address: Option<String>,
    component: &'static str,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    pub btc_client_resource: BtcClientResource,
}

impl BtcClientLayer {
    pub fn new(
        via_btc_client: ViaBtcClientConfig,
        secrets: ViaL1Secrets,
        wallets: ViaWallets,
        bridge_address: Option<String>,
    ) -> Self {
        Self {
            via_btc_client,
            secrets,
            wallets,
            bridge_address,
            component: "client",
        }
    }

    pub fn with_component(mut self, component: &'static str) -> Self {
        self.component = component;
        self
    }
}

#[async_trait::async_trait]
impl WiringLayer for BtcClientLayer {
    type Input = ();
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "btc_client_layer"
    }

    async fn wire(self, _: Self::Input) -> Result<Self::Output, WiringError> {
        let client = |wallet: Option<String>| {
            let url = match wallet {
                Some(wallet) => self
                    .via_btc_client
                    .rpc_url(self.secrets.rpc_url.expose_str().to_string(), wallet),
                None => self.secrets.rpc_url.expose_str().to_string(),
            };
            BitcoinClient::new_with_component(
                &url,
                NodeAuth::UserPass(self.secrets.rpc_user.clone(), self.secrets.rpc_password.clone()),
                self.via_btc_client.clone(),
                self.component,
            )
            .map(Arc::new)
            .map_err(|err| WiringError::Internal(err.into()))
        };
        let mut btc_client_resource = BtcClientResource::new(client(None)?);
        if let Some(wallet) = self.wallets.btc_sender {
            btc_client_resource =
                btc_client_resource.with_btc_sender(client(Some(wallet.address))?);
        }
        if let Some(wallet) = self.wallets.vote_operator {
            btc_client_resource = btc_client_resource.with_verifier(client(Some(wallet.address))?);
        }
        if let Some(address) = self.bridge_address {
            btc_client_resource = btc_client_resource.with_bridge(client(Some(address))?);
        }

        Ok(Output {
            btc_client_resource,
        })
    }
}
