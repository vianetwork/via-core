use via_btc_client::client::BitcoinClient;
use zksync_config::configs::{via_btc_client::ViaBtcClientConfig, via_secrets::ViaL1Secrets};

use crate::{
    implementations::resources::via_btc_client::BtcClientResource,
    wiring_layer::{WiringError, WiringLayer},
    IntoContext,
};

#[derive(Debug)]
pub struct BtcClientLayer {
    via_btc_client: ViaBtcClientConfig,
    secrets: ViaL1Secrets,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    pub btc_client_resource: BtcClientResource,
}

impl BtcClientLayer {
    pub fn new(via_btc_client: ViaBtcClientConfig, secrets: ViaL1Secrets) -> Self {
        Self {
            via_btc_client,
            secrets,
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
        let client = BitcoinClient::new(
            self.secrets.rpc_url.expose_str(),
            self.secrets.auth_node(),
            self.via_btc_client,
        )
        .map_err(|e| WiringError::Internal(e.into()))?;
        let btc_client_resource = BtcClientResource::from(client);

        Ok(Output {
            btc_client_resource,
        })
    }
}
