use std::sync::Arc;

use via_btc_client::client::BitcoinClient;
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
        }
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
        let mut btc_client_resource = BtcClientResource::new();

        if let Some(wallet) = self.wallets.btc_sender {
            let btc_client = BitcoinClient::new(
                &self.via_btc_client.rpc_url(
                    self.secrets.rpc_url.expose_str().to_string(),
                    wallet.address,
                ),
                self.secrets.auth_node(),
                self.via_btc_client.clone(),
            )
            .map_err(|e| WiringError::Internal(e.into()))?;

            btc_client_resource = btc_client_resource.with_btc_sender(Arc::new(btc_client))
        }

        if let Some(wallet) = self.wallets.vote_operator {
            let btc_client = BitcoinClient::new(
                &self.via_btc_client.rpc_url(
                    self.secrets.rpc_url.expose_str().to_string(),
                    wallet.address,
                ),
                self.secrets.auth_node(),
                self.via_btc_client.clone(),
            )
            .map_err(|e| WiringError::Internal(e.into()))?;
            btc_client_resource = btc_client_resource.with_verifier(Arc::new(btc_client))
        }

        if let Some(bridge_address) = self.bridge_address {
            let btc_client = BitcoinClient::new(
                &self.via_btc_client.rpc_url(
                    self.secrets.rpc_url.expose_str().to_string(),
                    bridge_address,
                ),
                self.secrets.auth_node(),
                self.via_btc_client.clone(),
            )
            .map_err(|e| WiringError::Internal(e.into()))?;
            btc_client_resource = btc_client_resource.with_bridge(Arc::new(btc_client))
        }

        Ok(Output {
            btc_client_resource,
        })
    }
}
