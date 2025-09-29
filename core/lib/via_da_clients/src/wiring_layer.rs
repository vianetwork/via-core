use std::fmt::Debug;

use zksync_config::{
    configs::{via_celestia::DaBackend, via_secrets::ViaDASecrets},
    ViaCelestiaConfig,
};
use zksync_da_client::DataAvailabilityClient;
use zksync_node_framework::{
    implementations::resources::da_client::DAClientResource,
    wiring_layer::{WiringError, WiringLayer},
    IntoContext,
};

use crate::{celestia::client::CelestiaClient, http::client::HttpDaClient};

#[derive(Debug)]
pub struct ViaDaClientWiringLayer {
    config: ViaCelestiaConfig,
    secrets: Option<ViaDASecrets>,
}

impl ViaDaClientWiringLayer {
    pub fn new(config: ViaCelestiaConfig, secrets: Option<ViaDASecrets>) -> Self {
        Self { config, secrets }
    }
}

#[derive(Debug, IntoContext)]
pub struct Output {
    pub client: DAClientResource,
}

#[async_trait::async_trait]
impl WiringLayer for ViaDaClientWiringLayer {
    type Input = ();
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_da_layer"
    }

    async fn wire(self, _input: Self::Input) -> Result<Self::Output, WiringError> {
        let client: Box<dyn DataAvailabilityClient> = match self.config.da_backend {
            DaBackend::Celestia => Box::new(
                CelestiaClient::new(self.secrets.unwrap(), self.config.blob_size_limit).await?,
            ),
            DaBackend::Http => Box::new(HttpDaClient::new(self.config.api_node_url)),
        };
        Ok(Output {
            client: DAClientResource(client),
        })
    }
}
